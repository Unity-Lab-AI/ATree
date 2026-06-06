//! Process Detection — execution flow tracing on the code graph.
//!
//! Detects execution flows (Processes) by:
//! 1. Finding entry points (functions with no internal callers)
//! 2. Tracing forward via CALLS edges (BFS)
//! 3. Grouping and deduplicating similar paths
//! 4. Labeling with heuristic names
//!
//! Ported from GitNexus's process-processor.ts.

use crate::store::GraphStore;
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Serialize, Deserialize};

/// A detected execution flow (process).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Process {
    pub id: String,
    pub label: String,
    pub heuristic_label: String,
    pub process_type: String, // "intra_community" | "cross_community"
    pub step_count: usize,
    pub entry_point_id: i64,
    pub terminal_id: i64,
    pub trace: Vec<i64>, // ordered symbol IDs
}

/// A step within a process trace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessStep {
    pub node_id: i64,
    pub process_id: String,
    pub step: usize, // 1-indexed
}

/// Result of process detection.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcessDetectionResult {
    pub processes: Vec<Process>,
    pub steps: Vec<ProcessStep>,
    pub stats: ProcessStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProcessStats {
    pub total_processes: usize,
    pub cross_community_count: usize,
    pub avg_step_count: f64,
    pub entry_points_found: usize,
}

/// Configuration for process detection.
pub struct ProcessConfig {
    pub max_trace_depth: usize,
    pub max_branching: usize,
    pub max_processes: usize,
    pub min_steps: usize,
}

impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            max_trace_depth: 10,
            max_branching: 4,
            max_processes: 75,
            min_steps: 3,
        }
    }
}

