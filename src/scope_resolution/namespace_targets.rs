//! Namespace target collection for scope-resolution.
//!
//! Builds a map from namespace alias → target file path for a parsed file.
//! Used by the receiver-bound calls pass to resolve namespace-qualified
//! calls like `models.User.create()`.

use rustc_hash::FxHashMap;
use crate::semantic::ParsedFile;
use crate::scope_resolution::{BindingOrigin, ScopeResolutionIndexes};

/// Build a map from namespace alias → target file path.
pub fn collect_namespace_targets(
    parsed: &ParsedFile,
    indexes: &ScopeResolutionIndexes,
) -> FxHashMap<String, String> {
    let mut targets = FxHashMap::default();

    // Find the module scope for this file
    let module_scope_id = parsed.scopes.iter()
        .find(|s| s.kind == crate::semantic::ScopeKind::Module)
        .map(|s| s.id);

    if let Some(msid) = module_scope_id {
        // Look for import bindings that map to files
        if let Some(bind_map) = indexes.bindings.get(&msid) {
            for (name, refs) in bind_map {
                for b in refs {
                    if b.origin == BindingOrigin::Import || b.origin == BindingOrigin::Namespace {
                        // The def_file_path is the target file
                        targets.insert(name.clone(), b.def_file_path.clone());
                    }
                }
            }
        }
    }

    targets
}
