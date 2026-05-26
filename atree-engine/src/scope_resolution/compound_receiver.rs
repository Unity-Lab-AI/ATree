//! Compound receiver resolution — resolve dotted receiver expressions.

use crate::scope_resolution::ScopeResolutionIndexes;
use crate::scope_resolution::walkers::{find_class_binding_in_scope, find_receiver_type_binding};
use crate::scope_resolution::workspace_index::WorkspaceResolutionIndex;

/// Options for compound receiver resolution.
pub struct CompoundReceiverOpts {
    /// Whether to fall back to field types when method lookup fails.
    pub field_fallback: bool,
    /// Whether to hoist type bindings to module scope.
    pub hoist_type_bindings_to_module: bool,
}

impl Default for CompoundReceiverOpts {
    fn default() -> Self {
        Self {
            field_fallback: true,
            hoist_type_bindings_to_module: false,
        }
    }
}

/// Resolve a compound receiver expression to its class/type symbol.
pub fn resolve_compound_receiver_class<'a>(
    indexes: &'a ScopeResolutionIndexes,
    receiver_name: &str,
    in_scope: u64,
    _workspace_index: &WorkspaceResolutionIndex,
    _opts: &CompoundReceiverOpts,
) -> Option<&'a crate::semantic::Symbol> {
    // Simple case: single identifier
    if !receiver_name.contains('.') && !receiver_name.contains('(') {
        if let Some(type_ref) = find_receiver_type_binding(indexes, in_scope, receiver_name) {
            return find_class_binding_in_scope(indexes, in_scope, &type_ref.raw_name);
        }
        return find_class_binding_in_scope(indexes, in_scope, receiver_name);
    }

    // Dotted case: a.b.c or a.b.c()
    let clean_name = receiver_name.trim_end_matches("()").trim_end_matches("(");
    let parts: Vec<&str> = clean_name.split('.').collect();

    if parts.is_empty() {
        return None;
    }

    let head = parts[0];
    let mut current_type = if let Some(type_ref) = find_receiver_type_binding(indexes, in_scope, head) {
        type_ref.raw_name.clone()
    } else if find_class_binding_in_scope(indexes, in_scope, head).is_some() {
        head.to_string()
    } else {
        return None;
    };

    // Walk the field chain
    for part in &parts[1..] {
        if let Some(class_sym) = find_class_binding_in_scope(indexes, in_scope, &current_type) {
            if let Some(member) = indexes.find_owned_member(class_sym.id, part) {
                current_type = member.name.clone();
            }
        }
    }

    find_class_binding_in_scope(indexes, in_scope, &current_type)
}
