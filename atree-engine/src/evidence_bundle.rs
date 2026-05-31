//! Evidence Bundle — token-bounded, confidence-ranked query results for MCP tools.
//!
//! This is the bridge between the evidence engine and the MCP tool layer.
//! Instead of spawning a CLI subprocess and returning raw stdout, MCP tools
//! can call [`query_evidence`] to get structured, token-bounded evidence
//! bundles directly from the in-memory graph.
//!
//! ## Architecture
//!
//! ```text
//! MCP tool → query_evidence() → KnowledgeGraph (from store)
//!                               → find_evidence_paths() (A* beam)
//!                               → EvidenceBundle (ranked, token-bounded)
//!                               → format_bundle_as_text() → MCP response
//! ```
//!
//! ## Token budgeting
//!
//! Each evidence path step costs ~120 tokens (file path + line + label + relationship).
//! With a default budget of 2000 tokens, that's ~16 steps across all paths.
//! The bundle truncates lowest-confidence paths first.

use crate::evidence_path::{
    EvidencePath, EvidenceVia,
    find_evidence_paths, EvidenceConfig,
};
use crate::graph::KnowledgeGraph;
use crate::search::{search, SearchConfig};
use crate::store::GraphStore;
use rustc_hash::FxHashSet;
use serde::Serialize;
use std::path::Path;

// ── EvidenceBundle ──────────────────────────────────────────────────────────

/// A token-bounded, confidence-ranked collection of evidence paths.
#[derive(Debug, Clone, Serialize)]
pub struct EvidenceBundle {
    /// The original query.
    pub query: String,
    /// Ranked evidence paths (highest confidence first).
    pub paths: Vec<EvidencePath>,
    /// Aggregate confidence (average of path confidences).
    pub confidence: f64,
    /// Total token estimate for this bundle.
    pub estimated_tokens: usize,
    /// Whether the bundle was truncated to fit the token budget.
    pub truncated: bool,
    /// Summary statistics.
    pub stats: BundleStats,
}

#[derive(Debug, Clone, Serialize)]
pub struct BundleStats {
    pub total_paths_found: usize,
    pub paths_returned: usize,
    pub total_steps: usize,
    pub seed_count: usize,
}

impl EvidenceBundle {
    /// Create an empty bundle for a query.
    pub fn empty(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            paths: Vec::new(),
            confidence: 0.0,
            estimated_tokens: 0,
            truncated: false,
            stats: BundleStats {
                total_paths_found: 0,
                paths_returned: 0,
                total_steps: 0,
                seed_count: 0,
            },
        }
    }

    /// Check if the bundle has any results.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Number of paths in the bundle.
    pub fn len(&self) -> usize {
        self.paths.len()
    }
}

// ── Token budgeting ─────────────────────────────────────────────────────────

/// Estimate token count for an evidence path.
///
/// `include_content` — when true, adds ~80 tokens per step for code snippets
/// (context_lines lines around each symbol's definition).
fn estimate_path_tokens(path: &EvidencePath, include_content: bool) -> usize {
    let snippet_cost = if include_content { 80 } else { 0 };
    let step_cost: usize = path.steps.iter().map(|s| {
        // Base: label + file + line + via
        80 + s.file_path.len() + s.label.len() + snippet_cost
    }).sum();
    // Plus per-path overhead (confidence, explanation)
    step_cost + 40
}

/// Truncate paths to fit within token budget, removing lowest-confidence paths first.
fn truncate_to_budget(paths: Vec<EvidencePath>, budget: usize, include_content: bool) -> (Vec<EvidencePath>, bool, usize) {
    if paths.is_empty() {
        return (paths, false, 0);
    }

    let total = paths.len();
    let mut result = Vec::new();
    let mut tokens = 100; // header overhead

    for path in &paths {
        let path_tokens = estimate_path_tokens(path, include_content);
        if tokens + path_tokens > budget && !result.is_empty() {
            // Would exceed budget — skip this path (lower confidence than what we have)
            continue;
        }
        tokens += path_tokens;
        result.push(path.clone());
    }

    let truncated = result.len() < total;
    (result, truncated, tokens)
}

// ── Core query function ─────────────────────────────────────────────────────

/// Query the code intelligence graph and return a token-bounded evidence bundle.
///
/// This is the primary entry point for MCP tools that want structured,
/// confidence-ranked results instead of raw CLI output.
///
/// # Arguments
///
/// * `store` — The SQLite-backed graph store.
/// * `query` — Natural language or symbol name query.
/// * `config` — Evidence traversal configuration (beam width, depth, token budget).
///
/// # Returns
///
/// An [`EvidenceBundle`] with paths sorted by confidence, truncated to fit
/// the token budget.
pub fn query_evidence(
    store: &GraphStore,
    query: &str,
    config: &EvidenceConfig,
) -> Result<EvidenceBundle, String> {
    query_evidence_inner(store, query, config, false)
}

/// Query with content-aware token budgeting.
///
/// When `include_content` is true, the token budget accounts for code snippets
/// (~80 tokens per step), so fewer paths are returned to stay within budget.
pub fn query_evidence_with_content(
    store: &GraphStore,
    query: &str,
    config: &EvidenceConfig,
) -> Result<EvidenceBundle, String> {
    query_evidence_inner(store, query, config, true)
}

/// Query using a pre-built KnowledgeGraph (avoids rebuilding the graph).
///
/// Use this when you already have a `KnowledgeGraph` from a prior call
/// (e.g., in `explain_symbol` or `context` tools that also call
/// `format_symbol_context` which needs the graph).
pub fn query_evidence_with_graph(
    store: &GraphStore,
    graph: &KnowledgeGraph,
    query: &str,
    config: &EvidenceConfig,
) -> Result<EvidenceBundle, String> {
    query_evidence_with_graph_inner(store, graph, query, config, false)
}

/// Query using a pre-built graph, with content-aware token budgeting.
pub fn query_evidence_with_graph_and_content(
    store: &GraphStore,
    graph: &KnowledgeGraph,
    query: &str,
    config: &EvidenceConfig,
) -> Result<EvidenceBundle, String> {
    query_evidence_with_graph_inner(store, graph, query, config, true)
}

fn query_evidence_inner(
    store: &GraphStore,
    query: &str,
    config: &EvidenceConfig,
    include_content: bool,
) -> Result<EvidenceBundle, String> {
    // Step 1: Build in-memory knowledge graph from store.
    let graph = KnowledgeGraph::from_store(store)
        .map_err(|e| format!("Failed to build knowledge graph: {}", e))?;

    // Step 2: Delegate to the shared implementation.
    query_evidence_with_graph_inner(store, &graph, query, config, include_content)
}

