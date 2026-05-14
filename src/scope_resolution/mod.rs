//! Scope-Resolution Pipeline (RFC #909 Ring 3)
//!
//! Language-agnostic registry-primary resolver. Replaces the legacy
//! Call-Resolution DAG for migrated languages.
//!
//! Pipeline stages:
//!   1. Extract ParsedFile per file (via SyntaxEngine)
//!   2. Finalize scope model → ScopeResolutionIndexes
//!   3. Resolve reference sites via scope-chain walk
//!   4. Emit edges (receiver-bound → free-call fallback → shared lookup → imports)
//!
//! Architecture mirrors GitNexus's scope-resolution pipeline:
//!   `runScopeResolution` orchestrator → per-language `ScopeResolver` hooks

pub mod walkers;
pub mod workspace_index;
pub mod graph_bridge;
pub mod receiver_bound;
pub mod free_call;
pub mod compound_receiver;
pub mod overload_narrowing;
pub mod namespace_targets;
pub mod reconcile_ownership;
pub mod orchestrator;

use serde::{Serialize, Deserialize};
use rustc_hash::FxHashMap;
use crate::lang::CaptureTag;
use crate::semantic::{Symbol, Scope, ScopeKind, ParsedFile, Confidence};

// =====================================================================
// Binding IR — cross-file name resolution
// =====================================================================

/// Where a binding originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingOrigin {
    /// Locally declared in this scope (def, assignment, parameter)
    Local,
    /// Brought in by an explicit import statement
    Import,
    /// Wildcard / namespace import (Go, Ruby, Swift)
    Wildcard,
    /// Re-export chain (C/C++ transitive includes)
    Reexport,
    /// Namespace alias resolved at call site (Python module alias)
    Namespace,
}

/// A reference to a symbol definition visible in a scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BindingRef {
    /// The symbol being bound.
    pub def_node_id: u64,
    /// The symbol name as it appears in the source.
    pub name: String,
    /// Where this binding came from.
    pub origin: BindingOrigin,
    /// The file path where the symbol is defined.
    pub def_file_path: String,
    /// Confidence tier for this binding.
    pub confidence: f64,
}

/// A type annotation on a variable/parameter in a scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeRef {
    /// The raw type name as written in source (e.g., "models.User")
    pub raw_name: String,
    /// The scope where this type binding was declared.
    pub declared_at_scope: u64,
}

// =====================================================================
// Scope-Resolution Indexes — the central lookup structure
// =====================================================================

/// Per-scope binding map: scope_id → (name → binding refs)
pub type ScopeBindings = FxHashMap<u64, FxHashMap<String, Vec<BindingRef>>>;

/// Post-finalize append-only binding augmentations (I8 invariant).
/// Hooks like `populateNamespaceSiblings` write here, never into `bindings`.
pub type ScopeBindingAugmentations = FxHashMap<u64, FxHashMap<String, Vec<BindingRef>>>;

/// Per-scope type bindings: scope_id → (name → type ref)
pub type ScopeTypeBindings = FxHashMap<u64, FxHashMap<String, TypeRef>>;

/// Method dispatch order: class def node_id → ancestor def node_ids (MRO order)
pub type MethodDispatch = FxHashMap<u64, Vec<u64>>;

/// The central index structure consumed by all resolution passes.
#[derive(Debug, Default)]
pub struct ScopeResolutionIndexes {
    /// Per-scope finalized bindings (immutable after finalize — I8).
    pub bindings: ScopeBindings,
    /// Post-finalize append-only binding augmentations (I8).
    pub binding_augmentations: ScopeBindingAugmentations,
    /// Per-scope type bindings (mutable post-finalize — I6).
    pub type_bindings: ScopeTypeBindings,
    /// Method dispatch order (MRO) per class.
    pub method_dispatch: MethodDispatch,
    /// All parsed files in this language.
    pub parsed_files: Vec<ParsedFile>,
    /// All scopes across all files, indexed by scope ID.
    pub scopes_by_id: FxHashMap<u64, Scope>,
    /// Scopes grouped by file path.
    pub scopes_by_file: FxHashMap<String, Vec<u64>>,
    /// Symbols grouped by file path.
    pub symbols_by_file: FxHashMap<String, Vec<Symbol>>,
    /// Symbols indexed by node ID.
    pub symbols_by_id: FxHashMap<u64, Symbol>,
    /// All imports across all files.
    pub imports: Vec<crate::semantic::Import>,
}

