//! Receiver-bound CALLS/ACCESSES emit pass — generic 7-case dispatcher.
//!
//! Ported from GitNexus's `passes/receiver-bound-calls.ts`.
//! Case order is load-bearing (Contract Invariant I4).

use rustc_hash::FxHashSet;
use crate::scope_resolution::{
    ScopeResolutionIndexes, ReferenceSite, ReferenceKind,
};
use crate::scope_resolution::walkers::*;
use crate::scope_resolution::compound_receiver::{self, CompoundReceiverOpts};
use crate::scope_resolution::workspace_index::WorkspaceResolutionIndex;
use crate::scope_resolution::graph_bridge::{self, GraphEdge, GraphNodeLookup};

/// Provider hooks needed by the receiver-bound pass.
pub struct ReceiverBoundProvider<'a> {
    /// Recognize super/base receiver text.
    pub is_super_receiver: &'a dyn Fn(&str) -> bool,
    /// Field fallback on method lookup.
    pub field_fallback: bool,
    /// Collapse member calls by caller/target.
    pub collapse_member_calls: bool,
    /// Hoist type bindings to module.
    pub hoist_type_bindings_to_module: bool,
}

/// Emit receiver-bound CALLS/ACCESSES edges.
/// Returns the number of edges emitted.
pub fn emit_receiver_bound_calls(
    edges: &mut Vec<GraphEdge>,
    indexes: &ScopeResolutionIndexes,
    reference_sites: &[ReferenceSite],
    node_lookup: &GraphNodeLookup,
    provider: &ReceiverBoundProvider,
    workspace_index: &WorkspaceResolutionIndex,
) -> usize {
    let mut emitted = 0;
    let mut seen = FxHashSet::default();

    let compound_opts = CompoundReceiverOpts {
        field_fallback: provider.field_fallback,
        hoist_type_bindings_to_module: provider.hoist_type_bindings_to_module,
    };

    for site in reference_sites {
        if site.kind != ReferenceKind::Call
            && site.kind != ReferenceKind::Read
            && site.kind != ReferenceKind::Write
        {
            continue;
        }

        let receiver_name = match &site.explicit_receiver {
            Some(r) => r.as_str(),
            None => continue,
        };

        let _site_key = format!("{}:{}:{}", site.in_scope, site.line, site.col);

        // ── super branch ─────────────────────────────────────────
        if (provider.is_super_receiver)(receiver_name) {
            if let Some(enclosing_class) = find_enclosing_class_def(indexes, site.in_scope) {
                if let Some(mro) = indexes.method_dispatch.get(&enclosing_class.id) {
                    for ancestor_id in mro {
                        if let Some(member) = indexes.find_owned_member(*ancestor_id, &site.name) {
                            let ok = graph_bridge::try_emit_edge(
                                edges, indexes, node_lookup, site, member,
                                "global", &mut seen, 0.85, provider.collapse_member_calls,
                            );
                            if ok { emitted += 1; }
                            break;
                        }
                    }
                }
            }
            continue;
        }

        // ── Case 0: compound receiver ────────────────────────────
        if receiver_name.contains('.') || receiver_name.contains('(') {
            if let Some(current_class) = compound_receiver::resolve_compound_receiver_class(
                indexes, receiver_name, site.in_scope, workspace_index, &compound_opts,
            ) {
                let mut chain = vec![current_class.id];
                if let Some(mro) = indexes.method_dispatch.get(&current_class.id) {
                    chain.extend(mro);
                }
                for owner_id in &chain {
                    if let Some(member) = indexes.find_owned_member(*owner_id, &site.name) {
                        let ok = graph_bridge::try_emit_edge(
                            edges, indexes, node_lookup, site, member,
                            "global", &mut seen, 0.85, provider.collapse_member_calls,
                        );
                        if ok { emitted += 1; }
                        break;
                    }
                }
                continue;
            }
        }

        // ── Case 2: class-name receiver ──────────────────────────
        if let Some(class_def) = find_class_binding_in_scope(indexes, site.in_scope, receiver_name) {
            let mut chain = vec![class_def.id];
            if let Some(mro) = indexes.method_dispatch.get(&class_def.id) {
                chain.extend(mro);
            }
            for owner_id in &chain {
                if let Some(member) = indexes.find_owned_member(*owner_id, &site.name) {
                    let reason = match site.kind {
                        ReferenceKind::Write | ReferenceKind::Read => {
                            if site.kind == ReferenceKind::Write { "write" } else { "read" }
                        }
                        _ => "global",
                    };
                    let conf = match site.kind {
                        ReferenceKind::Write | ReferenceKind::Read => 1.0,
                        _ => 0.85,
                    };
                    let ok = graph_bridge::try_emit_edge(
                        edges, indexes, node_lookup, site, member,
                        reason, &mut seen, conf, provider.collapse_member_calls,
                    );
                    if ok { emitted += 1; }
                    break;
                }
            }
            continue;
        }

        // ── Case 3: dotted typeBinding ───────────────────────────
        if let Some(type_ref) = find_receiver_type_binding(indexes, site.in_scope, receiver_name) {
            if type_ref.raw_name.contains('.') {
                let parts: Vec<&str> = type_ref.raw_name.splitn(2, '.').collect();
                if parts.len() == 2 {
                    if let Some(class_def) = find_class_binding_in_scope(indexes, site.in_scope, parts[1]) {
                        if let Some(member) = indexes.find_owned_member(class_def.id, &site.name) {
                            let ok = graph_bridge::try_emit_edge(
                                edges, indexes, node_lookup, site, member,
                                "global", &mut seen, 0.85, provider.collapse_member_calls,
                            );
                            if ok { emitted += 1; }
                            continue;
                        }
                    }
                }
            }

            // ── Case 4: simple typeBinding ────────────────────────
            if !type_ref.raw_name.contains('.') {
                if let Some(owner_def) = find_class_binding_in_scope(indexes, site.in_scope, &type_ref.raw_name) {
                    let mut chain = vec![owner_def.id];
                    if let Some(mro) = indexes.method_dispatch.get(&owner_def.id) {
                        chain.extend(mro);
                    }
                    for owner_id in &chain {
                        if let Some(member) = indexes.find_owned_member(*owner_id, &site.name) {
                            let reason = match site.kind {
                                ReferenceKind::Write | ReferenceKind::Read => {
                                    if site.kind == ReferenceKind::Write { "write" } else { "read" }
                                }
                                _ => "global",
                            };
                            let conf = match site.kind {
                                ReferenceKind::Write | ReferenceKind::Read => 1.0,
                                _ => 0.85,
                            };
                            let ok = graph_bridge::try_emit_edge(
                                edges, indexes, node_lookup, site, member,
                                reason, &mut seen, conf, provider.collapse_member_calls,
                            );
                            if ok { emitted += 1; }
                            break;
                        }
                    }
                    continue;
                }
            }
        }
    }

    emitted
}