/// Detect execution flows (processes) in the graph store.
pub fn detect_processes(
    store: &GraphStore,
    config: &ProcessConfig,
) -> rusqlite::Result<ProcessDetectionResult> {
    let files = store.get_all_files()?;
    if files.is_empty() {
        return Ok(ProcessDetectionResult::default());
    }

    // Collect all symbols and build adjacency (caller → callees)
    let mut all_symbols: Vec<crate::store::SymbolRecord> = Vec::new();
    let mut symbol_names: FxHashMap<i64, String> = FxHashMap::default();
    for file in &files {
        let mut syms = store.get_symbols_by_file(file.id)?;
        for sym in &syms {
            symbol_names.insert(sym.id, sym.name.clone());
        }
        all_symbols.append(&mut syms);
    }

    if all_symbols.is_empty() {
        return Ok(ProcessDetectionResult::default());
    }

    // Build forward adjacency: caller → [callees]
    let mut forward_adj: FxHashMap<i64, Vec<i64>> = FxHashMap::default();
    let mut reverse_adj: FxHashMap<i64, Vec<i64>> = FxHashMap::default();

    for sym in &all_symbols {
        forward_adj.entry(sym.id).or_default();
        reverse_adj.entry(sym.id).or_default();
    }

    // Load CALLS edges
    for file in &files {
        let syms = store.get_symbols_by_file(file.id)?;
        for sym in &syms {
            let edges = store.get_edges_for_node(sym.id)?;
            for edge in &edges {
                if edge.edge_kind == "CALLS" {
                    forward_adj.entry(edge.src_id).or_default().push(edge.dst_id);
                    reverse_adj.entry(edge.dst_id).or_default().push(edge.src_id);
                }
            }
        }
    }

    // Step 1: Find entry points using multiple heuristics
    let mut entry_points: Vec<i64> = Vec::new();

    // Heuristic 1: Top-level functions — callees but no callers in the indexed set
    for sym in &all_symbols {
        let has_callees = forward_adj.get(&sym.id).map(|v| !v.is_empty()).unwrap_or(false);
        let has_callers = reverse_adj.get(&sym.id).map(|v| !v.is_empty()).unwrap_or(false);
        if has_callees && !has_callers {
            entry_points.push(sym.id);
        }
    }

    // Heuristic 2: Exported functions with callees
    for sym in &all_symbols {
        if sym.is_exported {
            let has_callees = forward_adj.get(&sym.id).map(|v| !v.is_empty()).unwrap_or(false);
            if has_callees && !entry_points.contains(&sym.id) {
                entry_points.push(sym.id);
            }
        }
    }

    // Heuristic 3: API route handlers — functions that handle HTTP routes
    // These are natural entry points since they're called by the framework, not by user code
    let route_rows = store.conn().prepare(
        "SELECT handler_symbol_id FROM routes WHERE handler_symbol_id IS NOT NULL"
    ).and_then(|mut stmt| {
        stmt.query_map([], |row| row.get::<_, i64>(0))
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
    }).unwrap_or_default();
    for handler_id in route_rows {
        if !entry_points.contains(&handler_id) {
            entry_points.push(handler_id);
        }
    }

    // Heuristic 4: Event handlers — functions with common event handler naming patterns
    // (onX, handleX, processX) that have callees but may have callers
    for sym in &all_symbols {
        let name = symbol_names.get(&sym.id).map(|s| s.as_str()).unwrap_or("");
        let is_event_handler = name.starts_with("on") && name.len() > 2
            || name.starts_with("handle") && name.len() > 6
            || name.starts_with("process") && name.len() > 7;
        if is_event_handler {
            let has_callees = forward_adj.get(&sym.id).map(|v| !v.is_empty()).unwrap_or(false);
            if has_callees && !entry_points.contains(&sym.id) {
                entry_points.push(sym.id);
            }
        }
    }

    // Step 2: Trace forward from each entry point via BFS
    let mut processes = Vec::new();
    let mut steps = Vec::new();
    let mut seen_traces: FxHashSet<String> = FxHashSet::default();

    for entry_id in &entry_points {
        if processes.len() >= config.max_processes {
            break;
        }

        let trace = trace_forward(
            *entry_id,
            &forward_adj,
            config.max_trace_depth,
            config.max_branching,
        );

        if trace.len() < config.min_steps {
            continue;
        }

        // Deduplicate by trace signature
        let trace_key = trace.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
        if seen_traces.contains(&trace_key) {
            continue;
        }
        seen_traces.insert(trace_key);

        // Build process label from entry and terminal
        let entry_name = symbol_names.get(entry_id).map(|s| s.as_str()).unwrap_or("unknown");
        let terminal_id = *trace.last().unwrap();
        let terminal_name = symbol_names.get(&terminal_id).map(|s| s.as_str()).unwrap_or("unknown");

        let label = format!("{} → {}", entry_name, terminal_name);
        let heuristic_label = format!("{}Flow", capitalize(entry_name));

        let process_id = format!("proc_{}_{}", entry_name, processes.len());

        for (step_idx, node_id) in trace.iter().enumerate() {
            steps.push(ProcessStep {
                node_id: *node_id,
                process_id: process_id.clone(),
                step: step_idx + 1,
            });
        }

        let step_count = trace.len();
        processes.push(Process {
            id: process_id,
            label,
            heuristic_label,
            process_type: "intra_community".to_string(), // simplified
            step_count,
            entry_point_id: *entry_id,
            terminal_id,
            trace,
        });
    }

    let total_processes = processes.len();
    let avg_step_count = if total_processes > 0 {
        processes.iter().map(|p| p.step_count).sum::<usize>() as f64 / total_processes as f64
    } else {
        0.0
    };

    Ok(ProcessDetectionResult {
        processes,
        steps,
        stats: ProcessStats {
            total_processes,
            cross_community_count: 0, // would need community detection first
            avg_step_count,
            entry_points_found: entry_points.len(),
        },
    })
}

/// Persist detected processes to the graph store as process nodes + STEP_IN_PROCESS edges.
///
/// Process node IDs use the 2_000_000+ range (community uses 1_000_000+).
/// Each process becomes a node; each step becomes a STEP_IN_PROCESS edge
/// from the symbol node to the process node.
pub fn store_processes(
    store: &GraphStore,
    result: &ProcessDetectionResult,
) -> rusqlite::Result<usize> {
    let mut count = 0;
    // Ensure a placeholder file exists for process/community nodes (file_id must reference files(id)).
    let placeholder_file_id: i64 = store.conn().query_row(
        "INSERT OR IGNORE INTO files (path, hash, language, mtime, indexed_at, repo_label)
         VALUES ('__process_placeholder__', 0, 'Unknown', 0, 0, NULL)
         ON CONFLICT(path) DO UPDATE SET path=path
         RETURNING id",
        [], |r| r.get(0),
    ).unwrap_or(1);

    for process in &result.processes {
        let process_node_id = process_node_id(&process.id);
        // Insert the process node itself with explicit ID.
        store.insert_symbol_with_id(&crate::store::SymbolRecord {
            id: process_node_id,
            file_id: placeholder_file_id,
            name: process.heuristic_label.clone(),
            qualified_name: process.label.clone(),
            kind: "Process".to_string(),
            line: 0,
            col: 0,
            is_exported: false,
            scope_id: None,
            owner_symbol_id: None,
        })?;
        // Insert STEP_IN_PROCESS edges for each step.
        for step in &result.steps {
            if step.process_id == process.id {
                if let Ok(Some(file_id)) = store.get_file_id_for_symbol(step.node_id) {
                    store.insert_edge(&crate::store::EdgeRecord {
                        id: 0,
                        src_id: step.node_id,
                        dst_id: process_node_id,
                        edge_kind: "STEP_IN_PROCESS".to_string(),
                        confidence: 1.0,
                        file_id: Some(file_id),
                        line: 0,
                    })?;
                    count += 1;
                }
            }
        }
    }
    Ok(count)
}