impl ScopeResolutionIndexes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up binding refs at a scope for a name, consulting both
    /// finalized and augmented channels (I8 dual-source).
    pub fn lookup_bindings_at(&self, scope_id: u64, name: &str) -> &[BindingRef] {
        // Check augmented first (post-finalize hooks), then finalized
        if let Some(aug_map) = self.binding_augmentations.get(&scope_id) {
            if let Some(aug_refs) = aug_map.get(name) {
                if !aug_refs.is_empty() {
                    return aug_refs;
                }
            }
        }
        if let Some(bind_map) = self.bindings.get(&scope_id) {
            if let Some(refs) = bind_map.get(name) {
                return refs;
            }
        }
        &[]
    }

    /// Walk scope chain upward looking for a type binding.
    pub fn find_type_binding(&self, start_scope: u64, name: &str) -> Option<&TypeRef> {
        let mut current = start_scope;
        let mut visited = rustc_hash::FxHashSet::default();
        loop {
            if !visited.insert(current) {
                return None;
            }
            if let Some(tb) = self.type_bindings.get(&current) {
                if let Some(tr) = tb.get(name) {
                    return Some(tr);
                }
            }
            // Walk to parent
            if let Some(scope) = self.scopes_by_id.get(&current) {
                if let Some(parent) = scope.parent_id {
                    current = parent;
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
    }

    /// Walk scope chain upward looking for a binding (class-like).
    pub fn find_class_binding(&self, start_scope: u64, name: &str) -> Option<&BindingRef> {
        let mut current = start_scope;
        let mut visited = rustc_hash::FxHashSet::default();
        loop {
            if !visited.insert(current) {
                return None;
            }
            // Check local scope bindings
            if let Some(scope) = self.scopes_by_id.get(&current) {
                if let Some(local_map) = self.bindings.get(&current) {
                    if let Some(refs) = local_map.get(name) {
                        for b in refs {
                            if is_class_like_def(&self, b.def_node_id) {
                                return Some(b);
                            }
                        }
                    }
                }
                // Check augmented
                if let Some(aug_map) = self.binding_augmentations.get(&current) {
                    if let Some(refs) = aug_map.get(name) {
                        for b in refs {
                            if is_class_like_def(&self, b.def_node_id) {
                                return Some(b);
                            }
                        }
                    }
                }
                // Walk to parent
                if let Some(parent) = scope.parent_id {
                    current = parent;
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
    }

    /// Walk scope chain upward looking for a callable binding.
    pub fn find_callable_binding(&self, start_scope: u64, name: &str) -> Option<&BindingRef> {
        let mut current = start_scope;
        let mut visited = rustc_hash::FxHashSet::default();
        loop {
            if !visited.insert(current) {
                return None;
            }
            if let Some(scope) = self.scopes_by_id.get(&current) {
                // Check local
                if let Some(local_map) = self.bindings.get(&current) {
                    if let Some(refs) = local_map.get(name) {
                        for b in refs {
                            if let Some(sym) = self.symbols_by_id.get(&b.def_node_id) {
                                if matches!(sym.kind,
                                    CaptureTag::DefinitionFunction |
                                    CaptureTag::DefinitionMethod |
                                    CaptureTag::DefinitionConstructor) {
                                    return Some(b);
                                }
                            }
                        }
                    }
                }
                // Check augmented
                if let Some(aug_map) = self.binding_augmentations.get(&current) {
                    if let Some(refs) = aug_map.get(name) {
                        for b in refs {
                            if let Some(sym) = self.symbols_by_id.get(&b.def_node_id) {
                                if matches!(sym.kind,
                                    CaptureTag::DefinitionFunction |
                                    CaptureTag::DefinitionMethod |
                                    CaptureTag::DefinitionConstructor) {
                                    return Some(b);
                                }
                            }
                        }
                    }
                }
                if let Some(parent) = scope.parent_id {
                    current = parent;
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
    }

    /// Find the enclosing class def for a scope.
    pub fn find_enclosing_class_def(&self, start_scope: u64) -> Option<&Symbol> {
        let mut current = start_scope;
        let mut visited = rustc_hash::FxHashSet::default();
        loop {
            if !visited.insert(current) {
                return None;
            }
            if let Some(scope) = self.scopes_by_id.get(&current) {
                if matches!(scope.kind, ScopeKind::Class | ScopeKind::Interface | ScopeKind::Struct | ScopeKind::Enum | ScopeKind::Trait) {
                    if let Some(owner_id) = scope.owner_symbol_id {
                        if let Some(sym) = self.symbols_by_id.get(&owner_id) {
                            return Some(sym);
                        }
                    }
                }
                if let Some(parent) = scope.parent_id {
                    current = parent;
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
    }

    /// Find a member (method/field) owned by a class symbol.
    pub fn find_owned_member(&self, owner_symbol_id: u64, member_name: &str) -> Option<&Symbol> {
        for sym in self.symbols_by_id.values() {
            if sym.owner_id == Some(owner_symbol_id) && sym.name == member_name {
                return Some(sym);
            }
        }
        None
    }

    /// Find an exported def in a target file's module scope.
    pub fn find_exported_def(&self, target_file: &str, name: &str) -> Option<&Symbol> {
        if let Some(scope_ids) = self.scopes_by_file.get(target_file) {
            for sid in scope_ids {
                if let Some(scope) = self.scopes_by_id.get(sid) {
                    if matches!(scope.kind, ScopeKind::Module) {
                        // Check local bindings in module scope
                        if let Some(local_map) = self.bindings.get(sid) {
                            if let Some(refs) = local_map.get(name) {
                                for b in refs {
                                    if let Some(sym) = self.symbols_by_id.get(&b.def_node_id) {
                                        return Some(sym);
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
}

/// Check if a symbol is a class-like type.
fn is_class_like_def(indexes: &ScopeResolutionIndexes, node_id: u64) -> bool {
    if let Some(sym) = indexes.symbols_by_id.get(&node_id) {
        matches!(sym.kind,
            CaptureTag::DefinitionClass |
            CaptureTag::DefinitionInterface |
            CaptureTag::DefinitionStruct |
            CaptureTag::DefinitionEnum |
            CaptureTag::DefinitionTrait |
            CaptureTag::DefinitionRecord)
    } else {
        false
    }
}

/// Check if a scope kind is class-like.
pub fn is_class_like_scope(kind: ScopeKind) -> bool {
    matches!(kind,
        ScopeKind::Class | ScopeKind::Interface | ScopeKind::Struct |
        ScopeKind::Enum | ScopeKind::Trait)
}

// =====================================================================
// Reference site — a call/read/write site to be resolved
// =====================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferenceKind {
    Call,
    Read,
    Write,
    Inherits,
    TypeReference,
}

/// A reference site in source code that needs resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceSite {
    /// The kind of reference (call, read, write, etc.)
    pub kind: ReferenceKind,
    /// The name being referenced (function name, method name, etc.)
    pub name: String,
    /// The explicit receiver, if any (e.g., "self", "obj", "ClassName")
    pub explicit_receiver: Option<String>,
    /// The scope in which this reference occurs.
    pub in_scope: u64,
    /// Line number (0-indexed).
    pub line: usize,
    /// Column number (0-indexed).
    pub col: usize,
    /// Number of arguments (for calls).
    pub arity: Option<usize>,
    /// Whether this is a constructor call (new X()).
    pub is_constructor: bool,
}

// =====================================================================
// Resolution result
// =====================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedReference {
    pub site: ReferenceSite,
    pub target_symbol_id: Option<u64>,
    pub target_file_path: Option<String>,
    pub confidence: Confidence,
    pub edge_kind: Option<String>, // "CALLS", "ACCESSES", "EXTENDS", "USES"
}

/// Statistics from a scope-resolution run.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct ScopeResolutionStats {
    pub files_processed: usize,
    pub files_skipped: usize,
    pub imports_emitted: usize,
    pub reference_edges_emitted: usize,
    pub resolved_sites: usize,
    pub unresolved_sites: usize,
}
