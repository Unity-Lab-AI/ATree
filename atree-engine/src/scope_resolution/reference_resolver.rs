//! Registry-powered reference resolver — bridges scope-resolution indexes
//! to the evidence-based registry lookup system.
//!
//! This is the producer for the ReferenceIndex: for each reference site,
//! it calls `lookup_core` through the appropriate registry (class/method/field)
//! and produces a `Reference` record with evidence-based confidence.
//!
//! ## Architecture
//!
//! ```text
//! ScopeResolutionIndexes ──► RegistryContext ──► lookup_core()
//!                                                      │
//!                                                      ▼
//!                                               ReferenceIndex
//!                                                      │
//!                                                      ▼
//!                                               GraphEdge (fine_confidence)
//! ```

use rustc_hash::FxHashMap;
use crate::semantic::reference_index::{ReferenceIndex, Reference, ReferenceKind, Range as RefRange};
use crate::semantic::registries::*;
use crate::scope_resolution::{ScopeResolutionIndexes, BindingOrigin as ScopeBindingOrigin};
use crate::semantic::Symbol;

/// Build a `RegistryContext` from the existing `ScopeResolutionIndexes`.
///
/// Converts the u64-based ID system to the registry's string-keyed system.
/// - Scope IDs: `"scope:{u64}"`
/// - Symbol keys: `"sym:{u64}"` (using the symbol's numeric ID as key)
/// - Qualified names: from `qualified_name` field on symbols
/// - MRO: from `method_dispatch`
pub fn build_registry_context(indexes: &ScopeResolutionIndexes) -> RegistryContext {
    let mut scopes = FxHashMap::default();
    let mut defs = FxHashMap::default();
    let mut qualified_names: FxHashMap<String, Vec<SymbolDefinition>> = FxHashMap::default();

    // ── Build defs map from symbols_by_id ────────────────────────────
    // We use the symbol's numeric ID as a synthetic SymbolId key.
    for sym in indexes.symbols_by_id.values() {
        let sym_key = format!("sym:{}", sym.id);
        let def = SymbolDefinition {
            id: make_synth_symbol_id(sym.id, &sym.name, &sym_to_kind_str(sym)),
            name: sym.name.clone(),
            qualified_name: sym.qualified_name.clone(),
            kind: sym_to_kind_str(sym),
            owner_id: sym.owner_id.map(|oid| make_synth_symbol_id(oid, "", "")),
            file_id: FileId::new("default", &format!("file:{}", sym.file_id), 0),
            line: sym.line,
            col: sym.col,
            is_exported: sym.is_exported,
        };
        defs.insert(sym_key, def.clone());

        // Also index by qualified name for global fallback.
        if !sym.qualified_name.is_empty() {
            qualified_names.entry(sym.qualified_name.clone())
                .or_default()
                .push(def);
        }
    }

    // ── Build scope tree from scopes_by_id ───────────────────────────
    for (scope_id, scope) in &indexes.scopes_by_id {
        let reg_scope_id = format!("scope:{}", scope_id);
        let parent_id = scope.parent_id.map(|pid| format!("scope:{}", pid));
        let owner_id = scope.owner_symbol_id
            .map(|oid| make_synth_symbol_id(oid, "", ""));

        // Build bindings for this scope from indexes.bindings.
        let mut bindings: FxHashMap<String, Vec<crate::semantic::registries::BindingRef>> = FxHashMap::default();
        if let Some(bind_map) = indexes.bindings.get(scope_id) {
            for (name, refs) in bind_map {
                let reg_refs: Vec<crate::semantic::registries::BindingRef> = refs.iter().map(|br| {
                    let def = defs.get(&format!("sym:{}", br.def_node_id))
                        .cloned()
                        .unwrap_or_else(|| SymbolDefinition {
                            id: make_synth_symbol_id(br.def_node_id, &br.name, ""),
                            name: br.name.clone(),
                            qualified_name: br.name.clone(),
                            kind: "Unknown".to_string(),
                            owner_id: None,
                            file_id: FileId::new("default", &format!("file:{}", br.def_node_id), 0),
                            line: 0,
                            col: 0,
                            is_exported: false,
                        });
                    crate::semantic::registries::BindingRef {
                        def,
                        origin: map_binding_origin(br.origin),
                        via_unlinked_import: br.confidence < 0.5, // heuristic
                        dynamic_unresolved: false,
                    }
                }).collect();
                bindings.insert(name.clone(), reg_refs);
            }
        }

        // Build type bindings for this scope from indexes.type_bindings.
        let mut type_bindings: FxHashMap<String, crate::semantic::registries::TypeRef> = FxHashMap::default();
        if let Some(tb_map) = indexes.type_bindings.get(scope_id) {
            for (name, tr) in tb_map {
                type_bindings.insert(name.clone(), crate::semantic::registries::TypeRef {
                    raw_name: tr.raw_name.clone(),
                    resolved_symbol_id: None,
                });
            }
        }

        let reg_scope = crate::semantic::registries::Scope {
            id: reg_scope_id.clone(),
            file_id: FileId::new("default", &format!("file:{}", scope.file_id), 0),
            parent_id,
            owner_symbol_id: owner_id,
            kind: map_scope_kind(scope.kind),
            line_start: scope.line_start,
            line_end: scope.line_end,
            bindings,
            type_bindings,
        };
        scopes.insert(reg_scope_id, reg_scope);
    }

    // ── Build MRO from method_dispatch ──────────────────────────────
    let mut mro: FxHashMap<String, Vec<SymbolDefinition>> = FxHashMap::default();
    for (class_id, ancestors) in &indexes.method_dispatch {
        let class_key = format!("sym:{}", class_id);
        let mro_defs: Vec<SymbolDefinition> = ancestors.iter()
            .filter_map(|aid| defs.get(&format!("sym:{}", aid)).cloned())
            .collect();
        if !mro_defs.is_empty() {
            mro.insert(class_key, mro_defs);
        }
    }

    RegistryContext {
        scopes,
        defs,
        qualified_names,
        mro,
        providers: RegistryProviders {
            arity_compatibility: None,
        },
    }
}

