//! Free-call fallback pass — emit CALLS edges for free (unqualified) call
//! reference sites whose target is imported or visible via scope bindings.
//!
//! Ported from GitNexus's `passes/free-call-fallback.ts`.

use rustc_hash::FxHashSet;
use crate::semantic::Symbol;
use crate::scope_resolution::{
    ScopeResolutionIndexes, ReferenceSite, ReferenceKind,
};
use crate::scope_resolution::walkers::*;
use crate::scope_resolution::workspace_index::WorkspaceResolutionIndex;
use crate::scope_resolution::graph_bridge::{self, GraphEdge, GraphNodeLookup};

/// Emit CALLS edges for free-call reference sites.
/// Returns the number of edges emitted.
pub fn emit_free_call_fallback(
    edges: &mut Vec<GraphEdge>,
    indexes: &ScopeResolutionIndexes,
    reference_sites: &[ReferenceSite],
    node_lookup: &GraphNodeLookup,
    _workspace_index: &WorkspaceResolutionIndex,
) -> usize {
    let mut emitted = 0;
    let mut seen = FxHashSet::default();
    let mut handled_sites = FxHashSet::default();

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
                .or_else(|| {
                    // Fallback: try to find an exported def by name
                    find_exported_def_by_name(indexes, &site.name)
                })
        };

        let target_def = match target_def {
            Some(d) => d,
            None => continue,
        };

        let ok = graph_bridge::try_emit_edge(
            edges, indexes, node_lookup, site, target_def,
            "local-call", &mut seen, 0.85, false,
        );
        if ok {
            emitted += 1;
        }
        handled_sites.insert(site_key);
    }

    emitted
}

fn find_exported_def_by_name<'a>(
    indexes: &'a ScopeResolutionIndexes,
    name: &str,
) -> Option<&'a Symbol> {
    for parsed in &indexes.parsed_files {
        if let Some(sym) = find_exported_def(indexes, &parsed.path, name) {
            return Some(sym);
        }
    }
    None
}
