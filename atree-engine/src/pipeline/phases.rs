//! Pipeline phase implementations.
//!
//! Each phase is a struct implementing `PipelinePhase`. Phases are
//! organized in dependency order matching GitNexusRelay's 12-phase DAG.
//!
//! The scan and parse phases are handled by `build_graph()` in `src/lib.rs`
//! (work-stealing filesystem scan + parallel tree-sitter parse). The pipeline
//! starts at `cross_file` with pre-populated `ParsedFile` data.
//!
//! Phase dependency graph:
//! ```text
//!   cross_file → [routes, tools, orm, markdown, cobol, scope_resolution, mro]
//!     → [communities, processes]
//! ```
//!
//! Each phase writes its results into `PipelineSharedState` so downstream
//! phases can read them. The `PhaseResult::output` is a lightweight status
//! struct for dependency tracking.

use super::*;
use crate::perf_timer;
use rusqlite::params;
use rustc_hash::FxHashMap;

// ── Phase: cross_file ───────────────────────────────────────────────────────

pub struct CrossFileOutput {
    pub total_files: usize,
    pub resolved_calls: usize,
    pub resolved_imports: usize,
}

pub struct CrossFilePhase;

impl PipelinePhase for CrossFilePhase {
    fn name(&self) -> &str { "cross_file" }
    fn deps(&self) -> &[&str] { &[] }

    fn execute(&self, ctx: &PipelineContext, shared: &PipelineSharedState, _deps: &FxHashMap<PhaseName, &PhaseResult>) -> Box<dyn Any + Send + Sync> {
        let mut parsed_files_guard = shared.parsed_files.lock().unwrap_or_else(|e| e.into_inner());
        let total_files = parsed_files_guard.len();
        if parsed_files_guard.is_empty() {
            return Box::new(CrossFileOutput { total_files: 0, resolved_calls: 0, resolved_imports: 0 });
        }

        (ctx.on_progress)(PipelineProgress {
            phase: "cross_file".to_string(),
            percent: 50,
            message: "Building graph store...".to_string(),
            detail: None,
            stats: None,
        });

        // Open or create the graph store.
        let store = match ctx.db_path {
            Some(ref path) => crate::store::GraphStore::open(path),
            None => crate::store::GraphStore::open_in_memory(),
        };

        let store = match store {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[cross_file] Warning: failed to open graph store: {}", e);
                return Box::new(CrossFileOutput { total_files, resolved_calls: 0, resolved_imports: 0 });
            }
        };

        let conn = store.conn();

        // Delete old data for all files being re-indexed to prevent duplicates
        // and ensure visibility/edge data stays in sync with the latest parse.
        for pf in parsed_files_guard.iter() {
            let file_id: i64 = conn.query_row(
                "SELECT id FROM files WHERE path = ?1", [&pf.path], |r| r.get(0)
            ).unwrap_or(0);
            if file_id > 0 {
                let _ = conn.execute("DELETE FROM exports WHERE file_id = ?1", [file_id]);
                let _ = conn.execute("DELETE FROM calls WHERE file_id = ?1", [file_id]);
                let _ = conn.execute("DELETE FROM heritage WHERE file_id = ?1", [file_id]);
                let _ = conn.execute("DELETE FROM imports WHERE file_id = ?1", [file_id]);
                let _ = conn.execute("DELETE FROM edges WHERE src_id IN (SELECT id FROM symbols WHERE file_id = ?1) OR dst_id IN (SELECT id FROM symbols WHERE file_id = ?1)", [file_id]);
                let _ = conn.execute("DELETE FROM symbols WHERE file_id = ?1", [file_id]);
            }
        }

        // Batch-insert all parsed files (old data cleaned above).
        perf_timer!("SQLite batch insert");
        let global_symbol_id_map = match store.insert_all_files_batch(&parsed_files_guard, ctx.repo_label.as_deref()) {
            Ok(map) => map,
            Err(e) => {
                eprintln!("[cross_file] Warning: batch insert failed: {}", e);
                rustc_hash::FxHashMap::default()
            }
        };
        let resolved_imports: usize = parsed_files_guard.iter().map(|f| f.imports.len()).sum();
        let all_file_paths: Vec<String> = parsed_files_guard.iter().map(|f| f.path.clone()).collect();

        // Build the import graph for A*-guided file ordering in scope resolution.
        // This needs &parsed_files_guard, not &mut, so we do it while the lock is held.
        let import_graph = crate::resolver::import_graph::ImportGraph::from_parsed_files(&parsed_files_guard);

        // ── Evidence Lifecycle (Stages 1-4) ──────────────────────────────────
        // Collect evidence from all parsed files, dedupe, enrich, calibrate, commit.
        // This runs while we still have the parsed_files lock (read-only access to evidence).
        (ctx.on_progress)(PipelineProgress {
            phase: "cross_file".to_string(),
            percent: 52,
            message: "Extracting evidence...".to_string(),
            detail: None,
            stats: None,
        });

        perf_timer!("Evidence lifecycle");
        {
            let mut evidence_lifecycle = crate::evidence::lifecycle::EvidenceLifecycle::new();

            // Collect all evidence candidates from all parsed files.
            // Pre-allocate with estimated capacity to avoid reallocations.
            let _t0 = std::time::Instant::now();
            let total_evidence_estimate: usize = parsed_files_guard.iter().map(|pf| pf.evidence.len()).sum();
            let mut all_candidates = Vec::with_capacity(total_evidence_estimate);
            for pf in parsed_files_guard.iter() {
                all_candidates.extend(pf.evidence.iter().cloned());
            }
            let _total_candidates = all_candidates.len();
            tracing::debug!(total_evidence_estimate, "Evidence candidates collected");

            // Stage 1: Normalize.
            let _t1 = std::time::Instant::now();
            let evidence: Vec<crate::evidence::Evidence> = all_candidates
                .into_iter()
                .map(|c| c.into_evidence())
                .collect();
            evidence_lifecycle.normalize(evidence);

            // Stage 2: Deduplicate.
            let _t2 = std::time::Instant::now();
            let (_total, _merged) = evidence_lifecycle.dedupe();

            // Stage 3: Enrich.
            let _t3 = std::time::Instant::now();
            let file_id_map: rustc_hash::FxHashMap<u64, i64> = parsed_files_guard
                .iter()
                .filter_map(|pf| {
                    let file_db_id = store.get_file_by_id(pf.id as i64).ok().flatten().map(|f| f.id)?;
                    Some((pf.id, file_db_id))
                })
                .collect();
            evidence_lifecycle.enrich(&global_symbol_id_map, &file_id_map);

            // Stage 4: Calibrate.
            let _t4 = std::time::Instant::now();
            evidence_lifecycle.calibrate_all();

            // Stage 5: Commit + persist.
            let t5 = std::time::Instant::now();
            let _committed_count = evidence_lifecycle.commit_all();
            let committed_evidence: Vec<&crate::evidence::Evidence> = evidence_lifecycle
                .by_state(crate::evidence::lifecycle::EvidenceState::Committed);
            if !committed_evidence.is_empty() {
                let ev_store = crate::evidence::storage::EvidenceStore::new(store.conn());
                let owned: Vec<crate::evidence::Evidence> = committed_evidence.iter().map(|ev| (*ev).clone()).collect();
                match ev_store.insert_batch(&owned) {
                    Ok(n) => eprintln!("[PERF] Evidence persist: {:.2}s ({} records)", t5.elapsed().as_secs_f64(), n),
                    Err(e) => eprintln!("[PERF] Evidence persist failed: {}", e),
                }
            } else {
            }
        }

