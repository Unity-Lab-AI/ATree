//! Scope-resolution orchestrator — ties all passes together.
//!
//! Equivalent to GitNexus's `runScopeResolution` in `pipeline/run.rs`.
//!
//! Parallelism strategy:
//!   Phase 1 (reconcile_ownership): per-file, embarrassingly parallel
//!   Phase 2 (build_indexes): per-file index shards built in parallel, then merged
//!   Phases 3-4 (workspace + MRO): sequential, read-only on built indexes
//!   Phase 5 (extract_reference_sites): per-file, embarrassingly parallel
//!   Phase 6 (build_node_lookup): sequential, fast
//!   Phase 7 (edge emission): per-reference-site, parallel chunks + merge

use crate::semantic::{ParsedFile, ScopeKind};
use crate::scope_resolution::{
    ScopeResolutionIndexes, BindingRef, BindingOrigin, TypeRef,
    ReferenceSite, ReferenceKind, ScopeResolutionStats,
};
use crate::scope_resolution::workspace_index::WorkspaceResolutionIndex;
use crate::scope_resolution::graph_bridge::{self, GraphEdge, GraphNodeLookup};
use crate::scope_resolution::receiver_bound::{self, ReceiverBoundProvider};
use crate::scope_resolution::compound_receiver;
use crate::scope_resolution::free_call;
use crate::scope_resolution::reference_resolver;
use crate::scope_resolution::reconcile_ownership;
use crate::resolver::c3;
use crate::resolver::import_graph::ImportGraph;
use rustc_hash::FxHashMap;
use rustc_hash::FxHashSet;
use std::collections::HashMap;

/// Heritage resolution result: (child_symbol_id, parent_name, parent_symbol_id, confidence)
pub type HeritageResolution = (u64, String, u64, f64);

/// Order files by dependency depth for optimal scope resolution.
pub fn order_files_by_dependency_depth(
    parsed_files: &[ParsedFile],
    import_graph: &ImportGraph,
) -> (Vec<String>, FxHashMap<String, i32>) {
    let entry_points: Vec<String> = parsed_files
        .iter()
        .filter(|pf| import_graph.imported_by(&pf.path).is_empty())
        .map(|pf| pf.path.clone())
        .collect();

    let depths = import_graph.compute_depths(&entry_points);

    let mut file_depths: Vec<(String, i32)> = parsed_files
        .iter()
        .map(|pf| {
            let d = depths.get(&pf.path).copied().unwrap_or(i32::MAX);
            (pf.path.clone(), d)
        })
        .collect();

    file_depths.sort_by_key(|(_, d)| *d);

    let ordered_paths: Vec<String> = file_depths.iter().map(|(p, _)| p.clone()).collect();
    let depth_map: FxHashMap<String, i32> = file_depths.into_iter().collect();

    (ordered_paths, depth_map)
}