fn query_evidence_with_graph_inner(
    store: &GraphStore,
    graph: &KnowledgeGraph,
    query: &str,
    config: &EvidenceConfig,
    include_content: bool,
) -> Result<EvidenceBundle, String> {
    // Step 2: Run evidence path search.
    let all_paths = find_evidence_paths(store, graph, query, config);

    if all_paths.is_empty() {
        return Ok(EvidenceBundle::empty(query));
    }

    let total_found = all_paths.len();

    // Step 2b: Deduplicate paths that end at the same target.
    // Keep the highest-confidence path to each unique endpoint.
    let mut seen_targets: FxHashSet<String> = FxHashSet::default();
    let mut deduped: Vec<EvidencePath> = Vec::new();
    for path in &all_paths {
        if let Some(last_step) = path.steps.last() {
            if seen_targets.insert(last_step.node_id.clone()) {
                deduped.push(path.clone());
            }
        }
    }

    // Step 3: Truncate to token budget (content-aware).
    let (paths, truncated, estimated_tokens) =
        truncate_to_budget(deduped, config.token_budget, include_content);

    // Step 4: Compute aggregate stats (before `paths` is moved).
    let paths_returned = paths.len();
    let total_steps: usize = paths.iter().map(|p| p.steps.len()).sum();
    let confidence = if paths.is_empty() {
        0.0
    } else {
        paths.iter().map(|p| p.confidence).sum::<f64>() / paths.len() as f64
    };

    // Count unique seeds (paths with exactly 1 step = direct match).
    let seed_count = paths.iter().filter(|p| p.steps.len() == 1).count();

    Ok(EvidenceBundle {
        query: query.to_string(),
        paths,
        confidence,
        estimated_tokens,
        truncated,
        stats: BundleStats {
            total_paths_found: total_found,
            paths_returned,
            total_steps,
            seed_count,
        },
    })
}

/// Convenience: query with default config.
pub fn query_evidence_default(store: &GraphStore, query: &str) -> Result<EvidenceBundle, String> {
    let config = EvidenceConfig::default();
    query_evidence(store, query, &config)
}

// ── Formatting for MCP responses ────────────────────────────────────────────

/// Format an evidence bundle as compact text for MCP tool responses.
///
/// Output format:
/// ```text
/// Query: "auth handler"
/// Confidence: 0.82 | Paths: 3 | Steps: 7 | Tokens: ~1200
///
/// 1. [0.95] auth::login (src/auth.rs:12)
///    Direct match for 'login'
///
/// 2. [0.72] auth::login -> validate_token (src/auth.rs:12 -> src/auth.rs:45)
///    Found via 1 hop, cost 1.00
///    CALLS edge, confidence 1.0
///
/// 3. [0.61] auth::login -> validate_token -> check_session
///      (src/auth.rs:12 -> src/auth.rs:45 -> src/session.rs:8)
///    Found via 2 hops, cost 2.50
/// ```
pub fn format_bundle_as_text(bundle: &EvidenceBundle) -> String {
    if bundle.is_empty() {
        return format!("No evidence found for '{}'\n", bundle.query);
    }

    let mut out = String::new();

    // Header.
    out.push_str(&format!(
        "Query: \"{}\" | Confidence: {:.2} | Paths: {}{} | Steps: {} | Tokens: ~{}\n",
        bundle.query,
        bundle.confidence,
        bundle.stats.paths_returned,
        if bundle.truncated {
            format!(" of {}", bundle.stats.total_paths_found)
        } else {
            String::new()
        },
        bundle.stats.total_steps,
        bundle.estimated_tokens,
    ));

    if bundle.truncated {
        out.push_str(&format!(
            "(truncated — {} more paths omitted to fit token budget)\n",
            bundle.stats.total_paths_found - bundle.stats.paths_returned,
        ));
    }
    out.push('\n');

    // Paths.
    for (i, path) in bundle.paths.iter().enumerate() {
        let num = i + 1;

        // Build step summary with direction arrows.
        // Forward steps: A → B, backward steps: A ← B.
        if path.steps.len() > 1 && !path.directions.is_empty() {
            let mut parts = Vec::new();
            parts.push(format!("{} ({}:{})", path.steps[0].label, path.steps[0].file_path, path.steps[0].line));
            for (j, step) in path.steps[1..].iter().enumerate() {
                let arrow = if path.directions.get(j).copied().unwrap_or(true) { "→" } else { "←" };
                parts.push(format!("{} {} ({}:{})", arrow, step.label, step.file_path, step.line));
            }
            let step_summary = parts.join(" ");
            out.push_str(&format!("{}. [{:.2}] {}\n", num, path.confidence, step_summary));
        } else {
            let step_summary: String = path.steps.iter().map(|s| {
                format!("{} ({}:{})", s.label, s.file_path, s.line)
            }).collect::<Vec<_>>().join(" → ");
            out.push_str(&format!("{}. [{:.2}] {}\n", num, path.confidence, step_summary));
        }

        out.push_str(&format!("   {}\n", path.explanation));

        // Show edge details for multi-step paths.
        if path.steps.len() > 1 {
            for (j, step) in path.steps[1..].iter().enumerate() {
                let direction = path.directions.get(j).copied().unwrap_or(true);
                let arrow = if direction { "→" } else { "←" };
                let via_str = match &step.via {
                    EvidenceVia::TextMatch => "text match",
                    EvidenceVia::CallChain => "CALLS",
                    EvidenceVia::Inheritance => "INHERITS",
                    EvidenceVia::ImportChain => "IMPORTS",
                    EvidenceVia::DataFlow => "ACCESSES",
                    EvidenceVia::Containment => "CONTAINS",
                    EvidenceVia::Semantic => "semantic",
                };
                let dir_label = if direction { "forward" } else { "backward" };
                out.push_str(&format!(
                    "   {} {} via {} ({}, conf {:.1})\n",
                    arrow, step.label, via_str, dir_label, step.relevance
                ));
            }
        }
        out.push('\n');
    }

    out
}