/// Convert a process ID string to a numeric node ID for edge storage.
/// proc_main_0 → 2_000_000, proc_parse_1 → 2_000_001, etc.
/// Uses a hash to get a stable numeric ID from the process ID string.
fn process_node_id(process_id: &str) -> i64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    process_id.hash(&mut hasher);
    let hash = hasher.finish();
    2_000_000 + (hash % 1_000_000) as i64
}

/// Trace forward from an entry point via BFS, following CALLS edges.
fn trace_forward(
    entry_id: i64,
    forward_adj: &FxHashMap<i64, Vec<i64>>,
    max_depth: usize,
    max_branching: usize,
) -> Vec<i64> {
    let mut trace = vec![entry_id];
    let mut visited: FxHashSet<i64> = FxHashSet::default();
    visited.insert(entry_id);

    let mut current = entry_id;
    let mut depth = 0;

    while depth < max_depth {
        let callees = forward_adj.get(&current).map(|v| v.as_slice()).unwrap_or(&[]);
        // Pick the best callee: prefer unvisited, then by order
        let next = callees.iter()
            .filter(|id| !visited.contains(id))
            .take(max_branching)
            .copied()
            .next();

        match next {
            Some(next_id) => {
                trace.push(next_id);
                visited.insert(next_id);
                current = next_id;
                depth += 1;
            }
            None => break,
        }
    }

    trace
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_process_detection_finds_flows() {
        let store = GraphStore::open_in_memory().unwrap();
        let file_id = store.upsert_file("src/lib.rs", 1, "rust", 0, None).unwrap();

        // Create a call chain: main → parse → validate → save
        let main_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "main".into(), qualified_name: "main".into(),
            kind: "DefinitionFunction".into(), line: 1, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        let parse_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "parse".into(), qualified_name: "parse".into(),
            kind: "DefinitionFunction".into(), line: 10, col: 0,
            is_exported: false, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        let validate_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "validate".into(), qualified_name: "validate".into(),
            kind: "DefinitionFunction".into(), line: 20, col: 0,
            is_exported: false, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        let save_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "save".into(), qualified_name: "save".into(),
            kind: "DefinitionFunction".into(), line: 30, col: 0,
            is_exported: false, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        // main → parse → validate → save
        for (src, dst) in &[(main_id, parse_id), (parse_id, validate_id), (validate_id, save_id)] {
            store.insert_edge(&crate::store::EdgeRecord {
                id: 0, src_id: *src, dst_id: *dst,
                edge_kind: "CALLS".into(), confidence: 1.0,
                file_id: Some(file_id), line: 0,
            }).unwrap();
        }

        let config = ProcessConfig::default();
        let result = detect_processes(&store, &config).unwrap();

        // Should find at least 1 process starting from main
        assert!(!result.processes.is_empty(), "Should find at least 1 process");

        // The main process should have 4 steps
        let main_proc = result.processes.iter().find(|p| p.entry_point_id == main_id);
        assert!(main_proc.is_some(), "Should find process starting from main");
        let main_proc = main_proc.unwrap();
        assert_eq!(main_proc.step_count, 4);
        assert_eq!(main_proc.trace, vec![main_id, parse_id, validate_id, save_id]);
    }

    #[test]
    fn test_empty_graph() {
        let store = GraphStore::open_in_memory().unwrap();
        let config = ProcessConfig::default();
        let result = detect_processes(&store, &config).unwrap();
        assert_eq!(result.processes.len(), 0);
    }
}
