//! Ownership reconciliation — reconcile scope-resolution's ownership view
//! into the symbol table.
//!
//! Ported from GitNexus's `scope-resolution/pipeline/reconcile-ownership.ts`.
//! This is a transitional shim (Contract Invariant I9) that corrects
//! `ownerId` on defs where the legacy parse-time extractor didn't resolve it.

use crate::semantic::ParsedFile;
use crate::semantic::ScopeKind;

/// Reconcile ownership for all parsed files.
/// Walks class scopes and marks owned methods/fields with the correct owner_id.
pub fn reconcile_ownership(parsed_files: &mut [ParsedFile]) {
    for parsed in parsed_files {
        let scopes_by_id: rustc_hash::FxHashMap<u64, &crate::semantic::Scope> =
            parsed.scopes.iter().map(|s| (s.id, s)).collect();

        for scope in &parsed.scopes {
            // Methods: function scope whose parent is a Class scope
            if let Some(parent_id) = scope.parent_id {
                if let Some(parent_scope) = scopes_by_id.get(&parent_id) {
                    if matches!(parent_scope.kind,
                        ScopeKind::Class | ScopeKind::Struct | ScopeKind::Trait |
                        ScopeKind::Impl | ScopeKind::Interface | ScopeKind::Enum) {
                        if let Some(class_def_id) = parent_scope.owner_symbol_id {
                            for sym in &mut parsed.symbols {
                                if sym.scope_id == Some(scope.id) {
                                    sym.owner_id = Some(class_def_id);
                                }
                            }
                        }
                    }
                }
            }

            // Class-body fields: defs directly in a Class/Struct/Impl scope
            if matches!(scope.kind,
                ScopeKind::Class | ScopeKind::Struct | ScopeKind::Trait |
                ScopeKind::Impl | ScopeKind::Interface | ScopeKind::Enum) {
                if let Some(class_def_id) = scope.owner_symbol_id {
                    for sym in &mut parsed.symbols {
                        if sym.scope_id == Some(scope.id) && sym.id != class_def_id {
                            sym.owner_id = Some(class_def_id);
                        }
                    }
                }
            }
        }
    }
}