/// Extract a code snippet around a symbol's line from its source file.
///
/// Reads `context_lines` lines before and after the symbol's line.
/// Returns None if the file can't be read or the line is out of bounds.
/// `repo_root` is the base path for resolving relative file paths.
pub fn extract_code_snippet(
    repo_root: &Path,
    file_path: &str,
    line: usize,
    context_lines: usize,
) -> Option<String> {
    let full_path = repo_root.join(file_path);
    let content = std::fs::read_to_string(&full_path).ok()?;
    let lines: Vec<&str> = content.lines().collect();

    if line == 0 || line > lines.len() {
        return None;
    }

    // Convert 1-based line to 0-based index.
    let idx = line - 1;
    let start = idx.saturating_sub(context_lines);
    let end = (idx + context_lines + 1).min(lines.len());

    let mut snippet = String::new();
    for (i, src_line) in lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        let marker = if line_num == line { ">" } else { " " };
        snippet.push_str(&format!("{:>6} {} {}\n", line_num, marker, src_line));
    }
    Some(snippet)
}

/// Format an evidence bundle with code snippets for each step.
///
/// When `include_content` is true, reads source files and includes
/// `context_lines` lines of code around each symbol. This consumes
/// more tokens but gives agents the actual code they need.
pub fn format_bundle_with_content(
    bundle: &EvidenceBundle,
    repo_root: &Path,
    context_lines: usize,
) -> String {
    if bundle.is_empty() {
        return format!("No evidence found for '{}'\n", bundle.query);
    }

    let mut out = String::new();

    // Header.
    out.push_str(&format!(
        "Query: \"{}\" | Confidence: {:.2} | Paths: {}{} | Steps: {} | Tokens: ~{}\n",
        bundle.query,
        bundle.confidence,
        bundle.stats.paths_returned,
        if bundle.truncated {
            format!(" of {}", bundle.stats.total_paths_found)
        } else {
            String::new()
        },
        bundle.stats.total_steps,
        bundle.estimated_tokens,
    ));
    if bundle.truncated {
        out.push_str(&format!(
            "(truncated — {} more paths omitted to fit token budget)\n",
            bundle.stats.total_paths_found - bundle.stats.paths_returned,
        ));
    }
    out.push('\n');

    // Paths with code.
    for (i, path) in bundle.paths.iter().enumerate() {
        let num = i + 1;

        // Build step summary with direction arrows.
        let step_summary = if path.steps.len() > 1 && !path.directions.is_empty() {
            let mut parts = Vec::new();
            parts.push(format!("{} ({}:{})", path.steps[0].label, path.steps[0].file_path, path.steps[0].line));
            for (j, step) in path.steps[1..].iter().enumerate() {
                let arrow = if path.directions.get(j).copied().unwrap_or(true) { "→" } else { "←" };
                parts.push(format!("{} {} ({}:{})", arrow, step.label, step.file_path, step.line));
            }
            parts.join(" ")
        } else {
            path.steps.iter().map(|s| {
                format!("{} ({}:{})", s.label, s.file_path, s.line)
            }).collect::<Vec<_>>().join(" → ")
        };

        out.push_str(&format!("{}. [{:.2}] {}\n", num, path.confidence, step_summary));
        out.push_str(&format!("   {}\n", path.explanation));

        // Code snippet for EVERY step (not just the last).
        for step in &path.steps {
            if step.line > 0 {
                if let Some(snippet) = extract_code_snippet(repo_root, &step.file_path, step.line, context_lines) {
                    out.push_str(&format!("   ── {}:{} ──\n", step.file_path, step.line));
                    for line in snippet.lines() {
                        out.push_str(&format!("   {}\n", line));
                    }
                    out.push('\n');
                }
            }
        }

        // Edge details for multi-step paths.
        if path.steps.len() > 1 {
            for (j, step) in path.steps[1..].iter().enumerate() {
                let direction = path.directions.get(j).copied().unwrap_or(true);
                let arrow = if direction { "→" } else { "←" };
                let via_str = match &step.via {
                    EvidenceVia::TextMatch => "text match",
                    EvidenceVia::CallChain => "CALLS",
                    EvidenceVia::Inheritance => "INHERITS",
                    EvidenceVia::ImportChain => "IMPORTS",
                    EvidenceVia::DataFlow => "ACCESSES",
                    EvidenceVia::Containment => "CONTAINS",
                    EvidenceVia::Semantic => "semantic",
                };
                let dir_label = if direction { "forward" } else { "backward" };
                out.push_str(&format!(
                    "   {} {} via {} ({}, conf {:.1})\n",
                    arrow, step.label, via_str, dir_label, step.relevance
                ));
            }
        }
        out.push('\n');
    }

    out
}

/// Trace the call path between two symbols using A* pathfinding.
///
/// This is the in-process replacement for the CLI `trace_call_path` command.
/// Instead of naive BFS through CALLS edges only, it uses A* beam search
/// across all edge types with quality signals.
///
/// # Arguments
///
/// * `store` — The SQLite-backed graph store.
/// * `from_name` — Source symbol name.
/// * `to_name` — Destination symbol name.
/// * `config` — Evidence traversal configuration.
///
/// # Returns
///
/// Formatted text showing the best paths from `from_name` to `to_name`.
pub fn trace_path_evidence(
    store: &GraphStore,
    from_name: &str,
    to_name: &str,
    config: &EvidenceConfig,
) -> Result<String, String> {
    let graph = KnowledgeGraph::from_store(store)
        .map_err(|e| format!("Failed to build knowledge graph: {}", e))?;

    // Resolve symbol names to IDs.
    let search_config = SearchConfig { limit: 5, ..Default::default() };

    let from_hits = search(store, from_name, &search_config)
        .map_err(|e| format!("Search failed for '{}': {}", from_name, e))?;
    let to_hits = search(store, to_name, &search_config)
        .map_err(|e| format!("Search failed for '{}': {}", to_name, e))?;

    if from_hits.is_empty() {
        return Ok(format!("Symbol '{}' not found in index\n", from_name));
    }
    if to_hits.is_empty() {
        return Ok(format!("Symbol '{}' not found in index\n", to_name));
    }

    let from_id = from_hits[0].node_id;
    let to_id = to_hits[0].node_id;

    // Check if from and to resolve to the same node.
    if from_id == to_id {
        let node = graph.get_node(&format!("sym:{}", from_id));
        let _name = node.and_then(|n| n.properties.get("name").cloned()).unwrap_or_else(|| from_name.to_string());
        let file = node.and_then(|n| n.properties.get("file_path").cloned()).unwrap_or_default();
        let line = node.and_then(|n| n.properties.get("line").and_then(|l| l.parse().ok())).unwrap_or(0);
        return Ok(format!("{} → {} (same symbol)\n  {}:{}\n", from_name, to_name, file, line));
    }

    let paths = crate::evidence_path::find_path_between(store, &graph, from_id, to_id, config);

    if paths.is_empty() {
        return Ok(format!(
            "No path found from '{}' to '{}' within {} hops.\n",
            from_name, to_name, config.max_depth
        ));
    }

    // Format the trace path result.
    let mut out = String::new();
    out.push_str(&format!("Path: {} → {}\n", from_name, to_name));
    out.push_str(&format!(
        "Found {} path(s) | Max depth: {} | Beam width: {}\n\n",
        paths.len(),
        config.max_depth,
        config.beam_width,
    ));

    for (i, path) in paths.iter().enumerate() {
        out.push_str(&format!("── Path {} [{:.2}] ──\n", i + 1, path.confidence));

        // Build step chain with direction arrows.
        if !path.steps.is_empty() {
            out.push_str(&format!("  {}", path.steps[0].label));
            for (j, step) in path.steps[1..].iter().enumerate() {
                let arrow = if path.directions.get(j).copied().unwrap_or(true) { "→" } else { "←" };
                let via_str = match &step.via {
                    EvidenceVia::TextMatch => "match",
                    EvidenceVia::CallChain => "CALLS",
                    EvidenceVia::Inheritance => "INHERITS",
                    EvidenceVia::ImportChain => "IMPORTS",
                    EvidenceVia::DataFlow => "ACCESSES",
                    EvidenceVia::Containment => "CONTAINS",
                    EvidenceVia::Semantic => "semantic",
                };
                out.push_str(&format!(" --{}→ {} [{}]", arrow, step.label, via_str));
            }
            out.push('\n');

            // File locations.
            out.push_str("  Locations:\n");
            for step in &path.steps {
                out.push_str(&format!("    {} ({}:{})\n", step.label, step.file_path, step.line));
            }
        }

        out.push_str(&format!("  {}\n", path.explanation));
        out.push('\n');
    }

    Ok(out)
}