/// Run the full scope-resolution pipeline on parsed files.
pub fn run_scope_resolution(
    parsed_files: &mut [ParsedFile],
    all_file_paths: &[String],
    import_graph: Option<&ImportGraph>,
) -> (ScopeResolutionStats, Vec<GraphEdge>, Vec<HeritageResolution>) {
    let mut stats = ScopeResolutionStats::default();
    stats.files_processed = parsed_files.len();

    if parsed_files.is_empty() {
        return (stats, Vec::new(), Vec::new());
    }

    let ordered_file_paths: Vec<String> = if let Some(ig) = import_graph {
        let (paths, _depths) = order_files_by_dependency_depth(parsed_files, ig);
        paths
    } else {
        all_file_paths.to_vec()
    };

    if !ordered_file_paths.is_empty() {
        let path_to_index: FxHashMap<&str, usize> = ordered_file_paths
            .iter()
            .enumerate()
            .map(|(i, p)| (p.as_str(), i))
            .collect();
        parsed_files.sort_by(|a, b| {
            let idx_a = path_to_index.get(a.path.as_str()).copied().unwrap_or(usize::MAX);
            let idx_b = path_to_index.get(b.path.as_str()).copied().unwrap_or(usize::MAX);
            idx_a.cmp(&idx_b)
        });
    }

    // Determine thread count
    let n_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(parsed_files.len())
        .max(1);

    let t0 = std::time::Instant::now();

    // Phase 1: Reconcile ownership — per-file, embarrassingly parallel
    parallel_reconcile_ownership(parsed_files, n_threads);
    eprintln!("[SR] Phase 1 (reconcile_ownership): {:.2}s", t0.elapsed().as_secs_f64());

    // Phase 2: Build ScopeResolutionIndexes — parallel shards + merge
    let t1 = std::time::Instant::now();
    let mut indexes = parallel_build_indexes(parsed_files, n_threads);
    eprintln!("[SR] Phase 2 (build_indexes): {:.2}s", t1.elapsed().as_secs_f64());

    // Phase 3: Build WorkspaceResolutionIndex (sequential)
    let t2 = std::time::Instant::now();
    let workspace_index = WorkspaceResolutionIndex::new(parsed_files, &indexes.scopes_by_id);
    eprintln!("[SR] Phase 3 (workspace_index): {:.2}s", t2.elapsed().as_secs_f64());

    // Phase 4: Build Method Dispatch (MRO)
    let t3 = std::time::Instant::now();
    let heritage_resolutions = build_method_dispatch(&mut indexes, parsed_files);
    eprintln!("[SR] Phase 4 (method_dispatch): {:.2}s", t3.elapsed().as_secs_f64());

    // Phase 4b: Emit MRO edges
    let mut mro_edges = Vec::new();
    emit_mro_edges(&indexes, &mut mro_edges);
    stats.reference_edges_emitted += mro_edges.len();

    // Phase 5: Extract reference sites — parallel
    let t4 = std::time::Instant::now();
    let reference_sites = parallel_extract_reference_sites(parsed_files, n_threads);
    eprintln!("[SR] Phase 5 (extract_ref_sites): {:.2}s ({} sites)", t4.elapsed().as_secs_f64(), reference_sites.len());

    // Phase 6: Build graph node lookup (sequential)
    let t5 = std::time::Instant::now();
    let node_lookup = build_node_lookup(&indexes);
    eprintln!("[SR] Phase 6 (node_lookup): {:.2}s", t5.elapsed().as_secs_f64());

    // Phase 7: Emit edges (load-bearing order per Contract Invariant I1)
    let mut edges = Vec::new();

    // Phase 5b: Registry-based reference resolution
    let t6 = std::time::Instant::now();
    let ref_ctx = reference_resolver::build_registry_context(&indexes);
    let mut resolved_sites = reference_resolver::build_reference_index_and_resolve(
        &indexes, &ref_ctx, &reference_sites, &mut edges,
    );
    stats.reference_edges_emitted += resolved_sites.len();
    eprintln!("[SR] Phase 6b (ref_resolver): {:.2}s ({} resolved)", t6.elapsed().as_secs_f64(), resolved_sites.len());

    // 6c: MRO edges
    edges.extend(mro_edges);

    // 7a: Receiver-bound calls FIRST (parallel)
    let t7 = std::time::Instant::now();
    let provider = ReceiverBoundProvider {
        is_super_receiver: |name| name == "super" || name == "base" || name == "super()",
        field_fallback: true,
        collapse_member_calls: false,
        hoist_type_bindings_to_module: false,
    };

    let receiver_emitted = parallel_receiver_bound_calls(
        &mut edges, &indexes, &reference_sites, &node_lookup,
        &provider, &workspace_index, &mut resolved_sites, n_threads,
    );
    stats.reference_edges_emitted += receiver_emitted;
    eprintln!("[SR] Phase 7a (receiver_bound): {:.2}s ({} edges)", t7.elapsed().as_secs_f64(), receiver_emitted);

    // 7b: Free-call fallback SECOND (parallel)
    let t8 = std::time::Instant::now();
    let exported_map = free_call::build_exported_def_map(&indexes);
    let free_emitted = parallel_free_call_fallback(
        &mut edges, &indexes, &reference_sites, &node_lookup,
        &workspace_index, &mut resolved_sites, &exported_map, n_threads,
    );
    stats.reference_edges_emitted += free_emitted;
    eprintln!("[SR] Phase 7b (free_call): {:.2}s ({} edges)", t8.elapsed().as_secs_f64(), free_emitted);

    // 7c: Import edges LAST (sequential — fast)
    let t9 = std::time::Instant::now();
    let import_emitted = graph_bridge::emit_import_edges(&mut edges, &indexes);
    stats.imports_emitted = import_emitted;
    eprintln!("[SR] Phase 7c (import_edges): {:.2}s", t9.elapsed().as_secs_f64());

    eprintln!("[SR] TOTAL: {:.2}s | {} files | {} ref sites | {} edges | {} threads",
        t0.elapsed().as_secs_f64(), parsed_files.len(), reference_sites.len(), edges.len(), n_threads);

    // Honest stats
    let total_sites = reference_sites.len();
    stats.resolved_sites = resolved_sites.len().min(total_sites);
    stats.unresolved_sites = total_sites - stats.resolved_sites;

    (stats, edges, heritage_resolutions)
}