        // ── Type Environment Enrichment (Tier 1 + Tier 2) ──────────────────
        // Before scope resolution, enrich ParsedFile.type_bindings with
        // inferred types from constructor calls (Tier 1) and assignment
        // chain propagation (Tier 2). This gives scope resolution richer
        // type information for call resolution.
        (ctx.on_progress)(PipelineProgress {
            phase: "cross_file".to_string(),
            percent: 54,
            message: "Enriching type environments...".to_string(),
            detail: None,
            stats: None,
        });

        let type_envs = crate::type_env::build_type_envs(&parsed_files_guard);
        let type_bindings_added: usize = type_envs.values()
            .map(|e| e.bindings.values().map(|m| m.len()).sum::<usize>())
            .sum();
        if type_bindings_added > 0 {
            tracing::info!(type_bindings_added, "Type environment enrichment");
            // Merge enriched type bindings into parsed files so scope resolution
            // can use them during call resolution.
            for parsed in parsed_files_guard.iter_mut() {
                if let Some(env) = type_envs.get(&parsed.id) {
                    for (scope_key, name_map) in &env.bindings {
                        for (var_name, type_text) in name_map {
                            // Only add if not already present (Tier 0 takes precedence).
                            let already_has = parsed.type_bindings.iter()
                                .any(|tb| tb.var_name == *var_name && tb.line > 0);
                            if !already_has {
                                // Extract line from scope_key (format: "file_id:line_start")
                                let line = scope_key.split(':')
                                    .nth(1)
                                    .and_then(|s| s.parse().ok())
                                    .unwrap_or(0);
                                parsed.type_bindings.push(crate::syntax::TypeBinding {
                                    var_name: var_name.clone(),
                                    type_text: type_text.clone(),
                                    line,
                                    owner_kind: "type_env_inferred".to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // ── Cross-file type resolution ──────────────────────────────────────
        // Build a CrossFileTypeResolver that links type environments across files
        // using the import graph. This enables resolving `this.repo.find()` where
        // `repo` is typed as `UserRepository` imported from another file.
        perf_timer!("Cross-file type resolution");
        {
            use rustc_hash::FxHashMap;
            let mut file_id_to_path: FxHashMap<u64, String> = FxHashMap::default();
            let mut path_to_file_id: FxHashMap<String, u64> = FxHashMap::default();
            for pf in parsed_files_guard.iter() {
                file_id_to_path.insert(pf.id, pf.path.clone());
                path_to_file_id.insert(pf.path.clone(), pf.id);
            }
            let cross_resolver = crate::type_env::build_cross_file_resolver(
                &parsed_files_guard, &file_id_to_path, &path_to_file_id,
            );
            let cross_type_resolved = cross_resolver.resolve_all(&parsed_files_guard);
            if cross_type_resolved > 0 {
                tracing::info!(cross_type_resolved, "Cross-file type resolutions");
            }
        }

        // Drop the lock before scope resolution, which needs &mut.
        drop(parsed_files_guard);

        // Run scope-resolution pipeline (needs &mut parsed_files).
        (ctx.on_progress)(PipelineProgress {
            phase: "cross_file".to_string(),
            percent: 55,
            message: "Resolving cross-file references...".to_string(),
            detail: None,
            stats: None,
        });

        perf_timer!("Scope resolution (cross-file)");
        let mut parsed_files_guard = shared.parsed_files.lock().unwrap_or_else(|e| e.into_inner());
        let (sr_stats, sr_edges, heritage_resolutions) =
            crate::scope_resolution::orchestrator::run_scope_resolution(
                &mut parsed_files_guard,
                &all_file_paths,
                Some(&import_graph),
            );

        // Batch-persist scope-resolution edges in a single transaction.
        let sr_edge_records: Vec<crate::store::EdgeRecord> = sr_edges.iter()
            .filter_map(|edge| {
                let src_db_id = global_symbol_id_map.get(&{ edge.source_id }).copied().unwrap_or(0);
                let dst_db_id = global_symbol_id_map.get(&{ edge.target_id }).copied().unwrap_or(0);
                if src_db_id == 0 || dst_db_id == 0 { return None; }
                let file_id = store.get_file_id_for_symbol(src_db_id).unwrap_or(None).unwrap_or(0);
                Some(crate::store::EdgeRecord {
                    id: 0, src_id: src_db_id, dst_id: dst_db_id,
                    edge_kind: edge.edge_type.clone(),
                    confidence: edge.confidence,
                    file_id: Some(file_id),
                    line: 0,
                })
            })
            .collect();

        perf_timer!("Edge persistence (scope-res)");
        if let Err(e) = store.insert_edges_batch(&sr_edge_records) {
            eprintln!("[cross_file] Warning: edge batch insert failed: {}", e);
        }

        // Persist heritage resolutions.
        for (child_id, parent_name, parent_id, confidence) in &heritage_resolutions {
            let child_db_id = global_symbol_id_map.get(child_id).copied().unwrap_or(0);
            let parent_db_id = global_symbol_id_map.get(parent_id).copied().unwrap_or(0);
            if child_db_id == 0 || parent_db_id == 0 { continue; }
            if let Err(e) = store.update_heritage_parent(child_db_id, parent_name, parent_db_id, *confidence) {
                eprintln!("[cross_file] Warning: {}", e);
            }
        }

        let resolved_calls = sr_stats.resolved_sites;

        // ── Call Resolution (populate calls.resolved_symbol_id) ────────────────
        // The scope resolution pipeline emits GraphEdges directly to the edges table,
        // but the calls table's resolved_symbol_id is never populated during cold indexing.
        // The ResolutionEngine handles this: it walks all unresolved calls, resolves them
        // against the symbol table (same-file → import-scoped → receiver → global fallback),
        // and writes back resolved_symbol_id + confidence to the calls table.
        perf_timer!("Call resolution (ResolutionEngine)");
        let call_resolver = crate::resolver::ResolutionEngine::new(&store);
        match call_resolver {
            Ok(engine) => {
                match engine.run_full_resolution() {
                    Ok(stats) => {
                        eprintln!("[cross_file] Resolution: {} calls, {} imports, {} MRO, {} defines",
                            stats.calls_resolved, stats.imports_resolved, stats.mro_edges, stats.defines_edges);
                    }
                    Err(e) => eprintln!("[cross_file] Warning: call resolution failed: {}", e),
                }
            }
            Err(e) => eprintln!("[cross_file] Warning: failed to build resolution engine: {}", e),
        }

        // ── Data Flow Analysis (value propagation tracking) ───────────────────
        // Extract data flow edges from assignments, parameter passing, returns,
        // and property access patterns. This complements the call graph with
        // a value-flow graph: "where does this variable's value come from/go to?"
        perf_timer!("Data flow analysis");
        {
            let mut total_flows = 0usize;

            // Build a map from file_id → parsed_file for quick lookup
            let all_files = store.get_all_files().unwrap_or_default();
            for file_rec in &all_files {
                let file_id = file_rec.id;
                // Find the matching parsed file
                if let Some(pf) = parsed_files_guard.iter().find(|pf| {
                    file_rec.path.ends_with(&pf.path) || pf.path == file_rec.path
                }) {
                    let file_symbols = store.get_symbols_by_file(file_id).unwrap_or_default();
                    match crate::data_flow::extract_and_store_flows(
                        &store, file_id,
                        &pf.assignments, &pf.calls, &pf.type_bindings,
                        &file_symbols, &global_symbol_id_map,
                    ) {
                        Ok(count) => total_flows += count,
                        Err(e) => eprintln!("[cross_file] Warning: data flow for {}: {}", pf.path, e),
                    }
                }
            }
            eprintln!("[cross_file] Data flows: {} edges extracted", total_flows);
        }

        // ── Pattern Mining + Constraint Synthesis (Layers 2-3) ────────────────
        // Mine patterns from committed evidence, synthesize constraints.
        (ctx.on_progress)(PipelineProgress {
            phase: "cross_file".to_string(),
            percent: 58,
            message: "Mining patterns and synthesizing constraints...".to_string(),
            detail: None,
            stats: None,
        });
        perf_timer!("Pattern mining + constraint synthesis");
        {
            // Re-open store reference for evidence querying.
            // (store is moved into shared below, so we use it first).
            let ev_store = crate::evidence::storage::EvidenceStore::new(store.conn());

            // Fetch ALL committed evidence across all kinds for pattern mining.
            let all_kinds = [
                crate::evidence::EvidenceKind::SymbolDeclaration,
                crate::evidence::EvidenceKind::FunctionCall,
                crate::evidence::EvidenceKind::ImportEdge,
                crate::evidence::EvidenceKind::TypeRelation,
                crate::evidence::EvidenceKind::DataFlow,
            ];
            let all_evidence_records: Vec<crate::evidence::storage::EvidenceRecord> = all_kinds
                .iter()
                .filter_map(|k| ev_store.by_kind(*k).ok())
                .flatten()
                .collect();

            if !all_evidence_records.is_empty() {
                let pattern_config = crate::patterns::PatternMiningConfig::default();
                let constraint_config = crate::constraints::ConstraintSynthesisConfig::default();

                // Convert records to Evidence for mining.
                let mining_evidence: Vec<crate::evidence::Evidence> = all_evidence_records
                    .iter()
                    .map(|rec| record_to_evidence(rec.clone()))
                    .collect();

                let patterns = crate::patterns::mine_patterns(&mining_evidence, &pattern_config);
                let constraints = crate::constraints::synthesize_constraints(
                    &mining_evidence, &patterns, &constraint_config,
                );

                // Persist patterns to SQLite.
                if !patterns.is_empty() {
                    let pat_store = crate::patterns::PatternStore::new(store.conn());
                    if let Err(e) = pat_store.init_tables() {
                        eprintln!("[patterns] Warning: failed to init pattern tables: {}", e);
                    }
                    for pat in &patterns {
                        let _ = store.conn().execute(
                            "INSERT OR REPLACE INTO patterns (id, name, description, motif, frequency, dispersion, overall_score) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            rusqlite::params![
                                &pat.id, &pat.name, &pat.description,
                                &format!("{:?}", pat.motif),
                                pat.score.frequency as i64,
                                pat.score.dispersion, pat.score.overall,
                            ],
                        );
                    }
                    log::info!("[patterns] Mined and persisted {} patterns", patterns.len());
                }

                // Persist constraints to SQLite.
                if !constraints.is_empty() {
                    let con_store = crate::constraints::ConstraintStore::new(store.conn());
                    if let Err(e) = con_store.init_tables() {
                        eprintln!("[constraints] Warning: failed to init constraint tables: {}", e);
                    }
                    for con in &constraints {
                        let _ = store.conn().execute(
                            "INSERT OR REPLACE INTO constraints (id, name, description, kind, confidence, active) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            rusqlite::params![
                                &con.id, &con.name, &con.description,
                                con.kind.as_str(), con.confidence,
                                if con.active { 1i64 } else { 0 },
                            ],
                        );
                    }
                    log::info!("[constraints] Synthesized and persisted {} constraints", constraints.len());
                }
            }
        }

        // Store the graph store, symbol ID map, and scope resolution stats.
        *shared.store.lock().unwrap_or_else(|e| e.into_inner()) = Some(store);
        *shared.symbol_id_map.lock().unwrap_or_else(|e| e.into_inner()) = global_symbol_id_map;
        *shared.scope_resolution_stats.lock().unwrap_or_else(|e| e.into_inner()) = Some(sr_stats);
        drop(parsed_files_guard);

        (ctx.on_progress)(PipelineProgress {
            phase: "cross_file".to_string(),
            percent: 60,
            message: format!("Resolved {} calls, {} imports", resolved_calls, resolved_imports),
            detail: None,
            stats: None,
        });

        Box::new(CrossFileOutput {
            total_files,
            resolved_calls,
            resolved_imports,
        })
    }
}

/// Convert an EvidenceRecord (flat DB row) back to an Evidence for pattern mining.
/// This is a lossy conversion — graph links are not restored from the flat record.
/// For production pattern mining, operate directly on Evidence in memory before commit.
pub fn record_to_evidence(rec: crate::evidence::storage::EvidenceRecord) -> crate::evidence::Evidence {
    use crate::evidence::*;
    let imports: Vec<String> = serde_json::from_str(&rec.imports).unwrap_or_default();
    let scope_chain: Vec<String> = serde_json::from_str(&rec.scope_chain).unwrap_or_default();
    let tags: Vec<String> = serde_json::from_str(&rec.tags).unwrap_or_default();
    let kind = rec.kind.parse().unwrap_or(EvidenceKind::HeuristicInference);
    let target_type = match rec.target_type.as_str() {
        "primitive" => TargetType::Primitive,
        "pattern" => TargetType::Pattern,
        "constraint" => TargetType::Constraint,
        _ => TargetType::Symbol,
    };
    let state = rec.state.parse().unwrap_or(crate::evidence::EvidenceState::Committed);
    Evidence {
        id: EvidenceId(rec.id),
        kind,
        source: EvidenceSource {
            file: rec.file,
            span: SourceSpan {
                start_line: rec.start_line,
                start_col: rec.start_col,
                end_line: rec.end_line,
                end_col: rec.end_col,
            },
            language: rec.language,
        },
        target: EvidenceTarget { target_type, ref_id: rec.target_ref },
        content: EvidenceContent { raw: rec.raw, normalized: rec.normalized },
        context: EvidenceContext {
            enclosing_symbol: rec.enclosing_symbol,
            imports,
            scope_chain,
        },
        metadata: EvidenceMetadata {
            extractor: rec.extractor,
            confidence: rec.confidence,
            stability: rec.stability,
            entropy: rec.entropy,
            timestamp_ms: rec.timestamp_ms,
            commit: rec.commit,
        },
        links: EvidenceLinks::default(),
        tags,
        state,
    }
}

// ── Phase: mro ──────────────────────────────────────────────────────────────

pub struct MroOutput {
    pub total_files: usize,
    pub mro_chains_computed: usize,
}

pub struct MroPhase;

impl PipelinePhase for MroPhase {
    fn name(&self) -> &str { "mro" }
    fn deps(&self) -> &[&str] { &["cross_file"] }
    // MRO is a no-op count query — exists for DAG compatibility.

    fn execute(&self, ctx: &PipelineContext, shared: &PipelineSharedState, deps: &FxHashMap<PhaseName, &PhaseResult>) -> Box<dyn Any + Send + Sync> {
        let cross = get_phase_output::<CrossFileOutput>(deps, "cross_file");

        (ctx.on_progress)(PipelineProgress {
            phase: "mro".to_string(),
            percent: 65,
            message: "Computing MRO...".to_string(),
            detail: None,
            stats: None,
        });

        // MRO chains were already computed during scope resolution
        // (build_method_dispatch in the orchestrator). This phase counts
        // the computed chains from the store's heritage table.
        let mut mro_chains_computed = 0usize;

        let store_guard = shared.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref store) = *store_guard {
            let count = store.conn().query_row(
                "SELECT COUNT(*) FROM heritage WHERE parent_symbol_id IS NOT NULL",
                [],
                |row| row.get::<_, i64>(0),
            ).unwrap_or(0);
            mro_chains_computed = count as usize;
        }
        drop(store_guard);

        (ctx.on_progress)(PipelineProgress {
            phase: "mro".to_string(),
            percent: 70,
            message: format!("MRO: {} chains computed", mro_chains_computed),
            detail: None,
            stats: None,
        });

        Box::new(MroOutput {
            total_files: cross.total_files,
            mro_chains_computed,
        })
    }
}

// ── Phase: communities ──────────────────────────────────────────────────────

pub struct CommunitiesOutput {
    pub total_files: usize,
    pub community_count: usize,
}

pub struct CommunitiesPhase;

impl PipelinePhase for CommunitiesPhase {
    fn name(&self) -> &str { "communities" }
    fn deps(&self) -> &[&str] { &["mro"] }
    // Depends on mro (instant count query), not on cross_file data directly.
    // Runs in parallel with processes phase.

    fn execute(&self, ctx: &PipelineContext, shared: &PipelineSharedState, deps: &FxHashMap<PhaseName, &PhaseResult>) -> Box<dyn Any + Send + Sync> {
        let mro = get_phase_output::<MroOutput>(deps, "mro");

        if ctx.options.skip_graph_phases {
            return Box::new(CommunitiesOutput { total_files: mro.total_files, community_count: 0 });
        }

        perf_timer!("Community detection");

        (ctx.on_progress)(PipelineProgress {
            phase: "communities".to_string(),
            percent: 75,
            message: "Detecting communities...".to_string(),
            detail: None,
            stats: None,
        });

        let mut community_count = 0usize;

        let store_guard = shared.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref store) = *store_guard {
            match crate::community::detect_communities(
                store,
                &crate::community::CommunityConfig::default(),
            ) {
                Ok(result) => {
                    community_count = result.communities.len();
                    // Persist community memberships as MEMBER_OF edges
                    // so the evidence engine can use them.
                    match crate::community::store_memberships(store, &result) {
                        Ok(stored) => {
                            (ctx.on_progress)(PipelineProgress {
                                phase: "communities".to_string(),
                                percent: 80,
                                message: format!(
                                    "Found {} communities (modularity: {:.3}), stored {} memberships",
                                    community_count, result.stats.modularity, stored),
                                detail: None,
                                stats: None,
                            });
                        }
                        Err(e) => {
                            eprintln!("[communities] Warning: failed to store memberships: {}", e);
                        }
                    }
                }
                Err(e) => {
                    eprintln!("[communities] Warning: community detection failed: {}", e);
                }
            }
        }
        drop(store_guard);

        Box::new(CommunitiesOutput {
            total_files: mro.total_files,
            community_count,
        })
    }
}

// ── Phase: processes ────────────────────────────────────────────────────────

pub struct ProcessesOutput {
    pub total_files: usize,
    pub process_count: usize,
}

pub struct ProcessesPhase;

impl PipelinePhase for ProcessesPhase {
    fn name(&self) -> &str { "processes" }
    fn deps(&self) -> &[&str] { &["mro"] }
    // Depends on mro (instant count), not communities — runs in parallel with communities phase.
    // Process detection uses entry-point + BFS on the call graph, not community labels.

    fn execute(&self, ctx: &PipelineContext, shared: &PipelineSharedState, deps: &FxHashMap<PhaseName, &PhaseResult>) -> Box<dyn Any + Send + Sync> {
        let mro = get_phase_output::<MroOutput>(deps, "mro");

        if ctx.options.skip_graph_phases {
            return Box::new(ProcessesOutput { total_files: mro.total_files, process_count: 0 });
        }

        perf_timer!("Process detection");

        (ctx.on_progress)(PipelineProgress {
            phase: "processes".to_string(),
            percent: 85,
            message: "Detecting processes...".to_string(),
            detail: None,
            stats: None,
        });

        let mut process_count = 0usize;
        let mut steps_count = 0usize;

        let store_guard = shared.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref store) = *store_guard {
            match crate::process::detect_processes(
                store,
                &crate::process::ProcessConfig::default(),
            ) {
                Ok(result) => {
                    process_count = result.processes.len();
                    // Persist process nodes and STEP_IN_PROCESS edges.
                    match crate::process::store_processes(store, &result) {
                        Ok(n) => steps_count = n,
                        Err(e) => eprintln!("[processes] Warning: failed to store process steps: {}", e),
                    }
                    (ctx.on_progress)(PipelineProgress {
                        phase: "processes".to_string(),
                        percent: 90,
                        message: format!("Found {} execution flows ({} steps, avg {:.1})",
                            process_count, steps_count, result.stats.avg_step_count),
                        detail: None,
                        stats: None,
                    });
                }
                Err(e) => {
                    eprintln!("[processes] Warning: process detection failed: {}", e);
                }
            }
        }
        drop(store_guard);

        Box::new(ProcessesOutput {
            total_files: mro.total_files,
            process_count,
        })
    }
}

// ── Phase: routes ───────────────────────────────────────────────────────────

pub struct RoutesOutput {
    pub route_count: usize,
}

pub struct RoutesPhase;

impl PipelinePhase for RoutesPhase {
    fn name(&self) -> &str { "routes" }
    fn deps(&self) -> &[&str] { &["cross_file"] }

    fn execute(&self, ctx: &PipelineContext, shared: &PipelineSharedState, _deps: &FxHashMap<PhaseName, &PhaseResult>) -> Box<dyn Any + Send + Sync> {
        (ctx.on_progress)(PipelineProgress {
            phase: "routes".to_string(),
            percent: 55,
            message: "Detecting routes...".to_string(),
            detail: None,
            stats: None,
        });

        let parsed_files_guard = shared.parsed_files.lock().unwrap_or_else(|e| e.into_inner());
        let symbol_id_map_guard = shared.symbol_id_map.lock().unwrap_or_else(|e| e.into_inner());

        // Detect routes and collect them.
        let mut all_routes = Vec::new();
        // Also collect persistence data: (file_index, handler_db_id, file_routes)
        let mut persist_data: Vec<(usize, i64, Vec<crate::routes::Route>)> = Vec::new();

        for (file_idx, file) in parsed_files_guard.iter().enumerate() {
            let mut file_routes = crate::routes::detect_routes_from_path(&file.path);
            let lang_name = format!("{:?}", file.language).to_lowercase();
            file_routes.extend(crate::routes::detect_routes_from_parsed(
                &file.path, &lang_name, &file.decorators, &file.calls,
            ));

            if !file_routes.is_empty() {
                let handler_symbol = file.symbols.iter().find(|s| {
                    use crate::lang::CaptureTag;
                    matches!(s.kind,
                        CaptureTag::DefinitionFunction |
                        CaptureTag::DefinitionMethod |
                        CaptureTag::DefinitionClass)
                });

                let handler_db_id = handler_symbol
                    .and_then(|h| symbol_id_map_guard.get(&h.id).copied())
                    .unwrap_or(0);

                persist_data.push((file_idx, handler_db_id, file_routes.clone()));
            }

            all_routes.extend(file_routes);
        }

        // Batch-persist route→symbol edges in the graph store.
        let store_guard = shared.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref store) = *store_guard {
            let mut route_symbol_records: Vec<crate::store::SymbolRecord> = Vec::new();
            let mut route_edge_records: Vec<crate::store::EdgeRecord> = Vec::new();

            for (_file_idx, handler_db_id, file_routes) in &persist_data {
                if *handler_db_id == 0 { continue; }
                let handler_file_id = store.get_file_id_for_symbol(*handler_db_id).unwrap_or(None).unwrap_or(0);
                for route in file_routes {
                    let route_node_name = format!("ROUTE:{}:{}", route.method, route.path);
                    route_symbol_records.push(crate::store::SymbolRecord {
                        id: 0, file_id: handler_file_id,
                        name: route_node_name.clone(),
                        qualified_name: route_node_name,
                        kind: "Route".to_string(),
                        line: route.line, col: 0,
                        is_exported: false,
                        scope_id: None, owner_symbol_id: None,
                    });
                    route_edge_records.push(crate::store::EdgeRecord {
                        id: 0,
                        src_id: 0, // will be filled after symbol insert
                        dst_id: *handler_db_id,
                        edge_kind: "ROUTE".to_string(),
                        confidence: 1.0,
                        file_id: Some(handler_file_id),
                        line: route.line,
                    });
                }
            }

            // Batch insert all route symbols, then update edges with their IDs.
            if !route_symbol_records.is_empty() {
                let tx = store.begin_transaction();
                if let Ok(tx) = tx {
                    // Insert all route symbols.
                    let sym_ids: Vec<i64> = {
                        let stmt = tx.prepare(
                            "INSERT INTO symbols (file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
                        );
                        let mut sym_ids = Vec::with_capacity(route_symbol_records.len());
                        if let Ok(mut stmt) = stmt {
                            for rec in &route_symbol_records {
                                if stmt.execute(params![
                                    rec.file_id, rec.name, rec.qualified_name, rec.kind,
                                    rec.line as i64, rec.col as i64,
                                    if rec.is_exported { 1 } else { 0 },
                                    rec.scope_id, rec.owner_symbol_id,
                                ]).is_ok() {
                                    sym_ids.push(tx.last_insert_rowid());
                                } else {
                                    sym_ids.push(0);
                                }
                            }
                        }
                        sym_ids
                    }; // stmt is dropped here, releasing the borrow on tx

                    // Batch insert edges with correct src_ids.
                    {
                        let edge_stmt = tx.prepare(
                            "INSERT INTO edges (src_id, dst_id, edge_kind, confidence, file_id, line)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                        );
                        if let Ok(mut edge_stmt) = edge_stmt {
                            for (i, edge) in route_edge_records.iter().enumerate() {
                                if i < sym_ids.len() && sym_ids[i] != 0 {
                                    let _ = edge_stmt.execute(params![
                                        sym_ids[i], edge.dst_id, edge.edge_kind,
                                        edge.confidence, edge.file_id, edge.line as i64,
                                    ]);
                                }
                            }
                        }
                    }; // edge_stmt dropped here

                    let _ = tx.commit();
                }
            }

            // Persist to dedicated routes table.
            let mut routes_to_persist: Vec<(i64, crate::routes::Route)> = Vec::new();
            let mut file_ids_to_replace: Vec<i64> = Vec::new();
            for (_file_idx, handler_db_id, file_routes) in &persist_data {
                let file_id = if *handler_db_id != 0 {
                    store.get_file_id_for_symbol(*handler_db_id).unwrap_or(None).unwrap_or(0)
                } else { 0 };
                if file_id != 0 { file_ids_to_replace.push(file_id); }
                for route in file_routes {
                    routes_to_persist.push((*handler_db_id, route.clone()));
                }
            }
            let _ = store.persist_routes(&routes_to_persist, &file_ids_to_replace);
        }
        drop(store_guard);
        drop(symbol_id_map_guard);
        drop(parsed_files_guard);

        let route_count = all_routes.len();
        *shared.detected_routes.lock().unwrap_or_else(|e| e.into_inner()) = all_routes;

        (ctx.on_progress)(PipelineProgress {
            phase: "routes".to_string(),
            percent: 58,
            message: format!("Detected {} routes", route_count),
            detail: None,
            stats: None,
        });

        Box::new(RoutesOutput { route_count })
    }
}

// ── Phase: tools ────────────────────────────────────────────────────────────

pub struct ToolsOutput {
    pub tool_count: usize,
}

pub struct ToolsPhase;

impl PipelinePhase for ToolsPhase {
    fn name(&self) -> &str { "tools" }
    fn deps(&self) -> &[&str] { &["cross_file"] }

    fn execute(&self, ctx: &PipelineContext, shared: &PipelineSharedState, _deps: &FxHashMap<PhaseName, &PhaseResult>) -> Box<dyn Any + Send + Sync> {
        (ctx.on_progress)(PipelineProgress {
            phase: "tools".to_string(),
            percent: 55,
            message: "Detecting tools...".to_string(),
            detail: None,
            stats: None,
        });

        let parsed_files_guard = shared.parsed_files.lock().unwrap_or_else(|e| e.into_inner());

        // Tool detection: count symbols that look like tool definitions.
        // Heuristic: functions/methods with "tool" in the name, or symbols
        // in files that define MCP tool arrays, CLI commands, etc.
        let mut tool_count = 0usize;

        for parsed in parsed_files_guard.iter() {
            for sym in &parsed.symbols {
                let name_lower = sym.name.to_lowercase();
                if name_lower.contains("tool") || name_lower.contains("command")
                    || name_lower.contains("handler") || name_lower.contains("action")
                {
                    tool_count += 1;
                }
            }
        }
        drop(parsed_files_guard);

        (ctx.on_progress)(PipelineProgress {
            phase: "tools".to_string(),
            percent: 57,
            message: format!("Detected {} tool-like symbols", tool_count),
            detail: None,
            stats: None,
        });

        Box::new(ToolsOutput { tool_count })
    }
}

// ── Phase: orm ──────────────────────────────────────────────────────────────

pub struct OrmOutput {
    pub edges_created: usize,
    pub model_count: usize,
}

pub struct OrmPhase;

impl PipelinePhase for OrmPhase {
    fn name(&self) -> &str { "orm" }
    fn deps(&self) -> &[&str] { &["cross_file"] }

    fn execute(&self, ctx: &PipelineContext, shared: &PipelineSharedState, _deps: &FxHashMap<PhaseName, &PhaseResult>) -> Box<dyn Any + Send + Sync> {
        (ctx.on_progress)(PipelineProgress {
            phase: "orm".to_string(),
            percent: 55,
            message: "Detecting ORM queries...".to_string(),
            detail: None,
            stats: None,
        });

        let parsed_files_guard = shared.parsed_files.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::path::Path::new(&ctx.repo_path);
        let mut files_with_content: Vec<(String, String)> = Vec::new();

        for parsed in parsed_files_guard.iter() {
            let full_path = root.join(&parsed.path);
            if let Ok(content) = std::fs::read_to_string(&full_path) {
                files_with_content.push((parsed.path.clone(), content));
            }
        }
        drop(parsed_files_guard);

        let orm_result = crate::semantic::orm::extract_orm_queries_from_files(&files_with_content);

        // Persist ORM model nodes and QUERIES edges to the graph store.
        let mut edges_created = 0usize;
        let mut model_count = 0usize;

        let store_guard = shared.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref store) = *store_guard {
            // model_nodes: model_key -> (db_id, is_new). is_new=true means we need to insert it.
            let mut model_nodes: rustc_hash::FxHashMap<String, (i64, bool)> = rustc_hash::FxHashMap::default();
            let mut seen_edges: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();
            // New model symbols to insert, in order. The index in this vec corresponds
            // to the insertion order; we track which model_key maps to which index.
            let mut new_model_keys: Vec<String> = Vec::new();
            let mut new_model_records: Vec<crate::store::SymbolRecord> = Vec::new();
            let mut orm_edge_records: Vec<crate::store::EdgeRecord> = Vec::new();

            for q in &orm_result.queries {
                let model_key = format!("{}:{}", q.orm, q.model);
                let (model_db_id, _is_new) = *model_nodes.entry(model_key.clone()).or_insert_with(|| {
                    // Check if a Class/Interface with this name already exists.
                    let existing = store.get_symbols_by_name(&q.model).ok()
                        .and_then(|syms| syms.into_iter().find(|s| {
                            s.kind == "Class" || s.kind == "Interface"
                        }));
                    if let Some(sym) = existing {
                        return (sym.id, false);
                    }
                    // Create a new CodeElement node for the ORM model.
                    let rec = crate::store::SymbolRecord {
                        id: 0,
                        file_id: 0,
                        name: q.model.clone(),
                        qualified_name: format!("orm:{}:{}", q.orm, q.model),
                        kind: "CodeElement".to_string(),
                        line: q.line_number,
                        col: 0,
                        is_exported: false,
                        scope_id: None,
                        owner_symbol_id: None,
                    };
                    new_model_keys.push(model_key.clone());
                    new_model_records.push(rec);
                    (0, true) // placeholder, filled after batch insert
                });

                // Look up the file's DB ID.
                let file_id = store.get_file(&q.file_path).ok().flatten().map(|f| f.id).unwrap_or(0);
                if file_id == 0 {
                    continue;
                }

                let edge_key = format!("{}->{}:{}", file_id, model_key, q.method);
                if seen_edges.contains(&edge_key) {
                    continue;
                }
                seen_edges.insert(edge_key);

                orm_edge_records.push(crate::store::EdgeRecord {
                    id: 0,
                    src_id: file_id,
                    dst_id: model_db_id, // 0 for new models, filled after batch insert
                    edge_kind: "QUERIES".to_string(),
                    confidence: 0.9,
                    file_id: Some(file_id),
                    line: q.line_number,
                });
            }

            // Batch insert new model symbols.
            if !new_model_records.is_empty() {
                let tx = store.begin_transaction();
                if let Ok(tx) = tx {
                    // Insert all new model symbols and collect their real DB IDs.
                    let new_ids: Vec<i64> = {
                        let stmt = tx.prepare(
                            "INSERT INTO symbols (file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
                        );
                        let mut ids = Vec::with_capacity(new_model_records.len());
                        if let Ok(mut stmt) = stmt {
                            for rec in &new_model_records {
                                if stmt.execute(rusqlite::params![
                                    rec.file_id, rec.name, rec.qualified_name, rec.kind,
                                    rec.line as i64, rec.col as i64,
                                    if rec.is_exported { 1 } else { 0 },
                                    rec.scope_id, rec.owner_symbol_id,
                                ]).is_ok() {
                                    ids.push(tx.last_insert_rowid());
                                } else {
                                    ids.push(0);
                                }
                            }
                        }
                        ids
                    };

                    // Update model_nodes with real DB IDs (deterministic: new_model_keys order).
                    for (i, model_key) in new_model_keys.iter().enumerate() {
                        if i < new_ids.len() && new_ids[i] != 0 {
                            model_nodes.insert(model_key.clone(), (new_ids[i], false));
                        }
                    }

                    // Update edge dst_ids for edges pointing to newly-inserted models.
                    for edge in &mut orm_edge_records {
                        if edge.dst_id == 0 {
                            // Find which model_key this edge targets by checking seen_edges.
                            for (model_key, (db_id, _)) in &model_nodes {
                                if *db_id == 0 { continue; }
                                let prefix = format!("{}->{}:", edge.src_id, model_key);
                                if seen_edges.iter().any(|ek| ek.starts_with(&prefix)) {
                                    edge.dst_id = *db_id;
                                    break;
                                }
                            }
                        }
                    }

                    // Batch insert edges.
                    {
                        let edge_stmt = tx.prepare(
                            "INSERT INTO edges (src_id, dst_id, edge_kind, confidence, file_id, line)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                        );
                        if let Ok(mut edge_stmt) = edge_stmt {
                            for edge in &orm_edge_records {
                                if edge.dst_id != 0 {
                                    let _ = edge_stmt.execute(rusqlite::params![
                                        edge.src_id, edge.dst_id, edge.edge_kind,
                                        edge.confidence, edge.file_id, edge.line as i64,
                                    ]);
                                    edges_created += 1;
                                }
                            }
                        }
                    };

                    let _ = tx.commit();
                }
            }

            model_count = model_nodes.len();
        }
        drop(store_guard);

        (ctx.on_progress)(PipelineProgress {
            phase: "orm".to_string(),
            percent: 57,
            message: format!("Found {} ORM queries ({} Prisma, {} Supabase) — {} edges, {} models",
                orm_result.queries.len(), orm_result.prisma_count, orm_result.supabase_count,
                edges_created, model_count),
            detail: None,
            stats: None,
        });

        Box::new(OrmOutput { edges_created, model_count })
    }
}

// ── Phase: markdown ─────────────────────────────────────────────────────────

pub struct MarkdownOutput {
    pub sections_found: usize,
    pub links_created: usize,
}

pub struct MarkdownPhase;

impl PipelinePhase for MarkdownPhase {
    fn name(&self) -> &str { "markdown" }
    fn deps(&self) -> &[&str] { &["cross_file"] }

    fn execute(&self, ctx: &PipelineContext, shared: &PipelineSharedState, _deps: &FxHashMap<PhaseName, &PhaseResult>) -> Box<dyn Any + Send + Sync> {
        (ctx.on_progress)(PipelineProgress {
            phase: "markdown".to_string(),
            percent: 25,
            message: "Extracting markdown sections...".to_string(),
            detail: None,
            stats: None,
        });

        let parsed_files_guard = shared.parsed_files.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::path::Path::new(&ctx.repo_path);
        let mut md_files: Vec<(String, String)> = Vec::new();

        for parsed in parsed_files_guard.iter() {
            let ext = std::path::Path::new(&parsed.path)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            if ext == "md" || ext == "mdx" {
                let full_path = root.join(&parsed.path);
                if let Ok(content) = std::fs::read_to_string(&full_path) {
                    md_files.push((parsed.path.clone(), content));
                }
            }
        }
        drop(parsed_files_guard);

        let md_result = crate::semantic::markdown::process_markdown_files(&md_files);

        // Persist Section nodes, CONTAINS edges (heading hierarchy), and IMPORTS
        // edges (cross-file links) to the graph store.
        let sections_found = md_result.sections;
        let mut links_created = 0usize;

        let store_guard = shared.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref store) = *store_guard {
            // Build the set of all known file paths for cross-link validation.
            let all_path_set: rustc_hash::FxHashSet<String> = {
                let files = store.get_all_files().unwrap_or_default();
                files.into_iter().map(|f| f.path).collect()
            };

            let mut section_symbol_records: Vec<crate::store::SymbolRecord> = Vec::new();
            let mut contains_edge_records: Vec<crate::store::EdgeRecord> = Vec::new();
            let mut imports_edge_records: Vec<crate::store::EdgeRecord> = Vec::new();

            // Track Section node IDs: (file_path, heading_text, line) -> db_id
            let mut section_ids: Vec<(String, String, usize, i64)> = Vec::new();

            for (file_path, content) in &md_files {
                let file_id = store.get_file(file_path).ok().flatten().map(|f| f.id).unwrap_or(0);
                if file_id == 0 {
                    continue;
                }

                let (headings, cross_links) = crate::semantic::markdown::extract_markdown(file_path, content);

                if headings.is_empty() && cross_links.is_empty() {
                    continue;
                }

                // --- Heading hierarchy: compute endLine spans and create Section nodes ---
                let lines: Vec<&str> = content.lines().collect();
                let total_lines = lines.len();

                // Stack for heading hierarchy: (level, section_db_id).
                // section_db_id is 0 (placeholder) for new sections; filled after batch insert.
                let mut section_stack: Vec<(u8, i64)> = Vec::new();

                for h_idx in 0..headings.len() {
                    let heading = &headings[h_idx];
                    let line_1based = heading.line + 1;

                    // Compute endLine: line before next heading at same or higher level, or EOF.
                    let _end_line = {
                        let mut el = total_lines;
                        for j in (h_idx + 1)..headings.len() {
                            if headings[j].level <= heading.level {
                                el = headings[j].line;
                                break;
                            }
                        }
                        el
                    };

                    let section_rec = crate::store::SymbolRecord {
                        id: 0,
                        file_id,
                        name: heading.text.clone(),
                        qualified_name: format!("Section:{}:L{}:{}", file_path, line_1based, heading.text),
                        kind: "Section".to_string(),
                        line: heading.line,
                        col: 0,
                        is_exported: false,
                        scope_id: None,
                        owner_symbol_id: None,
                    };
                    section_symbol_records.push(section_rec);

                    // Find parent: pop stack until we find a level strictly less than current.
                    while section_stack.last().is_some_and(|(lvl, _)| *lvl >= heading.level) {
                        section_stack.pop();
                    }

                    let parent_db_id = section_stack.last().map(|(_, id)| *id).unwrap_or(file_id);

                    // CONTAINS edge from parent to this section (placeholder dst_id).
                    contains_edge_records.push(crate::store::EdgeRecord {
                        id: 0,
                        src_id: parent_db_id,
                        dst_id: 0, // placeholder, filled after batch insert
                        edge_kind: "CONTAINS".to_string(),
                        confidence: 1.0,
                        file_id: Some(file_id),
                        line: heading.line,
                    });

                    // Track for later ID resolution.
                    section_ids.push((file_path.clone(), heading.text.clone(), heading.line, 0));
                    // The contains edge at index (contains_edge_records.len() - 1) targets
                    // the section at index (section_idx).

                    section_stack.push((heading.level, 0)); // placeholder
                }

                // --- Cross-file links ---
                let file_dir = std::path::Path::new(file_path)
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                let mut seen_links: rustc_hash::FxHashSet<String> = rustc_hash::FxHashSet::default();

                for link in &cross_links {
                    // Skip external URLs, anchors, mailto.
                    if link.target_file.starts_with("http://")
                        || link.target_file.starts_with("https://")
                        || link.target_file.starts_with('#')
                        || link.target_file.starts_with("mailto:")
                    {
                        continue;
                    }

                    // Strip anchor fragments.
                    let clean_target = link.target_file.split('#').next().unwrap_or("");
                    if clean_target.is_empty() {
                        continue;
                    }

                    // Resolve relative to the file's directory.
                    let resolved = if clean_target.starts_with('/') {
                        clean_target.trim_start_matches('/').to_string()
                    } else {
                        let joined = std::path::Path::new(&file_dir).join(clean_target);
                        joined.to_string_lossy().to_string()
                    };

                    if !all_path_set.contains(&resolved) {
                        continue;
                    }

                    let target_file_id = store.get_file(&resolved).ok().flatten().map(|f| f.id).unwrap_or(0);
                    if target_file_id == 0 {
                        continue;
                    }

                    let link_key = format!("{}->{}", file_id, target_file_id);
                    if seen_links.contains(&link_key) {
                        continue;
                    }
                    seen_links.insert(link_key);

                    imports_edge_records.push(crate::store::EdgeRecord {
                        id: 0,
                        src_id: file_id,
                        dst_id: target_file_id,
                        edge_kind: "IMPORTS".to_string(),
                        confidence: 0.8,
                        file_id: Some(file_id),
                        line: link.line,
                    });
                }
            }

            // Batch insert Section symbols and resolve IDs for CONTAINS edges.
            if !section_symbol_records.is_empty() {
                let tx = store.begin_transaction();
                if let Ok(tx) = tx {
                    // Insert all Section symbols.
                    let new_ids: Vec<i64> = {
                        let stmt = tx.prepare(
                            "INSERT INTO symbols (file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
                        );
                        let mut ids = Vec::with_capacity(section_symbol_records.len());
                        if let Ok(mut stmt) = stmt {
                            for rec in &section_symbol_records {
                                if stmt.execute(rusqlite::params![
                                    rec.file_id, rec.name, rec.qualified_name, rec.kind,
                                    rec.line as i64, rec.col as i64,
                                    if rec.is_exported { 1 } else { 0 },
                                    rec.scope_id, rec.owner_symbol_id,
                                ]).is_ok() {
                                    ids.push(tx.last_insert_rowid());
                                } else {
                                    ids.push(0);
                                }
                            }
                        }
                        ids
                    };

                    // Update section_ids with real DB IDs.
                    for (i, (_, _, _, ref mut db_id)) in section_ids.iter_mut().enumerate() {
                        if i < new_ids.len() && new_ids[i] != 0 {
                            *db_id = new_ids[i];
                        }
                    }

                    // Update CONTAINS edge dst_ids: each edge at index i targets section at index i.
                    for (i, edge) in contains_edge_records.iter_mut().enumerate() {
                        if i < section_ids.len() {
                            edge.dst_id = section_ids[i].3;
                        }
                    }

                    // Batch insert CONTAINS edges.
                    {
                        let edge_stmt = tx.prepare(
                            "INSERT INTO edges (src_id, dst_id, edge_kind, confidence, file_id, line)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
                        );
                        if let Ok(mut edge_stmt) = edge_stmt {
                            for edge in &contains_edge_records {
                                if edge.dst_id != 0 {
                                    let _ = edge_stmt.execute(rusqlite::params![
                                        edge.src_id, edge.dst_id, edge.edge_kind,
                                        edge.confidence, edge.file_id, edge.line as i64,
                                    ]);
                                }
                            }
                        }
                    };

                    let _ = tx.commit();
                }
            }

            // Batch insert IMPORTS edges.
            if !imports_edge_records.is_empty() {
                let _ = store.insert_edges_batch(&imports_edge_records);
                links_created = imports_edge_records.len();
            }
        }
        drop(store_guard);

        (ctx.on_progress)(PipelineProgress {
            phase: "markdown".to_string(),
            percent: 27,
            message: format!("Found {} markdown sections, {} cross-links",
                sections_found, links_created),
            detail: None,
            stats: None,
        });

        Box::new(MarkdownOutput { sections_found, links_created })
    }
}

// ── Phase: cobol ────────────────────────────────────────────────────────────

pub struct CobolOutput {
    pub divisions_found: usize,
}

pub struct CobolPhase;

impl PipelinePhase for CobolPhase {
    fn name(&self) -> &str { "cobol" }
    fn deps(&self) -> &[&str] { &["cross_file"] }