/// Impact analysis result for a symbol.
///
/// Combines multi-depth caller/callee analysis (like the CLI `impact` command)
/// with evidence paths showing *why* each caller matters.
pub struct ImpactResult {
    pub symbol_name: String,
    pub symbol_kind: String,
    pub file_path: String,
    pub line: usize,
    pub risk: String,
    pub weighted_score: usize,
    pub direct_callers: Vec<(String, f64, String)>,  // (name, confidence, file)
    pub indirect_callers: Vec<(String, f64, String)>,
    pub direct_callees: Vec<(String, f64, String)>,
    pub indirect_callees: Vec<(String, f64, String)>,
    pub evidence_paths: Vec<EvidencePath>,
}

/// Run impact analysis for a symbol using the store directly.
///
/// This is the in-process replacement for the CLI `impact` command.
/// It computes multi-depth caller/callee analysis with weighted risk scoring,
/// then augments it with evidence paths from the target to its callers.
///
/// # Arguments
///
/// * `store` — The SQLite-backed graph store.
/// * `symbol_name` — The symbol to analyze.
/// * `depth` — Max traversal depth (default: 3).
/// * `evidence_config` — Configuration for evidence path search.
///
/// # Returns
///
/// Formatted text showing the impact analysis.
pub fn impact_evidence(
    store: &GraphStore,
    symbol_name: &str,
    depth: usize,
    evidence_config: &EvidenceConfig,
    direction: Option<&str>,
    kind_filter: Option<&str>,
) -> Result<String, String> {
    let syms = store.get_symbols_by_name(symbol_name)
        .map_err(|e| format!("Store query failed: {}", e))?;

    if syms.is_empty() {
        return Ok(format!("Symbol '{}' not found in index\n", symbol_name));
    }

    let sym = &syms[0];

    // Build the knowledge graph once; use it for file resolution and evidence paths.
    let graph = KnowledgeGraph::from_store(store)
        .map_err(|e| format!("Graph build failed: {}", e))?;

    // Resolve file path for the target symbol.
    let fp = graph.resolve_symbol(&format!("sym:{}", sym.id))
        .map(|(_, _, f, _)| f)
        .unwrap_or("?");

    let dir = direction.unwrap_or("both");
    let show_upstream = dir == "both" || dir == "upstream";
    let show_downstream = dir == "both" || dir == "downstream";

    // Multi-depth caller/callee analysis.
    let d1_callers = if show_upstream { store.get_callers(sym.id, 1).unwrap_or_default() } else { Vec::new() };
    let d2_callers = if show_upstream { store.get_callers(sym.id, 2).unwrap_or_default() } else { Vec::new() };
    let d3_callers = if show_upstream { store.get_callers(sym.id, depth.min(3)).unwrap_or_default() } else { Vec::new() };
    let d1_callees = if show_downstream { store.get_callees(sym.id, 1).unwrap_or_default() } else { Vec::new() };
    let d2_callees = if show_downstream { store.get_callees(sym.id, 2).unwrap_or_default() } else { Vec::new() };

    // Resolve caller/callee file paths using the KnowledgeGraph.
    let resolve_file = |fid: i64| -> String {
        graph.resolve_symbol(&format!("sym:{}", fid))
            .map(|(_, _, f, _)| f.to_string())
            .unwrap_or_else(|| "?".to_string())
    };

    // Apply kind filter: only include symbols whose kind contains the filter (case-insensitive).
    let kind_matches = |name: &str| -> bool {
        match kind_filter {
            Some(kf) => name.to_lowercase().contains(&kf.to_lowercase()),
            None => true,
        }
    };

    let direct_callers: Vec<_> = d1_callers.iter()
        .filter(|(_, name, _, _)| kind_matches(name))
        .map(|(_, name, conf, fid)| (name.clone(), *conf, resolve_file(*fid)))
        .collect();
    let indirect_callers: Vec<_> = d2_callers.iter()
        .filter(|(id, _, _, _)| !d1_callers.iter().any(|(d1id, _, _, _)| d1id == id))
        .filter(|(_, name, _, _)| kind_matches(name))
        .map(|(_, name, conf, fid)| (name.clone(), *conf, resolve_file(*fid)))
        .collect();
    let deep_callers: Vec<_> = d3_callers.iter()
        .filter(|(id, _, _, _)| !d2_callers.iter().any(|(d2id, _, _, _)| d2id == id))
        .filter(|(_, name, _, _)| kind_matches(name))
        .map(|(_, name, conf, fid)| (name.clone(), *conf, resolve_file(*fid)))
        .collect();
    let direct_callees: Vec<_> = d1_callees.iter()
        .filter(|(_, name, _, _)| kind_matches(name))
        .map(|(_, name, conf, fid)| (name.clone(), *conf, resolve_file(*fid)))
        .collect();
    let indirect_callees: Vec<_> = d2_callees.iter()
        .filter(|(id, _, _, _)| !d1_callees.iter().any(|(d1id, _, _, _)| d1id == id))
        .filter(|(_, name, _, _)| kind_matches(name))
        .map(|(_, name, conf, fid)| (name.clone(), *conf, resolve_file(*fid)))
        .collect();

    // Weighted risk score: direct callers matter most.
    let weighted_score = direct_callers.len() * 3 + indirect_callers.len() * 2 + deep_callers.len();
    let risk = match weighted_score {
        0 => "LOW (no consumers)",
        1..=5 => "LOW",
        6..=15 => "MEDIUM",
        16..=40 => "HIGH",
        _ => "CRITICAL",
    };

    // Build evidence paths from target to its direct callers.
    let mut evidence_paths = Vec::new();
    if show_upstream {
        for (caller_id, _, _, _) in d1_callers.iter().take(5) {
            let paths = crate::evidence_path::find_path_between(
                store, &graph, *caller_id, sym.id, evidence_config,
            );
            if let Some(best) = paths.first() {
                evidence_paths.push(best.clone());
            }
        }
    }

    // Format output.
    let mut out = String::new();
    out.push_str(&format!("═══ Impact: {} ═══\n", sym.name));
    out.push_str(&format!("  {}  |  {}:{}\n", sym.kind, fp, sym.line));
    out.push_str(&format!("  Risk: {} (weighted score: {})\n", risk, weighted_score));
    if let Some(kf) = kind_filter {
        out.push_str(&format!("  Kind filter: {}\n", kf));
    }
    out.push_str(&format!("  Direction: {}\n", dir));
    out.push('\n');

    // Upstream (callers).
    if show_upstream {
        out.push_str("── Upstream (callers) ──\n");
        out.push_str(&format!("  Direct callers (depth 1): {}\n", direct_callers.len()));
        for (name, conf, file) in &direct_callers {
            out.push_str(&format!("    ← {} [{:.2}]  {}\n", name, conf, file));
        }
        if !indirect_callers.is_empty() {
            out.push_str(&format!("  Indirect callers (depth 2): {}\n", indirect_callers.len()));
            for (name, conf, file) in &indirect_callers {
                out.push_str(&format!("    ← {} [{:.2}]  {} (indirect)\n", name, conf, file));
            }
        }
        if !deep_callers.is_empty() {
            out.push_str(&format!("  Deep callers (depth 3+): {}\n", deep_callers.len()));
            for (name, conf, file) in &deep_callers {
                out.push_str(&format!("    ← {} [{:.2}]  {} (deep)\n", name, conf, file));
            }
        }
    }

    // Downstream (callees).
    if show_downstream {
        if show_upstream { out.push('\n'); }
        out.push_str("── Downstream (callees) ──\n");
        out.push_str(&format!("  Direct callees: {}\n", direct_callees.len()));
        for (name, conf, file) in &direct_callees {
            out.push_str(&format!("    → {} [{:.2}]  {}\n", name, conf, file));
        }
        if !indirect_callees.is_empty() {
            out.push_str(&format!("  Indirect callees (depth 2): {}\n", indirect_callees.len()));
            for (name, conf, file) in &indirect_callees {
                out.push_str(&format!("    → {} [{:.2}]  {} (indirect)\n", name, conf, file));
            }
        }
    }

    // Evidence paths.
    if !evidence_paths.is_empty() {
        out.push('\n');
        out.push_str("── Evidence Paths (why callers matter) ──\n");
        for (i, path) in evidence_paths.iter().enumerate() {
            out.push_str(&format!("  Path {} [{:.2}]:\n", i + 1, path.confidence));
            if !path.steps.is_empty() {
                out.push_str(&format!("    {}", path.steps[0].label));
                for (j, step) in path.steps[1..].iter().enumerate() {
                    let arrow = if path.directions.get(j).copied().unwrap_or(true) { "→" } else { "←" };
                    out.push_str(&format!(" --{}→ {}", arrow, step.label));
                }
                out.push('\n');
            }
            out.push_str(&format!("    {}\n", path.explanation));
        }
    }

    out.push('\n');
    Ok(out)
}