/// Compute evidence-based confidence for a name at a scope, using the
/// appropriate registry based on the reference kind.
pub fn resolve_reference_confidence(
    name: &str,
    scope_id: u64,
    kind: &crate::scope_resolution::ReferenceKind,
    ctx: &RegistryContext,
) -> Option<f64> {
    let scope_str = format!("scope:{}", scope_id);
    if !ctx.scopes.contains_key(&scope_str) {
        return None;
    }

    let params = match kind {
        crate::scope_resolution::ReferenceKind::Call => LookupParams::for_methods(),
        crate::scope_resolution::ReferenceKind::Read |
        crate::scope_resolution::ReferenceKind::Write => {
            // Try method lookup first, then field lookup
            let method_params = LookupParams::for_methods();
            let results = lookup_core(name, &scope_str, &method_params, ctx);
            if !results.is_empty() {
                return Some(results[0].confidence);
            }
            LookupParams::for_fields()
        }
        crate::scope_resolution::ReferenceKind::Inherits |
        crate::scope_resolution::ReferenceKind::TypeReference => LookupParams::for_classes(),
    };

    let results = lookup_core(name, &scope_str, &params, ctx);
    results.first().map(|r| r.confidence)
}

/// Populate a `ReferenceIndex` by resolving all reference sites through the
/// registry system. Returns the populated index.
pub fn build_reference_index(
    _indexes: &ScopeResolutionIndexes,
    ctx: &RegistryContext,
    reference_sites: &[crate::scope_resolution::ReferenceSite],
) -> ReferenceIndex {
    let mut ref_index = ReferenceIndex::new();

    for site in reference_sites {
        let scope_str = format!("scope:{}", site.in_scope);
        let params = match site.kind {
            crate::scope_resolution::ReferenceKind::Call => LookupParams::for_methods(),
            crate::scope_resolution::ReferenceKind::Read |
            crate::scope_resolution::ReferenceKind::Write => LookupParams::for_fields(),
            crate::scope_resolution::ReferenceKind::Inherits |
            crate::scope_resolution::ReferenceKind::TypeReference => LookupParams::for_classes(),
        };

        let results = lookup_core(&site.name, &scope_str, &params, ctx);
        if let Some(best) = results.first() {
            let ref_kind = map_kind(site.kind.clone());
            let reference = Reference {
                from_scope: scope_str.clone(),
                to_def: best.def_id.clone(),
                at_range: RefRange {
                    start_line: site.line,
                    start_col: site.col,
                    end_line: site.line,
                    end_col: site.col + site.name.len().max(1),
                },
                kind: ref_kind,
                confidence: best.confidence,
            };
            ref_index.add(reference);
        }
    }

    ref_index
}

