//! Scope-resolution orchestrator — ties all passes together.
//!
//! Equivalent to GitNexus's `runScopeResolution` in `pipeline/run.rs`.

use crate::semantic::{ParsedFile, Call, ScopeKind, Confidence};
use crate::scope_resolution::{
    ScopeResolutionIndexes, ScopeBindings, ScopeBindingAugmentations,
    ScopeTypeBindings, MethodDispatch, BindingRef, BindingOrigin, TypeRef,
    ReferenceSite, ReferenceKind, ScopeResolutionStats,
};
use crate::scope_resolution::walkers;
use crate::scope_resolution::workspace_index::WorkspaceResolutionIndex;
use crate::scope_resolution::graph_bridge::{self, GraphEdge, GraphNodeLookup};
use crate::scope_resolution::receiver_bound::{self, ReceiverBoundProvider};
use crate::scope_resolution::free_call;
use crate::scope_resolution::reconcile_ownership;
use crate::resolver::c3;
use std::collections::HashMap;

/// Run the full scope-resolution pipeline on parsed files.
///
/// This is the entry point equivalent to GitNexus's `runScopeResolution`.
pub fn run_scope_resolution(
    parsed_files: &mut [ParsedFile],
    all_file_paths: &[String],
) -> (ScopeResolutionStats, Vec<GraphEdge>) {
    let mut stats = ScopeResolutionStats::default();
    stats.files_processed = parsed_files.len();

    if parsed_files.is_empty() {
        return (stats, Vec::new());
    }

    // Phase 1: Reconcile ownership (populate class-owned members)
    reconcile_ownership::reconcile_ownership(parsed_files);

    // Phase 2: Build ScopeResolutionIndexes
    let mut indexes = build_indexes(parsed_files, all_file_paths);

    // Phase 3: Build WorkspaceResolutionIndex
    let workspace_index = WorkspaceResolutionIndex::new(parsed_files, &indexes.scopes_by_id);

    // Phase 4: Build Method Dispatch (MRO) for all classes
    build_method_dispatch(&mut indexes, parsed_files);

    // Phase 4b: Emit MRO edges (EXTENDS/IMPLEMENTS) from computed method dispatch
    let mut mro_edges = Vec::new();
    emit_mro_edges(&indexes, &mut mro_edges);
    stats.reference_edges_emitted += mro_edges.len();

    // Phase 5: Extract reference sites from parsed files
    let reference_sites = extract_reference_sites(parsed_files);

    // Phase 6: Build graph node lookup
    let node_lookup = build_node_lookup(&indexes);

    // Phase 7: Emit edges (load-bearing order per Contract Invariant I1)
    let mut edges = Vec::new();
    let mut seen: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();

    // 6c: MRO edges (EXTENDS/IMPLEMENTS) — emitted first so call resolution can use them
    edges.extend(mro_edges);

    // 7a: Receiver-bound calls FIRST
    let provider = ReceiverBoundProvider {
        is_super_receiver: &|name| name == "super" || name == "base" || name == "super()",
        field_fallback: true,
        collapse_member_calls: false,
        hoist_type_bindings_to_module: false,
    };

    let receiver_emitted = receiver_bound::emit_receiver_bound_calls(
        &mut edges,
        &indexes,
        &reference_sites,
        &node_lookup,
        &provider,
        &workspace_index,
    );
    stats.reference_edges_emitted += receiver_emitted;

    // 7b: Free-call fallback SECOND
    let free_emitted = free_call::emit_free_call_fallback(
        &mut edges,
        &indexes,
        &reference_sites,
        &node_lookup,
        &workspace_index,
    );
    stats.reference_edges_emitted += free_emitted;

    // 7c: Import edges LAST
    let import_emitted = graph_bridge::emit_import_edges(&mut edges, &indexes);
    stats.imports_emitted = import_emitted;

    // Count unresolved
    stats.unresolved_sites = reference_sites.len(); // simplified

    (stats, edges)
}