    fn execute(&self, ctx: &PipelineContext, shared: &PipelineSharedState, _deps: &FxHashMap<PhaseName, &PhaseResult>) -> Box<dyn Any + Send + Sync> {
        (ctx.on_progress)(PipelineProgress {
            phase: "cobol".to_string(),
            percent: 25,
            message: "Tagging COBOL...".to_string(),
            detail: None,
            stats: None,
        });

        let parsed_files_guard = shared.parsed_files.lock().unwrap_or_else(|e| e.into_inner());
        let root = std::path::Path::new(&ctx.repo_path);
        let mut cobol_files: Vec<(String, String)> = Vec::new();

        for parsed in parsed_files_guard.iter() {
            if crate::semantic::cobol::is_cobol_file(&parsed.path)
                || crate::semantic::cobol::is_jcl_file(&parsed.path)
            {
                let full_path = root.join(&parsed.path);
                if let Ok(content) = std::fs::read_to_string(&full_path) {
                    cobol_files.push((parsed.path.clone(), content));
                }
            }
        }
        drop(parsed_files_guard);

        let cobol_result = crate::semantic::cobol::process_cobol_files(&cobol_files);
        let divisions_found = cobol_result.divisions;

        (ctx.on_progress)(PipelineProgress {
            phase: "cobol".to_string(),
            percent: 27,
            message: format!("Found {} divisions, {} sections, {} paragraphs ({} JCL jobs)",
                cobol_result.divisions, cobol_result.sections, cobol_result.paragraphs,
                cobol_result.jcl_jobs),
            detail: None,
            stats: None,
        });

        Box::new(CobolOutput { divisions_found })
    }
}

// ── Phase: scope_resolution ─────────────────────────────────────────────────

pub struct ScopeResolutionOutput {
    pub total_files: usize,
    pub resolved_references: usize,
}

pub struct ScopeResolutionPhase;

pub struct TypeEnvPhase;

impl PipelinePhase for TypeEnvPhase {
    fn name(&self) -> &str { "type_env" }
    fn deps(&self) -> &[&str] { &["cross_file"] }

