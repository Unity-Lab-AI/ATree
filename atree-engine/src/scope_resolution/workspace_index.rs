//! WorkspaceResolutionIndex — scope-tied lookup tables built ONCE
//! per resolution run, after `populateOwners` and before any
//! resolution pass.
//!
//! Carries only lookups that return a `Scope` — things the
//! symbol-level indexes structurally cannot provide.

use rustc_hash::FxHashMap;
use crate::semantic::{ParsedFile, Scope, ScopeKind};
use crate::scope_resolution::is_class_like_scope;

/// Scope-tied lookup index.
pub struct WorkspaceResolutionIndex {
    /// Class def node ID → that class's scope ID.
    pub class_scope_by_def_id: FxHashMap<u64, u64>,
    /// Inverse: class scope ID → class def node ID.
    pub class_scope_id_to_def_id: FxHashMap<u64, u64>,
    /// Module scope by file path.
    pub module_scope_by_file: FxHashMap<String, u64>,
    /// All scopes indexed by ID.
    scopes_by_id: FxHashMap<u64, Scope>,
}

impl WorkspaceResolutionIndex {
    pub fn new(
        parsed_files: &[ParsedFile],
        scopes_by_id: &FxHashMap<u64, Scope>,
    ) -> Self {
        let mut class_scope_by_def_id = FxHashMap::default();
        let mut class_scope_id_to_def_id = FxHashMap::default();
        let mut module_scope_by_file = FxHashMap::default();

        for parsed in parsed_files {
            for scope in &parsed.scopes {
                if scope.kind == ScopeKind::Module {
                    module_scope_by_file.insert(parsed.path.clone(), scope.id);
                }
                if is_class_like_scope(scope.kind) {
                    if let Some(owner_id) = scope.owner_symbol_id {
                        class_scope_by_def_id.insert(owner_id, scope.id);
                        class_scope_id_to_def_id.insert(scope.id, owner_id);
                    }
                }
            }
        }

        Self {
            class_scope_by_def_id,
            class_scope_id_to_def_id,
            module_scope_by_file,
            scopes_by_id: scopes_by_id.clone(),
        }
    }

    /// Get a class scope by its def node ID.
    pub fn class_scope(&self, def_node_id: u64) -> Option<&Scope> {
        self.class_scope_by_def_id
            .get(&def_node_id)
            .and_then(|sid| self.scopes_by_id.get(sid))
    }

    /// Get a class def node ID by its scope ID.
    pub fn class_def_id(&self, scope_id: u64) -> Option<u64> {
        self.class_scope_id_to_def_id.get(&scope_id).copied()
    }

    /// Get a module scope by file path.
    pub fn module_scope(&self, file_path: &str) -> Option<&Scope> {
        self.module_scope_by_file
            .get(file_path)
            .and_then(|sid| self.scopes_by_id.get(sid))
    }
}