// =====================================================================
// Parallel Phase 1: reconcile_ownership
// =====================================================================

fn parallel_reconcile_ownership(parsed_files: &mut [ParsedFile], n_threads: usize) {
    if n_threads <= 1 || parsed_files.len() < 100 {
        reconcile_ownership::reconcile_ownership(parsed_files);
        return;
    }
    let chunk_size = parsed_files.len().div_ceil(n_threads).max(1);
    std::thread::scope(|scope| {
        for chunk in parsed_files.chunks_mut(chunk_size) {
            scope.spawn(|| reconcile_ownership::reconcile_ownership(chunk));
        }
    });
}

// =====================================================================
// Parallel Phase 2: build_indexes
// =====================================================================

fn parallel_build_indexes(parsed_files: &[ParsedFile], n_threads: usize) -> ScopeResolutionIndexes {
    if n_threads <= 1 || parsed_files.len() < 100 {
        return build_indexes_single(parsed_files);
    }

    let chunk_size = parsed_files.len().div_ceil(n_threads).max(1);
    let shards: Vec<ScopeResolutionIndexes> = std::thread::scope(|scope| {
        let handles: Vec<_> = parsed_files
            .chunks(chunk_size)
            .map(|chunk| scope.spawn(move || build_indexes_single(chunk)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut merged = ScopeResolutionIndexes::new();
    for shard in shards {
        merged.merge(shard);
    }
    merged
}

fn build_indexes_single(parsed_files: &[ParsedFile]) -> ScopeResolutionIndexes {
    let mut indexes = ScopeResolutionIndexes::new();

    for parsed in parsed_files {
        for scope in &parsed.scopes {
            indexes.scopes_by_id.insert(scope.id, scope.clone());
            indexes.scopes_by_file
                .entry(parsed.path.clone())
                .or_default()
                .push(scope.id);
        }

        for sym in &parsed.symbols {
            indexes.symbols_by_id.insert(sym.id, sym.clone());
            indexes.symbols_by_file
                .entry(parsed.path.clone())
                .or_default()
                .push(sym.clone());
        }

        for imp in &parsed.imports {
            let resolved_symbol_id = parsed.symbols.iter()
                .find(|s| s.name == imp.local_name)
                .map(|s| s.id)
                .unwrap_or(0);

            let binding = BindingRef {
                def_node_id: resolved_symbol_id,
                name: imp.local_name.clone(),
                origin: BindingOrigin::Import,
                def_file_path: imp.source.clone(),
                confidence: imp.confidence.score(),
            };

            if let Some(module_scope) = parsed.scopes.iter().find(|s| s.kind == ScopeKind::Module) {
                indexes.bindings
                    .entry(module_scope.id)
                    .or_default()
                    .entry(imp.local_name.clone())
                    .or_default()
                    .push(binding);
            }
        }

        for sym in &parsed.symbols {
            if let Some(scope_id) = sym.scope_id {
                let binding = BindingRef {
                    def_node_id: sym.id,
                    name: sym.name.clone(),
                    origin: BindingOrigin::Local,
                    def_file_path: parsed.path.clone(),
                    confidence: 1.0,
                };
                indexes.bindings
                    .entry(scope_id)
                    .or_default()
                    .entry(sym.name.clone())
                    .or_default()
                    .push(binding);
            }
        }

        for assign in &parsed.assignments {
            let type_ref = TypeRef {
                raw_name: assign.name.clone(),
                declared_at_scope: 0,
            };
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

        for tb in &parsed.type_bindings {
            let mut best_scope_id = 0u64;
            let mut best_size = usize::MAX;
            for scope in &parsed.scopes {
                if tb.line >= scope.line_start && tb.line <= scope.line_end {
                    let size = scope.line_end - scope.line_start;
                    if size < best_size {
                        best_scope_id = scope.id;
                        best_size = size;
                    }
                }
            }
            let type_ref = crate::scope_resolution::TypeRef {
                raw_name: tb.type_text.clone(),
                declared_at_scope: best_scope_id,
            };
            indexes.type_bindings
                .entry(best_scope_id)
                .or_default()
                .entry(tb.var_name.clone())
                .or_insert(type_ref);
        }

        indexes.parsed_files.push(parsed.clone());
        indexes.imports.extend(parsed.imports.clone());
    }

    indexes
}

// =====================================================================
// Parallel Phase 7a: receiver-bound calls — shard reference_sites across threads
// =====================================================================

fn parallel_receiver_bound_calls(
    all_edges: &mut Vec<GraphEdge>,
    indexes: &ScopeResolutionIndexes,
    reference_sites: &[ReferenceSite],
    node_lookup: &GraphNodeLookup,
    provider: &ReceiverBoundProvider,
    workspace_index: &WorkspaceResolutionIndex,
    all_resolved: &mut FxHashSet<String>,
    n_threads: usize,
) -> usize {
    if n_threads <= 1 || reference_sites.len() < 500 {
        return receiver_bound::emit_receiver_bound_calls(
            all_edges, indexes, reference_sites, node_lookup,
            provider, workspace_index, all_resolved,
        );
    }

    let chunk_size = reference_sites.len().div_ceil(n_threads).max(1);
    let results: Vec<(Vec<GraphEdge>, FxHashSet<String>, usize)> = std::thread::scope(|scope| {
        let handles: Vec<_> = reference_sites
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    let mut local_edges = Vec::new();
                    let mut local_seen = FxHashSet::default();
                    let mut local_resolved = FxHashSet::default();
                    let mut emitted = 0usize;

                    let compound_opts = compound_receiver::CompoundReceiverOpts {
                        field_fallback: provider.field_fallback,
                        hoist_type_bindings_to_module: provider.hoist_type_bindings_to_module,
                    };

                    for site in chunk {
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

                        let site_key = format!("{}:{}:{}", site.in_scope, site.line, site.col);

                        // ── super branch
                        if (provider.is_super_receiver)(receiver_name) {
                            if let Some(enclosing_class) = crate::scope_resolution::walkers::find_enclosing_class_def(indexes, site.in_scope) {
                                if let Some(mro) = indexes.method_dispatch.get(&enclosing_class.id) {
                                    for ancestor_id in mro {
                                        if let Some(member) = indexes.find_owned_member(*ancestor_id, &site.name) {
                                            let ok = graph_bridge::try_emit_edge(
                                                &mut local_edges, indexes, node_lookup, site, member,
                                                "global", &mut local_seen, 0.85, provider.collapse_member_calls,
                                                None,
                                            );
                                            if ok { emitted += 1; local_resolved.insert(site_key.clone()); }
                                            break;
                                        }
                                    }
                                }
                            }
                            continue;
                        }

                        // ── Case 0: compound receiver
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
                                            &mut local_edges, indexes, node_lookup, site, member,
                                            "global", &mut local_seen, 0.85, provider.collapse_member_calls,
                                            None,
                                        );
                                        if ok { emitted += 1; local_resolved.insert(site_key.clone()); }
                                        break;
                                    }
                                }
                                continue;
                            }
                        }

                        // ── Case 2: class-name receiver
                        if let Some(class_def) = crate::scope_resolution::walkers::find_class_binding_in_scope(indexes, site.in_scope, receiver_name) {
                            let mut chain = vec![class_def.id];
                            if let Some(mro) = indexes.method_dispatch.get(&class_def.id) {
                                chain.extend(mro);
                            }
                            let mut found = false;
                            for owner_id in &chain {
                                if let Some(member) = indexes.find_owned_member(*owner_id, &site.name) {
                                    let ok = graph_bridge::try_emit_edge(
                                        &mut local_edges, indexes, node_lookup, site, member,
                                        "global", &mut local_seen, 1.0, provider.collapse_member_calls,
                                        None,
                                    );
                                    if ok { emitted += 1; local_resolved.insert(site_key.clone()); }
                                    found = true;
                                    break;
                                }
                            }
                            if found { continue; }
                        }

                        // ── Case 3: variable receiver
                        if let Some(var_sym) = crate::scope_resolution::walkers::find_callable_binding_in_scope(indexes, site.in_scope, receiver_name) {
                            let ok = graph_bridge::try_emit_edge(
                                &mut local_edges, indexes, node_lookup, site, var_sym,
                                "global", &mut local_seen, 0.7, provider.collapse_member_calls,
                                None,
                            );
                            if ok { emitted += 1; local_resolved.insert(site_key); }
                        }
                    }
                    (local_edges, local_resolved, emitted)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut total_emitted = 0;
    for (edges, resolved, emitted) in results {
        all_edges.extend(edges);
        all_resolved.extend(resolved);
        total_emitted += emitted;
    }
    total_emitted
}

// =====================================================================
// Parallel Phase 7b: free-call fallback — shard reference_sites across threads
// =====================================================================

fn parallel_free_call_fallback(
    all_edges: &mut Vec<GraphEdge>,
    indexes: &ScopeResolutionIndexes,
    reference_sites: &[ReferenceSite],
    node_lookup: &GraphNodeLookup,
    _workspace_index: &WorkspaceResolutionIndex,
    all_resolved: &mut FxHashSet<String>,
    exported_map: &free_call::ExportedDefMap,
    n_threads: usize,
) -> usize {
    if n_threads <= 1 || reference_sites.len() < 500 {
        return free_call::emit_free_call_fallback(
            all_edges, indexes, reference_sites, node_lookup,
            _workspace_index, all_resolved,
        );
    }

    let chunk_size = reference_sites.len().div_ceil(n_threads).max(1);
    let results: Vec<(Vec<GraphEdge>, FxHashSet<String>, usize)> = std::thread::scope(|scope| {
        let handles: Vec<_> = reference_sites
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    let mut local_edges = Vec::new();
                    let mut seen = FxHashSet::default();
                    let mut local_resolved = FxHashSet::default();
                    let mut emitted = 0usize;

                    for site in chunk {
                        if site.kind != ReferenceKind::Call {
                            continue;
                        }
                        if site.explicit_receiver.is_some() {
                            continue;
                        }

                        let site_key = format!("{}:{}:{}", site.in_scope, site.line, site.col);

                        let target_def = if site.is_constructor {
                            crate::scope_resolution::walkers::find_class_binding_in_scope(indexes, site.in_scope, &site.name)
                        } else {
                            crate::scope_resolution::walkers::find_callable_binding_in_scope(indexes, site.in_scope, &site.name)
                                .or_else(|| exported_map.get(site.name.as_str()).copied())
                        };

                        let target_def = match target_def {
                            Some(d) => d,
                            None => continue,
                        };

                        let ok = graph_bridge::try_emit_edge(
                            &mut local_edges, indexes, node_lookup, site, target_def,
                            "local-call", &mut seen, 0.85, false, None,
                        );
                        if ok {
                            emitted += 1;
                            local_resolved.insert(site_key);
                        }
                    }
                    (local_edges, local_resolved, emitted)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut total_emitted = 0;
    for (edges, resolved, emitted) in results {
        all_edges.extend(edges);
        all_resolved.extend(resolved);
        total_emitted += emitted;
    }
    total_emitted
}

// =====================================================================
// Parallel Phase 5: extract_reference_sites
// =====================================================================

fn parallel_extract_reference_sites(parsed_files: &[ParsedFile], n_threads: usize) -> Vec<ReferenceSite> {
    if n_threads <= 1 || parsed_files.len() < 100 {
        return extract_reference_sites_single(parsed_files);
    }

    let chunk_size = parsed_files.len().div_ceil(n_threads).max(1);
    let shards: Vec<Vec<ReferenceSite>> = std::thread::scope(|scope| {
        let handles: Vec<_> = parsed_files
            .chunks(chunk_size)
            .map(|chunk| scope.spawn(move || extract_reference_sites_single(chunk)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut sites = Vec::new();
    for shard in shards {
        sites.extend(shard);
    }
    sites
}

fn extract_reference_sites_single(parsed_files: &[ParsedFile]) -> Vec<ReferenceSite> {
    let mut sites = Vec::new();

    for parsed in parsed_files {
        for call in &parsed.calls {
            let is_constructor = call.callee_name.chars().next()
                .map(|c| c.is_uppercase())
                .unwrap_or(false) && call.receiver.is_none();

            sites.push(ReferenceSite {
                kind: ReferenceKind::Call,
                name: call.callee_name.clone(),
                explicit_receiver: call.receiver.clone(),
                in_scope: call.caller_scope_id.unwrap_or(0),
                line: call.line,
                col: call.col,
                arity: None,
                is_constructor,
            });
        }

        for assign in &parsed.assignments {
            if let Some(ref receiver) = assign.receiver {
                sites.push(ReferenceSite {
                    kind: ReferenceKind::Read,
                    name: assign.name.clone(),
                    explicit_receiver: Some(receiver.clone()),
                    in_scope: 0,
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

// =====================================================================
// Sequential helpers (unchanged)
// =====================================================================

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

fn build_method_dispatch(
    indexes: &mut ScopeResolutionIndexes,
    parsed_files: &[ParsedFile],
) -> Vec<HeritageResolution> {
    let mut resolutions = Vec::new();
    let mut parent_map: HashMap<u64, Vec<(String, String)>> = HashMap::new();
    let mut class_ids: Vec<u64> = Vec::new();

    for parsed in parsed_files {
        for her in &parsed.heritage {
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

    for class_id in &class_ids {
        let parents = parent_map.get(class_id).cloned().unwrap_or_default();
        let mut parent_ids: Vec<u64> = Vec::new();

        for (parent_name, from_file) in &parents {
            let mut found = false;
            for parsed in parsed_files {
                if let Some(parent_sym) = parsed.symbols.iter().find(|s| {
                    s.name == *parent_name && matches!(s.kind,
                        crate::lang::CaptureTag::DefinitionClass |
                        crate::lang::CaptureTag::DefinitionInterface)
                }) {
                    parent_ids.push(parent_sym.id);
                    resolutions.push((*class_id, parent_name.clone(), parent_sym.id, 1.0));
                    found = true;
                    break;
                }
            }
            if !found {
                for parsed in parsed_files {
                    if parsed.path == *from_file {
                        if let Some(parent_sym) = parsed.symbols.iter().find(|s| {
                            s.name == *parent_name
                        }) {
                            parent_ids.push(parent_sym.id);
                            resolutions.push((*class_id, parent_name.clone(), parent_sym.id, 0.9));
                            break;
                        }
                    }
                }
            }
        }

        let mut c3_parent_map: HashMap<String, Vec<String>> = HashMap::new();
        let class_id_str = class_id.to_string();
        let parent_id_strings: Vec<String> = parent_ids.iter().map(|id| id.to_string()).collect();
        c3_parent_map.insert(class_id_str.clone(), parent_id_strings);

        for pid in &parent_ids {
            if let Some(grandparents) = parent_map.get(pid) {
                let gp_strings: Vec<String> = grandparents.iter()
                    .map(|(name, _)| {
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

    resolutions
}

fn build_node_lookup(indexes: &ScopeResolutionIndexes) -> GraphNodeLookup {
    let lookup = GraphNodeLookup::new();
    for (file_path, syms) in &indexes.symbols_by_file {
        for sym in syms {
            let _ = lookup.get(file_path, sym);
        }
    }
    lookup
}