    fn execute(&self, ctx: &PipelineContext, shared: &PipelineSharedState, deps: &FxHashMap<PhaseName, &PhaseResult>) -> Box<dyn Any + Send + Sync> {
        let cross = get_phase_output::<CrossFileOutput>(deps, "cross_file");

        (ctx.on_progress)(PipelineProgress {
            phase: "type_env".to_string(),
            percent: 56,
            message: "Type environment (enriched inline in cross_file)...".to_string(),
            detail: None,
            stats: None,
        });

        // Type environment enrichment now runs inline in the cross_file phase
        // (between evidence lifecycle and scope resolution) so that enriched
        // type bindings are available during scope resolution. This phase is
        // kept for DAG compatibility but is a no-op.
        let parsed_files_guard = shared.parsed_files.lock().unwrap_or_else(|e| e.into_inner());
        let type_bindings_count: usize = parsed_files_guard.iter()
            .map(|pf| pf.type_bindings.len())
            .sum();
        drop(parsed_files_guard);

        tracing::info!(files = cross.total_files, type_bindings = type_bindings_count, "Type environment (already enriched in cross_file)");

        Box::new(TypeEnvOutput {
            total_files: cross.total_files,
            type_bindings_count,
        })
    }
}

pub struct TypeEnvOutput {
    pub total_files: usize,
    pub type_bindings_count: usize,
}

impl PipelinePhase for ScopeResolutionPhase {
    fn name(&self) -> &str { "scope_resolution" }
    fn deps(&self) -> &[&str] { &["cross_file"] }
    // No-op: scope resolution already ran inside cross_file phase (after type_env
    // enrichment was applied to parsed_files). This phase exists only for DAG
    // compatibility with GitNexusRelay.

