//! Graph bridge — translate scope-resolution references into graph edges.

use rustc_hash::FxHashSet;
use crate::semantic::Symbol;
use crate::scope_resolution::{ScopeResolutionIndexes, ReferenceKind};

/// A resolved graph edge to be emitted.
#[derive(Debug, Clone)]
pub struct GraphEdge {
    pub source_id: u64,
    pub target_id: u64,
    pub edge_type: String,
    pub confidence: f64,
    pub reason: String,
}

/// Graph node lookup — maps symbol ID → graph node ID.
/// In ATree, the symbol ID IS the graph node ID, so this is an identity mapping.
pub struct GraphNodeLookup;

impl Default for GraphNodeLookup {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphNodeLookup {
    pub fn new() -> Self {
        Self
    }

    /// Look up a symbol's graph node ID. In ATree, symbol ID = node ID.
    pub fn get(&self, _file_path: &str, sym: &Symbol) -> Option<u64> {
        Some(sym.id)
    }

    /// Look up by simple key.
    pub fn get_simple(&self, key: &str) -> Option<u64> {
        key.parse::<u64>().ok()
    }
}

/// Resolve a symbol definition to its graph node ID.
pub fn resolve_def_graph_id(
    file_path: &str,
    def: &Symbol,
    node_lookup: &GraphNodeLookup,
) -> Option<u64> {
    node_lookup.get(file_path, def)
}

/// Resolve a reference site's scope to its caller graph node ID.
pub fn resolve_caller_graph_id(
    start_scope: u64,
    indexes: &ScopeResolutionIndexes,
    node_lookup: &GraphNodeLookup,
) -> Option<u64> {
    let mut current_id: u64 = start_scope;
    let mut visited = FxHashSet::default();

    loop {
        if !visited.insert(current_id) {
            return None;
        }
        if let Some(scope) = indexes.scopes_by_id.get(&current_id) {
            // Look for function/method/class owned defs in this scope
            for sym in indexes.symbols_by_id.values() {
                if sym.scope_id == Some(current_id) {
                    if let Some(id) = node_lookup.get("", sym) {
                        return Some(id);
                    }
                }
            }
            if let Some(parent) = scope.parent_id {
                current_id = parent;
            } else {
                return None;
            }
        } else {
            return None;
        }
    }
}

/// Map a reference kind to a graph edge type.
pub fn map_reference_kind_to_edge_type(kind: &ReferenceKind) -> Option<&'static str> {
    match kind {
        ReferenceKind::Call => Some("CALLS"),
        ReferenceKind::Read | ReferenceKind::Write => Some("ACCESSES"),
        ReferenceKind::Inherits => Some("EXTENDS"),
        ReferenceKind::TypeReference => Some("USES"),
    }
}

/// Try to emit a graph edge. Returns true if the edge was emitted (not deduped).
/// When `fine_confidence` is provided, it takes precedence over `confidence`.
pub fn try_emit_edge(
    edges: &mut Vec<GraphEdge>,
    indexes: &ScopeResolutionIndexes,
    node_lookup: &GraphNodeLookup,
    site: &crate::scope_resolution::ReferenceSite,
    target_def: &Symbol,
    reason: &str,
    seen: &mut FxHashSet<String>,
    confidence: f64,
    collapse_by_caller_target: bool,
    fine_confidence: Option<f64>,
) -> bool {
    let caller_id = resolve_caller_graph_id(site.in_scope, indexes, node_lookup);
    let target_id = resolve_def_graph_id("", target_def, node_lookup);

    let (caller_id, target_id) = match (caller_id, target_id) {
        (Some(c), Some(t)) => (c, t),
        _ => return false,
    };

    let edge_type = match map_reference_kind_to_edge_type(&site.kind) {
        Some(et) => et.to_string(),
        None => return false,
    };

    let dedup_key = if collapse_by_caller_target && edge_type == "CALLS" {
        format!("{}:{}->{}", edge_type, caller_id, target_id)
    } else {
        format!("{}:{}->{}:{}:{}", edge_type, caller_id, target_id, site.line, site.col)
    };

    if seen.contains(&dedup_key) {
        return false;
    }
    seen.insert(dedup_key.clone());

    // Use fine-grained evidence-based confidence if available, else the
    // legacy coarse tier-based confidence.
    let edge_confidence = fine_confidence.unwrap_or(confidence);

    edges.push(GraphEdge {
        source_id: caller_id,
        target_id,
        edge_type,
        confidence: edge_confidence,
        reason: reason.to_string(),
    });

    true
}

/// Emit IMPORTS edges from parsed file imports.
pub fn emit_import_edges(
    edges: &mut Vec<GraphEdge>,
    indexes: &ScopeResolutionIndexes,
) -> usize {
    let mut count = 0;

    for parsed in &indexes.parsed_files {
        for imp in &indexes.imports {
            if imp.file_id == parsed.id {
                if let Some(target_file_id) = imp.resolved_file_id {
                    edges.push(GraphEdge {
                        source_id: parsed.id,
                        target_id: target_file_id,
                        edge_type: "IMPORTS".to_string(),
                        confidence: imp.confidence.score(),
                        reason: "import-resolved".to_string(),
                    });
                    count += 1;
                }
            }
        }
    }

    count
}
