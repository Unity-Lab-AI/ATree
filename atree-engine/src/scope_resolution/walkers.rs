//! Scope-chain lookup primitives shared across the scope-resolution pipeline.

use rustc_hash::FxHashSet;
use crate::semantic::{ScopeKind, Symbol};
use crate::scope_resolution::{
    BindingOrigin, ScopeResolutionIndexes, is_class_like_scope,
};

const EMPTY_BINDINGS: &[crate::scope_resolution::BindingRef] = &[];

/// Look up binding refs at `scope_id` for `name`, consulting both
/// finalized and augmented channels (I8 dual-source).
pub fn lookup_bindings_at<'a>(
    indexes: &'a ScopeResolutionIndexes,
    scope_id: u64,
    name: &str,
) -> &'a [crate::scope_resolution::BindingRef] {
    if let Some(aug_map) = indexes.binding_augmentations.get(&scope_id) {
        if let Some(aug_refs) = aug_map.get(name) {
            if !aug_refs.is_empty() {
                return aug_refs;
            }
        }
    }
    if let Some(bind_map) = indexes.bindings.get(&scope_id) {
        if let Some(refs) = bind_map.get(name) {
            if !refs.is_empty() {
                return refs;
            }
        }
    }
    EMPTY_BINDINGS
}

/// Walk scope chain looking for a type binding named `receiver_name`.
pub fn find_receiver_type_binding<'a>(
    indexes: &'a ScopeResolutionIndexes,
    start_scope: u64,
    receiver_name: &str,
) -> Option<&'a crate::scope_resolution::TypeRef> {
    let mut current_id = start_scope;
    let mut visited = FxHashSet::default();
    loop {
        if !visited.insert(current_id) {
            return None;
        }
        if let Some(tb) = indexes.type_bindings.get(&current_id) {
            if let Some(tr) = tb.get(receiver_name) {
                return Some(tr);
            }
        }
        match indexes.scopes_by_id.get(&current_id) {
            Some(scope) => {
                if let Some(parent) = scope.parent_id {
                    current_id = parent;
                } else {
                    return None;
                }
            }
            None => return None,
        }
    }
}

/// Look up a class-like binding by name in the scope chain.
pub fn find_class_binding_in_scope<'a>(
    indexes: &'a ScopeResolutionIndexes,
    start_scope: u64,
    name: &str,
) -> Option<&'a Symbol> {
    let mut current_id: u64 = start_scope;
    let mut visited = FxHashSet::default();
    loop {
        if !visited.insert(current_id) {
            return None;
        }
        if let Some(scope) = indexes.scopes_by_id.get(&current_id) {
            // Check local bindings
            if let Some(local_map) = indexes.bindings.get(&current_id) {
                if let Some(refs) = local_map.get(name) {
                    for b in refs {
                        if is_class_like(indexes, b.def_node_id) {
                            return indexes.symbols_by_id.get(&b.def_node_id);
                        }
                    }
                }
            }
            // Check augmented
            if let Some(aug_map) = indexes.binding_augmentations.get(&current_id) {
                if let Some(refs) = aug_map.get(name) {
                    for b in refs {
                        if is_class_like(indexes, b.def_node_id) {
                            return indexes.symbols_by_id.get(&b.def_node_id);
                        }
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

/// Look up a callable (Function/Method/Constructor) by name in the scope chain.
pub fn find_callable_binding_in_scope<'a>(
    indexes: &'a ScopeResolutionIndexes,
    start_scope: u64,
    name: &str,
) -> Option<&'a Symbol> {
    let mut current_id: u64 = start_scope;
    let mut visited = FxHashSet::default();
    loop {
        if !visited.insert(current_id) {
            return None;
        }
        if let Some(scope) = indexes.scopes_by_id.get(&current_id) {
            // Check local
            if let Some(local_map) = indexes.bindings.get(&current_id) {
                if let Some(refs) = local_map.get(name) {
                    for b in refs {
                        if let Some(sym) = indexes.symbols_by_id.get(&b.def_node_id) {
                            if is_callable(sym) {
                                return Some(sym);
                            }
                        }
                    }
                }
            }
            // Check augmented
            if let Some(aug_map) = indexes.binding_augmentations.get(&current_id) {
                if let Some(refs) = aug_map.get(name) {
                    for b in refs {
                        if let Some(sym) = indexes.symbols_by_id.get(&b.def_node_id) {
                            if is_callable(sym) {
                                return Some(sym);
                            }
                        }
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

/// Walk scope chain upward looking for the innermost enclosing Class scope.
pub fn find_enclosing_class_def(
    indexes: &ScopeResolutionIndexes,
    start_scope: u64,
) -> Option<&Symbol> {
    let mut current_id: u64 = start_scope;
    let mut visited = FxHashSet::default();
    loop {
        if !visited.insert(current_id) {
            return None;
        }
        if let Some(scope) = indexes.scopes_by_id.get(&current_id) {
            if is_class_like_scope(scope.kind) {
                if let Some(owner_id) = scope.owner_symbol_id {
                    if let Some(sym) = indexes.symbols_by_id.get(&owner_id) {
                        return Some(sym);
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

/// Find a member (method/field) owned by a class symbol, walking the MRO.
pub fn find_owned_member_with_mro<'a>(
    indexes: &'a ScopeResolutionIndexes,
    class_def_id: u64,
    member_name: &str,
) -> Option<&'a Symbol> {
    if let Some(member) = indexes.find_owned_member(class_def_id, member_name) {
        return Some(member);
    }
    if let Some(mro) = indexes.method_dispatch.get(&class_def_id) {
        for ancestor_id in mro {
            if let Some(member) = indexes.find_owned_member(*ancestor_id, member_name) {
                return Some(member);
            }
        }
    }
    None
}

/// Find a file-level exported def (top-of-module class/function) by name.
pub fn find_exported_def<'a>(
    indexes: &'a ScopeResolutionIndexes,
    target_file: &str,
    name: &str,
) -> Option<&'a Symbol> {
    if let Some(scope_ids) = indexes.scopes_by_file.get(target_file) {
        for sid in scope_ids {
            if let Some(scope) = indexes.scopes_by_id.get(sid) {
                if scope.kind == ScopeKind::Module {
                    if let Some(local_map) = indexes.bindings.get(sid) {
                        if let Some(refs) = local_map.get(name) {
                            for b in refs {
                                if b.origin == BindingOrigin::Local {
                                    if let Some(sym) = indexes.symbols_by_id.get(&b.def_node_id) {
                                        return Some(sym);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

// =====================================================================
// Helper functions
// =====================================================================

fn is_class_like(indexes: &ScopeResolutionIndexes, node_id: u64) -> bool {
    indexes.symbols_by_id.get(&node_id).is_some_and(|sym| {
        matches!(sym.kind,
            crate::lang::CaptureTag::DefinitionClass |
            crate::lang::CaptureTag::DefinitionInterface |
            crate::lang::CaptureTag::DefinitionStruct |
            crate::lang::CaptureTag::DefinitionEnum |
            crate::lang::CaptureTag::DefinitionTrait |
            crate::lang::CaptureTag::DefinitionRecord)
    })
}

fn is_callable(sym: &Symbol) -> bool {
    matches!(sym.kind,
        crate::lang::CaptureTag::DefinitionFunction |
        crate::lang::CaptureTag::DefinitionMethod |
        crate::lang::CaptureTag::DefinitionConstructor)
}