/// Emit `GraphEdge`s from a `ReferenceIndex`, using evidence-based confidence.
/// Each reference that resolves to a known symbol gets a CALLS/ACCESSES/USES edge.
///
/// `ctx` is the registry context used to build the reference index. Its `defs`
/// map is keyed by `"sym:{u64}"` and is used to resolve the `SymbolId`-based
/// `to_def` strings back to numeric IDs.
pub fn emit_reference_index_edges(
    ref_index: &ReferenceIndex,
    indexes: &ScopeResolutionIndexes,
    ctx: &RegistryContext,
    edges: &mut Vec<crate::scope_resolution::graph_bridge::GraphEdge>,
) -> usize {
    use crate::scope_resolution::graph_bridge::GraphEdge;
    let mut count = 0;

    // Build a reverse map: SymbolId::as_key() (long structured string) → u64.
    // The defs map is keyed by "sym:{u64}", but each SymbolDefinition carries
    // a full SymbolId whose as_key() is a long structured string. Resolution
    // records store the long key, so we need this to translate back to u64.
    let sym_key_to_u64: FxHashMap<String, u64> = ctx.defs.iter()
        .filter_map(|(sym_key, def)| {
            let id = parse_sym_id(sym_key)?;
            Some((def.id.as_key(), id))
        })
        .collect();

    // For each reference, find the source symbol and target symbol IDs.
    // The reference's from_scope tells us which scope, and we walk up
    // to find the enclosing callable/class.
    for ref_record in ref_index.iter() {
        // Look up the target u64 from the SymbolId key.
        let target_id = match sym_key_to_u64.get(&ref_record.to_def) {
            Some(id) => *id,
            None => continue,
        };

        // The source is the enclosing function/method, found by walking
        // the scope chain from from_scope.
        let source_id = resolve_scope_caller(&ref_record.from_scope, indexes);
        let source_id = match source_id {
            Some(id) => id,
            None => continue,
        };

        // Determine edge type.
        let edge_type = match ref_record.kind {
            ReferenceKind::Call => "CALLS",
            ReferenceKind::Read | ReferenceKind::Write => "ACCESSES",
            ReferenceKind::Inherits => "EXTENDS",
            ReferenceKind::TypeReference | ReferenceKind::ImportUse => "USES",
        };

        // Skip self-loops: a symbol should not call itself.
        if source_id == target_id {
            continue;
        }

        // Don't emit duplicates.
        let already = edges.iter().any(|e| {
            e.edge_type == edge_type && e.source_id == source_id && e.target_id == target_id
        });
        if already {
            continue;
        }

        edges.push(GraphEdge {
            source_id,
            target_id,
            edge_type: edge_type.to_string(),
            confidence: ref_record.confidence,
            reason: "registry-resolution".to_string(),
        });
        count += 1;
    }

    count
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Make a synthetic SymbolId from a numeric u64 ID.
fn make_synth_symbol_id(id: u64, name: &str, kind: &str) -> SymbolId {
    use std::hash::{Hash, Hasher};
    let fid = FileId::new("default", &format!("file:{}", id), 0);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    SymbolId {
        file_id: fid,
        kind: kind.to_string(),
        qualified_name: name.to_string(),
        span_hash: hasher.finish(),
    }
}

/// Convert a u64 sym ID string back to number.
fn parse_sym_id(key: &str) -> Option<u64> {
    if let Some(rest) = key.strip_prefix("sym:") {
        rest.parse::<u64>().ok()
    } else {
        None
    }
}

/// Walk from a scope string to find the enclosing caller symbol ID.
fn resolve_scope_caller(scope_str: &str, indexes: &ScopeResolutionIndexes) -> Option<u64> {
    let scope_id = scope_str.strip_prefix("scope:")?.parse::<u64>().ok()?;
    let mut current = scope_id;
    let mut visited = rustc_hash::FxHashSet::default();
    loop {
        if !visited.insert(current) {
            return None;
        }
        let scope = indexes.scopes_by_id.get(&current)?;
        // Collect candidate symbols in this scope.
        let candidates: Vec<u64> = indexes.symbols_by_id.values()
            .filter(|sym| sym.scope_id == Some(current))
            .map(|sym| sym.id)
            .collect();
        if !candidates.is_empty() {
            // Prefer callable symbols (Function, Method, Constructor) — these are
            // the actual callers in a call graph. Fall back to any symbol if no
            // callable exists (e.g., top-level module scope).
            let callable = candidates.iter().find(|&&id| {
                indexes.symbols_by_id.get(&id).map_or(false, |s| {
                    matches!(s.kind,
                        crate::lang::CaptureTag::DefinitionFunction |
                        crate::lang::CaptureTag::DefinitionMethod |
                        crate::lang::CaptureTag::DefinitionConstructor)
                })
            });
            return Some(*callable.unwrap_or(&candidates[0]));
        }
        current = scope.parent_id?;
    }
}

/// Map scope-resolution ReferenceKind to reference_index ReferenceKind.
fn map_kind(kind: crate::scope_resolution::ReferenceKind) -> ReferenceKind {
    match kind {
        crate::scope_resolution::ReferenceKind::Call => ReferenceKind::Call,
        crate::scope_resolution::ReferenceKind::Read => ReferenceKind::Read,
        crate::scope_resolution::ReferenceKind::Write => ReferenceKind::Write,
        crate::scope_resolution::ReferenceKind::Inherits => ReferenceKind::Inherits,
        crate::scope_resolution::ReferenceKind::TypeReference => ReferenceKind::TypeReference,
    }
}

/// Map scope-resolution BindingOrigin to registry BindingOrigin.
fn map_binding_origin(o: ScopeBindingOrigin) -> crate::semantic::evidence::BindingOrigin {
    match o {
        ScopeBindingOrigin::Local => crate::semantic::evidence::BindingOrigin::Local,
        ScopeBindingOrigin::Import => crate::semantic::evidence::BindingOrigin::Import,
        ScopeBindingOrigin::Wildcard => crate::semantic::evidence::BindingOrigin::Wildcard,
        ScopeBindingOrigin::Reexport => crate::semantic::evidence::BindingOrigin::Reexport,
        ScopeBindingOrigin::Namespace => crate::semantic::evidence::BindingOrigin::Namespace,
    }
}

/// Map Semantic ScopeKind to Registry ScopeKind.
fn map_scope_kind(kind: crate::semantic::ScopeKind) -> crate::semantic::registries::ScopeKind {
    use crate::semantic::registries::ScopeKind as R;
    use crate::semantic::ScopeKind as S;
    match kind {
        S::Module => R::Module,
        S::Function => R::Function,
        S::Class => R::Class,
        S::Interface => R::Interface,
        S::Struct => R::Struct,
        S::Enum => R::Enum,
        S::Trait => R::Trait,
        S::Impl => R::Impl,
        S::Block => R::Block,
        S::Namespace => R::Namespace,
        S::Method => R::Method,
        S::Constructor => R::Constructor,
        S::Unknown => R::Unknown,
    }
}

/// Convert CaptureTag to a kind string for the registry.
fn sym_to_kind_str(sym: &Symbol) -> String {
    use crate::lang::CaptureTag;
    match sym.kind {
        CaptureTag::DefinitionClass => "Class".to_string(),
        CaptureTag::DefinitionInterface => "Interface".to_string(),
        CaptureTag::DefinitionStruct => "Struct".to_string(),
        CaptureTag::DefinitionEnum => "Enum".to_string(),
        CaptureTag::DefinitionTrait => "Trait".to_string(),
        CaptureTag::DefinitionFunction => "Function".to_string(),
        CaptureTag::DefinitionMethod => "Method".to_string(),
        CaptureTag::DefinitionConstructor => "Constructor".to_string(),
        CaptureTag::DefinitionVariable => "Variable".to_string(),
        CaptureTag::DefinitionProperty => "Property".to_string(),
        CaptureTag::DefinitionConst => "Const".to_string(),
        CaptureTag::DefinitionStatic => "Static".to_string(),
        CaptureTag::DefinitionRecord => "Record".to_string(),
        CaptureTag::DefinitionDelegate => "Delegate".to_string(),
        CaptureTag::DefinitionAnnotation => "Annotation".to_string(),
        _ => "Unknown".to_string(),
    }
}

/// Combined: build reference index, emit edges, and return the set of resolved site keys.
/// This replaces the three-step process (build_reference_index → emit_reference_index_edges → build resolved_sites set)
/// with a single pass that also returns the resolved sites for downstream phases.
pub fn build_reference_index_and_resolve(
    indexes: &ScopeResolutionIndexes,
    ctx: &RegistryContext,
    reference_sites: &[crate::scope_resolution::ReferenceSite],
    edges: &mut Vec<crate::scope_resolution::graph_bridge::GraphEdge>,
) -> rustc_hash::FxHashSet<String> {
    use crate::scope_resolution::graph_bridge::GraphEdge;
    let mut resolved_sites = rustc_hash::FxHashSet::default();

    // Build reverse map: SymbolId::as_key() → u64
    let sym_key_to_u64: FxHashMap<String, u64> = ctx.defs.iter()
        .filter_map(|(sym_key, def)| {
            let id = parse_sym_id(sym_key)?;
            Some((def.id.as_key(), id))
        })
        .collect();

    for site in reference_sites {
        let scope_str = format!("scope:{}", site.in_scope);
        let params = match site.kind {
            crate::scope_resolution::ReferenceKind::Call => LookupParams::for_methods(),
            crate::scope_resolution::ReferenceKind::Read |
            crate::scope_resolution::ReferenceKind::Write => LookupParams::for_fields(),
            crate::scope_resolution::ReferenceKind::Inherits |
            crate::scope_resolution::ReferenceKind::TypeReference => LookupParams::for_classes(),
        };

        let results = lookup_core(&site.name, &scope_str, &params, ctx);
        if let Some(best) = results.first() {
            let target_id = match sym_key_to_u64.get(&best.def_id) {
                Some(id) => *id,
                None => continue,
            };
            let source_id = match resolve_scope_caller(&scope_str, indexes) {
                Some(id) => id,
                None => continue,
            };

            // Skip self-loops: a symbol should not call itself.
            if source_id == target_id {
                continue;
            }

            let edge_type = match site.kind {
                crate::scope_resolution::ReferenceKind::Call => "CALLS",
                crate::scope_resolution::ReferenceKind::Read |
                crate::scope_resolution::ReferenceKind::Write => "ACCESSES",
                crate::scope_resolution::ReferenceKind::Inherits => "EXTENDS",
                crate::scope_resolution::ReferenceKind::TypeReference => "USES",
            };

            let already = edges.iter().any(|e| {
                e.edge_type == edge_type && e.source_id == source_id && e.target_id == target_id
            });
            if already {
                continue;
            }

            edges.push(GraphEdge {
                source_id,
                target_id,
                edge_type: edge_type.to_string(),
                confidence: best.confidence,
                reason: "registry-resolution".to_string(),
            });

            let site_key = format!("{}:{}:{}", site.in_scope, site.line, site.col);
            resolved_sites.insert(site_key);
        }
    }

    resolved_sites
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::ScopeKind;
    use crate::scope_resolution::BindingRef as ScopeBr;

    #[test]
    fn test_build_from_empty_indexes() {
        let indexes = ScopeResolutionIndexes::new();
        let ctx = build_registry_context(&indexes);
        assert!(ctx.scopes.is_empty());
        assert!(ctx.defs.is_empty());
    }

    #[test]
    fn test_resolve_with_no_context() {
        let ctx = build_registry_context(&ScopeResolutionIndexes::new());
        let conf = resolve_reference_confidence("foo", 1,
            &crate::scope_resolution::ReferenceKind::Call, &ctx);
        assert!(conf.is_none());
    }

    #[test]
    fn test_resolve_known_symbol() {
        let mut indexes = ScopeResolutionIndexes::new();
        // Add a scope
        let scope = crate::semantic::Scope {
            id: 1, file_id: 1, parent_id: None, owner_symbol_id: None,
            kind: ScopeKind::Module, line_start: 0, line_end: 100,
        };
        indexes.scopes_by_id.insert(1, scope);
        // Add a symbol with a binding
        let sym = Symbol {
            id: 100, name: "my_func".to_string(), qualified_name: "my_func".to_string(),
            kind: crate::lang::CaptureTag::DefinitionFunction,
            file_id: 1, scope_id: Some(1), owner_id: None,
            line: 10, col: 0, is_exported: true,
        };
        indexes.symbols_by_id.insert(100, sym);
        // Add binding at scope 1
        let binding = ScopeBr {
            def_node_id: 100,
            name: "my_func".to_string(),
            origin: ScopeBindingOrigin::Local,
            def_file_path: "lib.rs".to_string(),
            confidence: 1.0,
        };
        indexes.bindings.entry(1).or_default().entry("my_func".to_string()).or_default().push(binding);

        let ctx = build_registry_context(&indexes);
        let conf = resolve_reference_confidence("my_func", 1,
            &crate::scope_resolution::ReferenceKind::Call, &ctx);
        assert!(conf.is_some(), "Should resolve known symbol");
        // Local binding should give confidence >= 0.55 (LOCAL weight)
        assert!(conf.unwrap() >= 0.55, "Local binding confidence should be >= 0.55");
    }
}