    fn execute(&self, ctx: &PipelineContext, _shared: &PipelineSharedState, deps: &FxHashMap<PhaseName, &PhaseResult>) -> Box<dyn Any + Send + Sync> {
        let cross = get_phase_output::<CrossFileOutput>(deps, "cross_file");

        (ctx.on_progress)(PipelineProgress {
            phase: "scope_resolution".to_string(),
            percent: 60,
            message: "Resolving scopes...".to_string(),
            detail: None,
            stats: None,
        });

        // Scope resolution was already run as part of the cross_file phase
        // (the orchestrator does the full scope-resolution pipeline).
        // This phase exists in the DAG for compatibility with GitNexusRelay's
        // 12-phase structure, but the actual work is done in cross_file.
        let resolved_references = cross.resolved_calls;

        Box::new(ScopeResolutionOutput {
            total_files: cross.total_files,
            resolved_references,
        })
    }
}

// ── Convenience: build the full phase list ──────────────────────────────────

/// Build the complete pipeline phase list.
///
/// DAG:
/// ```text
///   cross_file → [routes, tools, orm, markdown, cobol, type_env (no-op), scope_resolution (no-op), mro]
///   Note: type_env enrichment runs inline in cross_file before scope resolution.
///     → [communities, processes]
/// ```
///
/// Scan and parse are handled by `build_graph()` before the pipeline runs.
pub fn all_phases() -> Vec<Box<dyn PipelinePhase>> {
    vec![
        Box::new(CrossFilePhase),
        // Wave 2 — all independent readers, run in parallel:
        Box::new(RoutesPhase),
        Box::new(ToolsPhase),
        Box::new(OrmPhase),
        Box::new(MarkdownPhase),
        Box::new(CobolPhase),
        Box::new(TypeEnvPhase),           // type inference, depends on cross_file
        Box::new(ScopeResolutionPhase),  // no-op, depends on cross_file
        Box::new(MroPhase),              // no-op count, depends on cross_file
        // Wave 3 — parallel graph analytics:
        Box::new(CommunitiesPhase),
        Box::new(ProcessesPhase),
    ]
}