/// Format a single symbol's context as a compact evidence snippet.
/// Used by the `context` MCP tool.
///
/// Shows all edge types organized by category:
/// - Callers / Callees (CALLS)
/// - Inheritance (INHERITS, EXTENDS, IMPLEMENTS, METHOD_OVERRIDES)
/// - Imports (IMPORTS, USES)
/// - Data flow (ACCESSES)
/// - Structure (CONTAINS, DEFINES, HAS_METHOD, HAS_PROPERTY)
/// - Behavior (STEP_IN_PROCESS, HANDLES_ROUTE, HANDLES_TOOL, ENTRY_POINT_OF)
/// - Community (MEMBER_OF)
///
/// If `kind_filter` is provided, only symbols whose `kind` contains the filter
/// string (case-insensitive) are included.
pub fn format_symbol_context(
    store: &GraphStore,
    graph: &KnowledgeGraph,
    symbol_name: &str,
    kind_filter: Option<&str>,
) -> Result<String, String> {
    let search_config = SearchConfig {
        limit: 10,
        ..Default::default()
    };
    let hits = search(store, symbol_name, &search_config)
        .map_err(|e| format!("Search failed: {}", e))?;

    let hits: Vec<_> = if let Some(kf) = kind_filter {
        let kf_lower = kf.to_lowercase();
        hits.into_iter()
            .filter(|h| h.kind.to_lowercase().contains(&kf_lower))
            .collect()
    } else {
        hits
    };

    if hits.is_empty() {
        return Ok(format!("Symbol '{}' not found in index{}\n",
            symbol_name,
            kind_filter.map(|k| format!(" (kind: {})", k)).unwrap_or_default()));
    }

    let mut out = String::new();
    out.push_str(&format!("Symbol: {}\n", symbol_name));
    if let Some(kf) = kind_filter {
        out.push_str(&format!("Kind filter: {}\n", kf));
    }
    out.push_str(&format!("Matches: {}\n\n", hits.len()));

    // Helper: resolve a sym:<i64> node ID to (name, kind, file, line).
    let resolve = |node_id: &str| -> Option<(&str, &str, &str, &str)> {
        graph.resolve_symbol(node_id)
    };

    for (i, hit) in hits.iter().enumerate() {
        out.push_str(&format!(
            "{}. {} ({})\n   {}:{}\n   Score: {:.2}\n",
            i + 1,
            hit.name,
            hit.kind,
            hit.file_path,
            hit.line,
            hit.score,
        ));

        let node_id = format!("sym:{}", hit.node_id);
        let edges = graph.edges_for_node(&node_id);

        // ── Callers / Callees (CALLS) ─────────────────────────────────
        let callers: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::Calls && e.target_id == node_id)
            .collect();
        let callees: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::Calls && e.source_id == node_id)
            .collect();

        if !callers.is_empty() {
            out.push_str(&format!("   Called by ({}):\n", callers.len()));
            for edge in callers.iter().take(8) {
                if let Some((name, _, _, _)) = resolve(&edge.source_id) {
                    out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.source_id) {
                    out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                }
            }
            if callers.len() > 8 {
                out.push_str(&format!("     ... and {} more\n", callers.len() - 8));
            }
        }

        if !callees.is_empty() {
            out.push_str(&format!("   Calls ({}):\n", callees.len()));
            for edge in callees.iter().take(8) {
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                }
            }
            if callees.len() > 8 {
                out.push_str(&format!("     ... and {} more\n", callees.len() - 8));
            }
        }

        // ── Inheritance ──────────────────────────────────────────────
        let inherits: Vec<_> = edges.iter()
            .filter(|e| matches!(e.rel_type, crate::graph::RelationshipType::Inherits | crate::graph::RelationshipType::Extends | crate::graph::RelationshipType::Implements)
                && e.source_id == node_id)
            .collect();
        let inherited_by: Vec<_> = edges.iter()
            .filter(|e| matches!(e.rel_type, crate::graph::RelationshipType::Inherits | crate::graph::RelationshipType::Extends | crate::graph::RelationshipType::Implements)
                && e.target_id == node_id)
            .collect();
        let overrides: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::MethodOverrides && e.source_id == node_id)
            .collect();
        let implements: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::MethodImplements && e.source_id == node_id)
            .collect();

        if !inherits.is_empty() {
            out.push_str(&format!("   Inherits/Implements ({}):\n", inherits.len()));
            for edge in inherits.iter().take(5) {
                let kind_name = format!("{:?}", edge.rel_type);
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} ({}, conf {:.1})\n", name, kind_name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} ({}, conf {:.1})\n", name, kind_name, edge.confidence));
                }
            }
        }
        if !inherited_by.is_empty() {
            out.push_str(&format!("   Extended/Implemented by ({}):\n", inherited_by.len()));
            for edge in inherited_by.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.source_id) {
                    out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.source_id) {
                    out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }
        if !overrides.is_empty() {
            out.push_str(&format!("   Overrides ({}):\n", overrides.len()));
            for edge in overrides.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }
        if !implements.is_empty() {
            out.push_str(&format!("   Implements interface method ({}):\n", implements.len()));
            for edge in implements.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }

        // ── Imports ──────────────────────────────────────────────────
        let imported_by: Vec<_> = edges.iter()
            .filter(|e| matches!(e.rel_type, crate::graph::RelationshipType::Imports | crate::graph::RelationshipType::Uses) && e.target_id == node_id)
            .collect();
        let imports: Vec<_> = edges.iter()
            .filter(|e| matches!(e.rel_type, crate::graph::RelationshipType::Imports | crate::graph::RelationshipType::Uses) && e.source_id == node_id)
            .collect();

        if !imported_by.is_empty() {
            out.push_str(&format!("   Imported/Used by ({}):\n", imported_by.len()));
            for edge in imported_by.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.source_id) {
                    out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.source_id) {
                    out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }
        if !imports.is_empty() {
            out.push_str(&format!("   Imports/Uses ({}):\n", imports.len()));
            for edge in imports.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }

        // ── Data flow ────────────────────────────────────────────────
        let accessed_by: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::Accesses && e.target_id == node_id)
            .collect();
        let accesses: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::Accesses && e.source_id == node_id)
            .collect();

        if !accessed_by.is_empty() {
            out.push_str(&format!("   Accessed by ({}):\n", accessed_by.len()));
            for edge in accessed_by.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.source_id) {
                    out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.source_id) {
                    out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }
        if !accesses.is_empty() {
            out.push_str(&format!("   Accesses ({}):\n", accesses.len()));
            for edge in accesses.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }

        // ── Structure ────────────────────────────────────────────────
        let contains: Vec<_> = edges.iter()
            .filter(|e| matches!(e.rel_type, crate::graph::RelationshipType::Contains | crate::graph::RelationshipType::Defines) && e.source_id == node_id)
            .collect();
        let defined_by: Vec<_> = edges.iter()
            .filter(|e| matches!(e.rel_type, crate::graph::RelationshipType::Contains | crate::graph::RelationshipType::Defines) && e.target_id == node_id)
            .collect();
        let has_methods: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::HasMethod && e.source_id == node_id)
            .collect();
        let has_props: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::HasProperty && e.source_id == node_id)
            .collect();

        if !contains.is_empty() {
            out.push_str(&format!("   Contains/Defines ({}):\n", contains.len()));
            for edge in contains.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }
        if !defined_by.is_empty() {
            for edge in defined_by.iter().take(1) {
                if let Some((name, _, _, _)) = resolve(&edge.source_id) {
                    out.push_str(&format!("   Defined in: {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.source_id) {
                    out.push_str(&format!("   Defined in: {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }
        if !has_methods.is_empty() {
            out.push_str(&format!("   Methods ({}):\n", has_methods.len()));
            for edge in has_methods.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }
        if !has_props.is_empty() {
            out.push_str(&format!("   Properties ({}):\n", has_props.len()));
            for edge in has_props.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }

        // ── Behavior / Runtime ───────────────────────────────────────
        // Use the direct node_process map for reliable process lookup.
        if let Some(proc_id) = graph.node_process(&node_id) {
            let proc_name = graph.node_display_name(proc_id).unwrap_or("?");
            out.push_str(&format!("   Process: {}\n", proc_name));
        }
        let handles_route: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::HandlesRoute && e.source_id == node_id)
            .collect();
        let handled_by: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::HandlesRoute && e.target_id == node_id)
            .collect();
        let handles_tool: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::HandlesTool && e.source_id == node_id)
            .collect();
        let entry_point: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::EntryPointOf && e.source_id == node_id)
            .collect();
        let fetches: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::Fetches && e.source_id == node_id)
            .collect();

        if !handles_route.is_empty() {
            for edge in handles_route.iter().take(3) {
                let name = graph.node_display_name(&edge.target_id).unwrap_or("?");
                out.push_str(&format!("   Handles route: {} (conf {:.1})\n", name, edge.confidence));
            }
        }
        if !handled_by.is_empty() {
            for edge in handled_by.iter().take(3) {
                let name = graph.node_display_name(&edge.source_id).unwrap_or("?");
                out.push_str(&format!("   Route: {} (conf {:.1})\n", name, edge.confidence));
            }
        }
        if !handles_tool.is_empty() {
            for edge in handles_tool.iter().take(3) {
                let name = graph.node_display_name(&edge.target_id).unwrap_or("?");
                out.push_str(&format!("   Handles tool: {} (conf {:.1})\n", name, edge.confidence));
            }
        }
        if !entry_point.is_empty() {
            out.push_str("   Entry point: yes\n");
        }
        if !fetches.is_empty() {
            out.push_str(&format!("   Fetches ({}):\n", fetches.len()));
            for edge in fetches.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }

        // ── Community ────────────────────────────────────────────────
        if let Some(com_id) = graph.node_community(&node_id) {
            let name = graph.node_display_name(com_id).unwrap_or("?");
            out.push_str(&format!("   Community: {}\n", name));
            // Show other members of the same community by querying the
            // community node's incoming MEMBER_OF edges.
            let com_edges = graph.edges_for_node(com_id);
            let members: Vec<_> = com_edges.iter()
                .filter(|e| e.rel_type == crate::graph::RelationshipType::MemberOf && e.source_id != node_id)
                .collect();
            if !members.is_empty() {
                out.push_str(&format!("   Community members ({}):\n", members.len()));
                for edge in members.iter().take(5) {
                    if let Some((name, _, _, _)) = resolve(&edge.source_id) {
                        out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                    } else if let Some(name) = graph.node_display_name(&edge.source_id) {
                        out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                    }
                }
                if members.len() > 5 {
                    out.push_str(&format!("     ... and {} more\n", members.len() - 5));
                }
            }
        }

        // ── Decorators / Wrappers ────────────────────────────────────
        let decorates: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::Decorates && e.source_id == node_id)
            .collect();
        let decorated_by: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::Decorates && e.target_id == node_id)
            .collect();
        let wraps: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::Wraps && e.source_id == node_id)
            .collect();
        let wrapped_by: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::Wraps && e.target_id == node_id)
            .collect();
        let queries: Vec<_> = edges.iter()
            .filter(|e| e.rel_type == crate::graph::RelationshipType::Queries && e.source_id == node_id)
            .collect();

        if !decorated_by.is_empty() {
            out.push_str(&format!("   Decorated by ({}):\n", decorated_by.len()));
            for edge in decorated_by.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.source_id) {
                    out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.source_id) {
                    out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }
        if !decorates.is_empty() {
            out.push_str(&format!("   Decorates ({}):\n", decorates.len()));
            for edge in decorates.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }
        if !wrapped_by.is_empty() {
            out.push_str(&format!("   Wrapped by ({}):\n", wrapped_by.len()));
            for edge in wrapped_by.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.source_id) {
                    out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.source_id) {
                    out.push_str(&format!("     ← {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }
        if !wraps.is_empty() {
            out.push_str(&format!("   Wraps ({}):\n", wraps.len()));
            for edge in wraps.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }
        if !queries.is_empty() {
            out.push_str(&format!("   Queries ({}):\n", queries.len()));
            for edge in queries.iter().take(5) {
                if let Some((name, _, _, _)) = resolve(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                } else if let Some(name) = graph.node_display_name(&edge.target_id) {
                    out.push_str(&format!("     → {} (conf {:.1})\n", name, edge.confidence));
                }
            }
        }

        out.push('\n');
    }

    Ok(out)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_path_tokens() {
        let path = EvidencePath {
            steps: vec![
                crate::evidence_path::Evidence {
                    node_id: "n1".into(),
                    label: "main".into(),
                    file_path: "src/main.rs".into(),
                    line: 10,
                    relevance: 0.9,
                    via: EvidenceVia::TextMatch,
                },
                crate::evidence_path::Evidence {
                    node_id: "n2".into(),
                    label: "handle_request".into(),
                    file_path: "src/handler.rs".into(),
                    line: 20,
                    relevance: 0.8,
                    via: EvidenceVia::CallChain,
                },
            ],
            confidence: 0.72,
            cost: 1.0,
            explanation: "test".into(),
            directions: vec![],
            quality: crate::evidence_path::PathQuality::default(),
        };
        let tokens = estimate_path_tokens(&path, false);
        assert!(tokens > 0);
        assert!(tokens < 1000); // Should be reasonable.

        // With content, tokens should be higher (80 extra per step).
        let tokens_with = estimate_path_tokens(&path, true);
        assert!(tokens_with > tokens);
        // 2 steps × 80 = 160 more tokens.
        assert_eq!(tokens_with - tokens, 160);
    }

    #[test]
    fn test_truncate_to_budget() {
        let paths = vec![
            EvidencePath {
                steps: vec![crate::evidence_path::Evidence {
                    node_id: "n1".into(), label: "a".into(),
                    file_path: "f.rs".into(), line: 1,
                    relevance: 0.9, via: EvidenceVia::TextMatch,
                }],
                confidence: 0.9, cost: 0.0, explanation: "high".into(),
                directions: vec![],
                quality: crate::evidence_path::PathQuality::default(),
            },
            EvidencePath {
                steps: vec![crate::evidence_path::Evidence {
                    node_id: "n2".into(), label: "b".into(),
                    file_path: "f.rs".into(), line: 2,
                    relevance: 0.5, via: EvidenceVia::TextMatch,
                }],
                confidence: 0.5, cost: 0.0, explanation: "low".into(),
                directions: vec![],
                quality: crate::evidence_path::PathQuality::default(),
            },
        ];

        // Large budget — nothing truncated.
        let (result, truncated, _) = truncate_to_budget(paths.clone(), 10_000, false);
        assert_eq!(result.len(), 2);
        assert!(!truncated);

        // Tiny budget — should keep only the first (highest confidence) path.
        let (result, _truncated, _) = truncate_to_budget(paths.clone(), 200, false);
        assert!(result.len() <= 2);
        // The first path should always be kept.
        assert_eq!(result[0].confidence, 0.9);

        // With content flag, same budget fits fewer paths (higher per-path cost).
        let (result_content, _, _) = truncate_to_budget(paths, 200, true);
        // Content adds 80 tokens per step, so with a tiny budget we might get fewer.
        assert!(result_content.len() <= 2);
    }

    #[test]
    fn test_format_bundle_empty() {
        let bundle = EvidenceBundle::empty("test_query");
        let text = format_bundle_as_text(&bundle);
        assert!(text.contains("No evidence found"));
    }

    #[test]
    fn test_format_bundle_with_paths() {
        let bundle = EvidenceBundle {
            query: "auth".into(),
            paths: vec![
                EvidencePath {
                    steps: vec![crate::evidence_path::Evidence {
                        node_id: "n1".into(),
                        label: "login".into(),
                        file_path: "src/auth.rs".into(),
                        line: 12,
                        relevance: 0.95,
                        via: EvidenceVia::TextMatch,
                    }],
                    confidence: 0.95,
                    cost: 0.0,
                    explanation: "Direct match for 'login'".into(),
                    directions: vec![],
                    quality: crate::evidence_path::PathQuality::default(),
                },
                EvidencePath {
                    steps: vec![
                        crate::evidence_path::Evidence {
                            node_id: "n1".into(),
                            label: "login".into(),
                            file_path: "src/auth.rs".into(),
                            line: 12,
                            relevance: 0.95,
                            via: EvidenceVia::TextMatch,
                        },
                        crate::evidence_path::Evidence {
                            node_id: "n2".into(),
                            label: "validate_token".into(),
                            file_path: "src/auth.rs".into(),
                            line: 45,
                            relevance: 0.8,
                            via: EvidenceVia::CallChain,
                        },
                    ],
                    confidence: 0.72,
                    cost: 1.0,
                    explanation: "Found via 1 hop, cost 1.00".into(),
                    directions: vec![true],
                    quality: crate::evidence_path::PathQuality::default(),
                },
            ],
            confidence: 0.835,
            estimated_tokens: 500,
            truncated: false,
            stats: BundleStats {
                total_paths_found: 2,
                paths_returned: 2,
                total_steps: 3,
                seed_count: 1,
            },
        };

        let text = format_bundle_as_text(&bundle);
        assert!(text.contains("Query: \"auth\""));
        assert!(text.contains("login"));
        assert!(text.contains("validate_token"));
        assert!(text.contains("CALLS"));
        assert!(text.contains("src/auth.rs"));
    }

    #[test]
    fn test_evidence_bundle_stats() {
        let bundle = EvidenceBundle::empty("test");
        assert!(bundle.is_empty());
        assert_eq!(bundle.len(), 0);
        assert_eq!(bundle.confidence, 0.0);
    }

    #[test]
    fn test_format_symbol_context_with_graph() {
        use crate::graph::{KnowledgeGraph, GraphNode};
        use rustc_hash::FxHashMap;

        let mut graph = KnowledgeGraph::new();

        // Create a symbol with callers, callees, and community
        let mut props = FxHashMap::default();
        props.insert("name".to_string(), "login".to_string());
        props.insert("kind".to_string(), "Function".to_string());
        props.insert("file_path".to_string(), "src/auth.rs".to_string());
        props.insert("line".to_string(), "10".to_string());
        graph.add_node(GraphNode { id: "sym:1".to_string(), label: "Function".to_string(), properties: props });

        let mut props2 = FxHashMap::default();
        props2.insert("name".to_string(), "handle_request".to_string());
        props2.insert("kind".to_string(), "Function".to_string());
        props2.insert("file_path".to_string(), "src/handler.rs".to_string());
        props2.insert("line".to_string(), "5".to_string());
        graph.add_node(GraphNode { id: "sym:2".to_string(), label: "Function".to_string(), properties: props2 });

        let mut props3 = FxHashMap::default();
        props3.insert("name".to_string(), "validate_token".to_string());
        props3.insert("kind".to_string(), "Function".to_string());
        props3.insert("file_path".to_string(), "src/auth.rs".to_string());
        props3.insert("line".to_string(), "20".to_string());
        graph.add_node(GraphNode { id: "sym:3".to_string(), label: "Function".to_string(), properties: props3 });

        // Add edges: handle_request → login → validate_token
        graph.add_edge(crate::graph::GraphEdge {
            id: "e1".into(), source_id: "sym:2".into(), target_id: "sym:1".into(),
            rel_type: crate::graph::RelationshipType::Calls, confidence: 1.0,
            reason: "CALLS".into(), step: None,
        });
        graph.add_edge(crate::graph::GraphEdge {
            id: "e2".into(), source_id: "sym:1".into(), target_id: "sym:3".into(),
            rel_type: crate::graph::RelationshipType::Calls, confidence: 0.9,
            reason: "CALLS".into(), step: None,
        });

        // Set community
        graph.set_node_community("sym:1".to_string(), "com:auth".to_string());
        graph.set_node_community("sym:2".to_string(), "com:auth".to_string());

        // Create a minimal store for search (we just need the connection)
        let store = crate::store::GraphStore::open_in_memory().unwrap();
        crate::search::init_search_index(&store).unwrap();

        // Insert the login symbol into the store so search can find it
        let file_id = store.upsert_file("src/auth.rs", 1, "rust", 0, None).unwrap();
        store.insert_symbol(&crate::store::SymbolRecord {
            id: 1, file_id, name: "login".into(), qualified_name: "auth::login".into(),
            kind: "Function".into(), line: 10, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();
        store.insert_symbol(&crate::store::SymbolRecord {
            id: 2, file_id, name: "handle_request".into(), qualified_name: "handle_request".into(),
            kind: "Function".into(), line: 5, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();
        store.insert_symbol(&crate::store::SymbolRecord {
            id: 3, file_id, name: "validate_token".into(), qualified_name: "auth::validate_token".into(),
            kind: "Function".into(), line: 20, col: 0,
            is_exported: false, scope_id: None, owner_symbol_id: None,
        }).unwrap();
        crate::search::index_symbols(&store).unwrap();

        let text = format_symbol_context(&store, &graph, "login", None).unwrap();
        assert!(text.contains("login"), "Should show the symbol name");
        assert!(text.contains("handle_request"), "Should show the caller");
        assert!(text.contains("validate_token"), "Should show the callee");
        assert!(text.contains("Called by"), "Should show 'Called by' section");
        assert!(text.contains("Calls"), "Should show 'Calls' section");
    }
}