/// Emit MRO edges (EXTENDS/IMPLEMENTS) from computed method dispatch.
/// For each class with MRO, emit edges: class → each ancestor in MRO order.
fn emit_mro_edges(indexes: &ScopeResolutionIndexes, edges: &mut Vec<GraphEdge>) {
    for (class_id, ancestor_ids) in &indexes.method_dispatch {
        for (order, ancestor_id) in ancestor_ids.iter().enumerate() {
            edges.push(GraphEdge {
                source_id: *class_id,
                target_id: *ancestor_id,
                edge_type: "EXTENDS".to_string(),
                confidence: 1.0,
                reason: format!("MRO-order-{}", order),
            });
        }
    }
}

/// Build ScopeResolutionIndexes from parsed files.
fn build_indexes(
    parsed_files: &[ParsedFile],
    all_file_paths: &[String],
) -> ScopeResolutionIndexes {
    let mut indexes = ScopeResolutionIndexes::new();

    for parsed in parsed_files {
        // Index scopes
        for scope in &parsed.scopes {
            indexes.scopes_by_id.insert(scope.id, scope.clone());
            indexes.scopes_by_file
                .entry(parsed.path.clone())
                .or_default()
                .push(scope.id);
        }

        // Index symbols
        for sym in &parsed.symbols {
            indexes.symbols_by_id.insert(sym.id, sym.clone());
            indexes.symbols_by_file
                .entry(parsed.path.clone())
                .or_default()
                .push(sym.clone());
        }

        // Build bindings from imports
        for imp in &parsed.imports {
            let binding = BindingRef {
                def_node_id: imp.resolved_file_id.unwrap_or(0),
                name: imp.local_name.clone(),
                origin: BindingOrigin::Import,
                def_file_path: imp.source.clone(),
                confidence: imp.confidence.score(),
            };

            // Find the module scope for this file
            if let Some(module_scope) = parsed.scopes.iter().find(|s| s.kind == ScopeKind::Module) {
                indexes.bindings
                    .entry(module_scope.id)
                    .or_default()
                    .entry(imp.local_name.clone())
                    .or_default()
                    .push(binding);
            }
        }

        // Build type bindings from assignments
        for assign in &parsed.assignments {
            let type_ref = TypeRef {
                raw_name: assign.name.clone(),
                declared_at_scope: 0, // Will be resolved from scope
            };
            // Find the scope for this assignment
            for scope in &parsed.scopes {
                if assign.line >= scope.line_start && assign.line <= scope.line_end {
                    indexes.type_bindings
                        .entry(scope.id)
                        .or_default()
                        .entry(assign.name.clone())
                        .or_insert(type_ref);
                    break;
                }
            }
        }

        indexes.parsed_files.push(parsed.clone());
        indexes.imports.extend(parsed.imports.clone());
    }

    indexes
}

