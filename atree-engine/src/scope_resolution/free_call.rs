//! Free-call fallback pass — emit CALLS edges for free (unqualified) call
//! reference sites whose target is imported or visible via scope bindings.
//!
//! Ported from GitNexus's `passes/free-call-fallback.ts`.

use rustc_hash::{FxHashMap, FxHashSet};
use crate::semantic::Symbol;
use crate::scope_resolution::{
    ScopeResolutionIndexes, ReferenceSite, ReferenceKind,
};
use crate::scope_resolution::walkers::*;
use crate::scope_resolution::workspace_index::WorkspaceResolutionIndex;
use crate::scope_resolution::graph_bridge::{self, GraphEdge, GraphNodeLookup};

/// Pre-built exported-def lookup: name → symbol.
/// Avoids O(files) scan per reference site.
pub type ExportedDefMap<'a> = FxHashMap<&'a str, &'a Symbol>;

/// Build a name→symbol map from all module-scope exported defs.
pub fn build_exported_def_map<'a>(indexes: &'a ScopeResolutionIndexes) -> ExportedDefMap<'a> {
    let mut map = FxHashMap::default();
    for (scope_id, scope) in &indexes.scopes_by_id {
        if !matches!(scope.kind, crate::semantic::ScopeKind::Module) {
            continue;
        }
        if let Some(bind_map) = indexes.bindings.get(scope_id) {
            for (name, refs) in bind_map {
                for b in refs {
                    if let Some(sym) = indexes.symbols_by_id.get(&b.def_node_id) {
                        map.entry(name.as_str()).or_insert(sym);
                    }
                }
            }
        }
        if let Some(aug_map) = indexes.binding_augmentations.get(scope_id) {
            for (name, refs) in aug_map {
                for b in refs {
                    if let Some(sym) = indexes.symbols_by_id.get(&b.def_node_id) {
                        map.entry(name.as_str()).or_insert(sym);
                    }
                }
            }
        }
    }
    map
}

/// Emit CALLS edges for free-call reference sites.
/// Returns the number of edges emitted.
pub fn emit_free_call_fallback(
    edges: &mut Vec<GraphEdge>,
    indexes: &ScopeResolutionIndexes,
    reference_sites: &[ReferenceSite],
    node_lookup: &GraphNodeLookup,
    _workspace_index: &WorkspaceResolutionIndex,
    resolved_sites: &mut rustc_hash::FxHashSet<String>,
) -> usize {
    // Pre-build exported-def map once — O(files) — instead of O(files) per site.
    let exported_map = build_exported_def_map(indexes);

    let mut emitted = 0;
    let mut seen = FxHashSet::default();

    for site in reference_sites {
        if site.kind != ReferenceKind::Call {
            continue;
        }
        if site.explicit_receiver.is_some() {
            continue;
        }

        let site_key = format!("{}:{}:{}", site.in_scope, site.line, site.col);

        // Constructor form: new X(...)
        let target_def = if site.is_constructor {
            find_class_binding_in_scope(indexes, site.in_scope, &site.name)
        } else {
            // Try callable binding in scope chain
            find_callable_binding_in_scope(indexes, site.in_scope, &site.name)
                .or_else(|| exported_map.get(site.name.as_str()).copied())
        };

        let target_def = match target_def {
            Some(d) => d,
            None => continue,
        };

        let ok = graph_bridge::try_emit_edge(
            edges, indexes, node_lookup, site, target_def,
            "local-call", &mut seen, 0.85, false, None,
        );
        if ok {
            emitted += 1;
            resolved_sites.insert(site_key);
        }
    }

    emitted
}