/// Build method dispatch (MRO) for all classes using C3 linearization.
fn build_method_dispatch(indexes: &mut ScopeResolutionIndexes, parsed_files: &[ParsedFile]) {
    // Build parent map: class_def_id → Vec<parent_class_name>
    let mut parent_map: HashMap<u64, Vec<(String, String)>> = HashMap::new(); // class_id → [(parent_name, file_path)]
    let mut class_ids: Vec<u64> = Vec::new();

    for parsed in parsed_files {
        for her in &parsed.heritage {
            // Find the child class symbol. When class_name is empty (parser limitation),
            // match by finding the class whose scope contains the heritage line.
            let child_sym = if !her.class_name.is_empty() {
                parsed.symbols.iter().find(|s| {
                    s.name == her.class_name
                        && matches!(s.kind,
                            crate::lang::CaptureTag::DefinitionClass |
                            crate::lang::CaptureTag::DefinitionInterface)
                })
            } else {
                parsed.symbols.iter().find(|s| {
                    if !matches!(s.kind,
                        crate::lang::CaptureTag::DefinitionClass |
                        crate::lang::CaptureTag::DefinitionInterface) {
                        return false;
                    }
                    if let Some(scope_id) = s.scope_id {
                        if let Some(scope) = indexes.scopes_by_id.get(&scope_id) {
                            return her.line >= scope.line_start && her.line <= scope.line_end;
                        }
                    }
                    her.line >= s.line && her.line <= s.line + 100
                })
            };

            if let Some(child_sym) = child_sym {
                parent_map.entry(child_sym.id)
                    .or_default()
                    .push((her.target_name.clone(), parsed.path.clone()));
                if !class_ids.contains(&child_sym.id) {
                    class_ids.push(child_sym.id);
                }
            }
        }
    }

    // For each class, resolve parent names to symbol IDs and compute MRO
    for class_id in &class_ids {
        let parents = parent_map.get(class_id).cloned().unwrap_or_default();
        let mut parent_ids: Vec<u64> = Vec::new();

        for (parent_name, from_file) in &parents {
            // Try to find the parent class in the same file first, then globally
            let mut found = false;
            for parsed in parsed_files {
                if let Some(parent_sym) = parsed.symbols.iter().find(|s| {
                    s.name == *parent_name && matches!(s.kind,
                        crate::lang::CaptureTag::DefinitionClass |
                        crate::lang::CaptureTag::DefinitionInterface)
                }) {
                    parent_ids.push(parent_sym.id);
                    found = true;
                    break;
                }
            }
            if !found {
                // Try from the importing file's perspective
                for parsed in parsed_files {
                    if parsed.path == *from_file {
                        if let Some(parent_sym) = parsed.symbols.iter().find(|s| {
                            s.name == *parent_name
                        }) {
                            parent_ids.push(parent_sym.id);
                            break;
                        }
                    }
                }
            }
        }

        // Compute C3 linearization
        let mut c3_parent_map: HashMap<String, Vec<String>> = HashMap::new();
        let class_id_str = class_id.to_string();
        let parent_id_strings: Vec<String> = parent_ids.iter().map(|id| id.to_string()).collect();
        c3_parent_map.insert(class_id_str.clone(), parent_id_strings);

        // Also add parent chains
        for pid in &parent_ids {
            if let Some(grandparents) = parent_map.get(pid) {
                let gp_strings: Vec<String> = grandparents.iter()
                    .map(|(name, _)| {
                        // Resolve name to ID
                        for parsed in parsed_files {
                            if let Some(gp_sym) = parsed.symbols.iter().find(|s| s.name == *name) {
                                return gp_sym.id.to_string();
                            }
                        }
                        name.clone()
                    })
                    .collect();
                c3_parent_map.insert(pid.to_string(), gp_strings);
            }
        }

        let mut cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
        if let Some(mro_names) = c3::c3_linearize(&class_id_str, &c3_parent_map, &mut cache) {
            let mro_ids: Vec<u64> = mro_names.iter()
                .filter_map(|name| name.parse::<u64>().ok())
                .collect();
            indexes.method_dispatch.insert(*class_id, mro_ids);
        }
    }
}

/// Extract reference sites from parsed files.
fn extract_reference_sites(parsed_files: &[ParsedFile]) -> Vec<ReferenceSite> {
    let mut sites = Vec::new();

    for parsed in parsed_files {
        for call in &parsed.calls {
            let kind = ReferenceKind::Call;
            let is_constructor = call.callee_name.chars().next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false) && call.receiver.is_none();

            sites.push(ReferenceSite {
                kind,
                name: call.callee_name.clone(),
                explicit_receiver: call.receiver.clone(),
                in_scope: call.caller_scope_id.unwrap_or(0),
                line: call.line,
                col: call.col,
                arity: None, // TODO: extract from AST
                is_constructor,
            });
        }

        // Also extract read/write sites from assignments
        for assign in &parsed.assignments {
            if let Some(ref receiver) = assign.receiver {
                sites.push(ReferenceSite {
                    kind: ReferenceKind::Read,
                    name: assign.name.clone(),
                    explicit_receiver: Some(receiver.clone()),
                    in_scope: 0, // TODO: resolve from scope
                    line: assign.line,
                    col: 0,
                    arity: None,
                    is_constructor: false,
                });
            }
        }
    }

    sites
}

/// Build graph node lookup from indexes.
fn build_node_lookup(indexes: &ScopeResolutionIndexes) -> GraphNodeLookup {
    let lookup = GraphNodeLookup::new();
    // In ATree, symbol IDs are the node IDs, so the lookup is identity.
    // We still index symbols for potential future use.
    for sym in indexes.symbols_by_id.values() {
        // Find the file path for this symbol
        if let Some(file_syms) = indexes.symbols_by_file.iter().find(|(_, syms)| {
            syms.iter().any(|s| s.id == sym.id)
        }) {
            let _ = lookup.get(file_syms.0, sym);
        }
    }
    lookup
}
