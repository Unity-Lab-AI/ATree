//! Pipeline DAG — typed phase orchestration for code intelligence analysis.
//!
//! Modeled after GitNexusRelay's pipeline orchestrator from
//! `gitnexus/src/core/ingestion/pipeline.ts` and
//! `gitnexus/src/core/ingestion/pipeline-phases/runner.ts`.
//!
//! The pipeline is a DAG of named phases with explicit dependencies.
//! Each phase has typed inputs (from upstream phases) and typed outputs
//! (consumed by downstream phases). The runner executes phases in
//! topological order via Kahn's algorithm.
//!
//! Scan and parse are handled by `build_graph()` in `src/lib.rs` before
//! the pipeline runs. The pipeline starts at `cross_file` with pre-populated
//! `ParsedFile` data.
//!
//! Phase dependency graph:
//! ```text
//!   cross_file → [routes, tools, orm, markdown, cobol, scope_resolution, mro]
//!     → [communities, processes]
//! ```

use crate::perf_print;
use crate::perf_timer;
use rustc_hash::FxHashMap;
use std::any::Any;
use std::sync::{Arc, Mutex};
use std::time::Instant;

// ── Re-exports ─────────────────────────────────────────────────────────────

pub mod phases;
pub use phases::{
    CrossFilePhase, CrossFileOutput,
    RoutesPhase, RoutesOutput,
    ToolsPhase, ToolsOutput,
    OrmPhase, OrmOutput,
    MarkdownPhase, MarkdownOutput,
    CobolPhase, CobolOutput,
    ScopeResolutionPhase, ScopeResolutionOutput,
    MroPhase, MroOutput,
    CommunitiesPhase, CommunitiesOutput,
    ProcessesPhase, ProcessesOutput,
    all_phases,
};

// ── Core types ──────────────────────────────────────────────────────────────

/// Unique phase name.
pub type PhaseName = String;

/// Options controlling pipeline execution.
#[derive(Debug, Clone)]
#[derive(Default)]
pub struct PipelineOptions {
    /// Skip MRO, community detection, and process extraction for faster test runs.
    pub skip_graph_phases: bool,
    /// Force sequential parsing (no worker pool).
    pub skip_workers: bool,
}


/// Progress event emitted by the pipeline.
#[derive(Debug, Clone)]
pub struct PipelineProgress {
    pub phase: String,
    pub percent: u8,
    pub message: String,
    pub detail: Option<String>,
    pub stats: Option<PipelineStats>,
}

#[derive(Debug, Clone, Default)]
pub struct PipelineStats {
    pub files_processed: usize,
    pub total_files: usize,
    pub nodes_created: usize,
}

/// Shared state for pipeline phases.
///
/// Fields that multiple phases need to mutate are protected by `Mutex`.
/// Read-only fields (like `parsed_files` after the parse phase) are plain values
/// since every phase only reads them after initial population.
///
/// The pipeline runner executes independent phases in parallel waves using
/// `std::thread::scope`. The `Mutex` types allow shared access without requiring
/// `&mut PipelineSharedState` in the `PipelinePhase::execute` signature.
pub struct PipelineSharedState {
    /// The graph store — populated by cross_file phase, consumed by all later phases.
    pub store: Mutex<Option<crate::store::GraphStore>>,
    /// Parsed files from the parse phase. CrossFilePhase mutates these during
    /// scope resolution; all other phases read them.
    pub parsed_files: Mutex<Vec<crate::semantic::ParsedFile>>,
    /// In-memory symbol ID → DB symbol ID mapping.
    pub symbol_id_map: Mutex<rustc_hash::FxHashMap<u64, i64>>,
    /// Detected routes.
    pub detected_routes: Mutex<Vec<crate::routes::Route>>,
    /// Scope resolution stats from the cross_file phase.
    pub scope_resolution_stats: Mutex<Option<crate::scope_resolution::ScopeResolutionStats>>,
}

impl Default for PipelineSharedState {
    fn default() -> Self {
        Self {
            store: Mutex::new(None),
            parsed_files: Mutex::new(Vec::new()),
            symbol_id_map: Mutex::new(rustc_hash::FxHashMap::default()),
            detected_routes: Mutex::new(Vec::new()),
            scope_resolution_stats: Mutex::new(None),
        }
    }
}

/// Immutable context available to every phase.
///
/// `on_progress` is `Arc<dyn Fn(PipelineProgress) + Send + Sync>` so the runner
/// can share it across threads when executing phases in parallel.
pub struct PipelineContext {
    /// Absolute path to the repository root.
    pub repo_path: String,
    /// Pipeline options.
    pub options: PipelineOptions,
    /// Progress callback — `Arc` + `Sync` for parallel wave execution.
    pub on_progress: Arc<dyn Fn(PipelineProgress) + Send + Sync>,
    /// DB path for the graph store.
    pub db_path: Option<std::path::PathBuf>,
    /// Whether to run incremental scanning.
    pub incremental: bool,
    /// Repo label for cross-repo grouping.
    pub repo_label: Option<String>,
}

/// The result of a single phase execution.
pub struct PhaseResult {
    pub phase_name: PhaseName,
    /// Phase output — `Send + Sync` so the results HashMap can be shared
    /// across threads during parallel wave execution.
    pub output: Box<dyn Any + Send + Sync>,
    pub duration_ms: u128,
}

/// A single phase in the pipeline DAG.
///
/// `execute` takes `&PipelineSharedState` (not `&mut`) so the runner can
/// execute independent phases concurrently. Mutable shared state is accessed
/// through `Mutex` fields inside `PipelineSharedState`.
pub trait PipelinePhase: Send + Sync {
    /// Unique name for logging and result lookup.
    fn name(&self) -> &str;

    /// Names of phases this phase depends on.
    fn deps(&self) -> &[&str];

    /// Execute the phase.
    fn execute(
        &self,
        ctx: &PipelineContext,
        shared: &PipelineSharedState,
        dep_outputs: &FxHashMap<PhaseName, &PhaseResult>,
    ) -> Box<dyn Any + Send + Sync>;
}

/// Typed accessor for a dependency's output.
pub fn get_phase_output<'a, T: 'static>(deps: &'a FxHashMap<PhaseName, &PhaseResult>, phase_name: &str) -> &'a T {
    let result = deps.get(phase_name)
        .unwrap_or_else(|| panic!("Phase '{}' not found in resolved dependencies", phase_name));
    result.output.downcast_ref::<T>()
        .unwrap_or_else(|| panic!("Phase '{}' output type mismatch", phase_name))
}

// ── Pipeline runner ─────────────────────────────────────────────────────────

/// Execute a set of pipeline phases in dependency order, running independent
/// phases concurrently using `std::thread::scope`.
///
/// Phases are grouped into **waves**: all phases whose dependencies have been
/// satisfied are spawned simultaneously. The wave completes (all threads joined)
/// before the next wave begins. This preserves the DAG ordering guarantee while
/// extracting parallelism from independent branches.
pub fn run_pipeline(
    phases: &[&dyn PipelinePhase],
    ctx: &PipelineContext,
    shared: &PipelineSharedState,
) -> Result<FxHashMap<PhaseName, PhaseResult>, PipelineError> {
    let sorted = topological_sort(phases)?;
    let mut results: FxHashMap<PhaseName, PhaseResult> = FxHashMap::default();

    // Build a set of completed phase names to track which deps are satisfied.
    let mut completed: FxHashMap<PhaseName, ()> = FxHashMap::default();
    let mut remaining: Vec<&dyn PipelinePhase> = sorted;

    while !remaining.is_empty() {
        // Find all phases whose deps are fully satisfied.
        let (ready, not_ready): (Vec<_>, Vec<_>) = remaining
            .into_iter()
            .partition(|p| p.deps().iter().all(|d| completed.contains_key(*d)));

        if ready.is_empty() {
            return Err(PipelineError::CycleDetected);
        }

        // Execute ready phases — parallel by default, sequential when skip_workers is set.
        let wave_results: Vec<(PhaseName, PhaseResult)> = if ctx.options.skip_workers {
            let mut out = Vec::with_capacity(ready.len());
            for phase in ready {
                let start = Instant::now();
                let dep_map: FxHashMap<PhaseName, &PhaseResult> = phase.deps()
                    .iter()
                    .filter_map(|dep_name| {
                        results.get(*dep_name).map(|r| (dep_name.to_string(), r))
                    })
                    .collect();
                let output = phase.execute(ctx, shared, &dep_map);
                let duration_ms = start.elapsed().as_millis();
                out.push((phase.name().to_string(), PhaseResult {
                    phase_name: phase.name().to_string(),
                    output,
                    duration_ms,
                }));
            }
            out
        } else {
            std::thread::scope(|scope| {
                let handles: Vec<_> = ready
                    .into_iter()
                    .map(|phase| {
                        let results_ref = &results;
                        scope.spawn(move || {
                            let start = Instant::now();

                            let dep_map: FxHashMap<PhaseName, &PhaseResult> = phase.deps()
                                .iter()
                                .filter_map(|dep_name| {
                                    results_ref.get(*dep_name).map(|r| (dep_name.to_string(), r))
                                })
                                .collect();

                            let output = phase.execute(ctx, shared, &dep_map);
                            let duration_ms = start.elapsed().as_millis();

                            (phase.name().to_string(), PhaseResult {
                                phase_name: phase.name().to_string(),
                                output,
                                duration_ms,
                            })
                        })
                    })
                    .collect();

                handles.into_iter().map(|h| h.join().unwrap()).collect()
            })
        };

        for (name, result) in wave_results {
            completed.insert(name.clone(), ());
            perf_print!("[PERF]   Phase '{}': {}ms", name, result.duration_ms);
            (ctx.on_progress)(PipelineProgress {
                phase: name.clone(),
                percent: 0,
                message: format!("Phase '{}' completed ({}ms)", name, result.duration_ms),
                detail: None,
                stats: None,
            });
            results.insert(name, result);
        }

        remaining = not_ready;
    }

    Ok(results)
}

/// Validate the phase DAG and return phases in topological order.
fn topological_sort<'a>(phases: &'a [&'a dyn PipelinePhase]) -> Result<Vec<&'a dyn PipelinePhase>, PipelineError> {
    let phase_map: FxHashMap<&str, &dyn PipelinePhase> = phases.iter()
        .map(|p| (p.name(), *p))
        .collect();

    if phase_map.len() != phases.len() {
        return Err(PipelineError::DuplicatePhase);
    }

    for phase in phases {
        for dep in phase.deps() {
            if !phase_map.contains_key(dep) {
                return Err(PipelineError::MissingDependency {
                    phase: phase.name().to_string(),
                    dependency: dep.to_string(),
                });
            }
        }
    }

    let mut in_degree: FxHashMap<&str, usize> = FxHashMap::default();
    let mut reverse_deps: FxHashMap<&str, Vec<&str>> = FxHashMap::default();

    for phase in phases {
        in_degree.insert(phase.name(), phase.deps().len());
        for dep in phase.deps() {
            reverse_deps.entry(dep).or_default().push(phase.name());
        }
    }

    let mut sorted: Vec<&dyn PipelinePhase> = Vec::new();
    let mut queue: Vec<&str> = in_degree.iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&name, _)| name)
        .collect();

    while let Some(name) = queue.pop() {
        sorted.push(phase_map[name]);
        for dependent in reverse_deps.get(name).map(|v| v.as_slice()).unwrap_or(&[]) {
            let deg = in_degree.get_mut(dependent).unwrap();
            *deg -= 1;
            if *deg == 0 { queue.push(dependent); }
        }
    }

    if sorted.len() != phases.len() {
        return Err(PipelineError::CycleDetected);
    }

    Ok(sorted)
}

/// Pipeline execution errors.
#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("Duplicate phase name in pipeline")]
    DuplicatePhase,
    #[error("Phase '{phase}' depends on '{dependency}', which is not registered")]
    MissingDependency { phase: String, dependency: String },
    #[error("Cycle detected in pipeline phase graph")]
    CycleDetected,
    #[error("Phase '{phase}' failed: {message}")]
    PhaseFailed { phase: String, message: String },
}

// ── Pipeline result ─────────────────────────────────────────────────────────

/// Final result of a full pipeline run.
#[derive(Default)]
pub struct PipelineResult {
    pub total_file_count: usize,
    pub used_worker_pool: bool,
}


#[cfg(test)]
mod tests {
    use super::*;

    struct TestPhase {
        name: &'static str,
        deps: &'static [&'static str],
        output: &'static str,
    }

    impl PipelinePhase for TestPhase {
        fn name(&self) -> &str { self.name }
        fn deps(&self) -> &[&str] { self.deps }
        fn execute(&self, _ctx: &PipelineContext, _shared: &PipelineSharedState, _deps: &FxHashMap<PhaseName, &PhaseResult>) -> Box<dyn Any + Send + Sync> {
            Box::new(self.output.to_string())
        }
    }

    #[test]
    fn test_topological_sort_linear() {
        let a = TestPhase { name: "a", deps: &[], output: "a" };
        let b = TestPhase { name: "b", deps: &["a"], output: "b" };
        let c = TestPhase { name: "c", deps: &["b"], output: "c" };
        let phases: Vec<&dyn PipelinePhase> = vec![&c, &a, &b];
        let sorted = topological_sort(&phases).unwrap();
        assert_eq!(sorted[0].name(), "a");
        assert_eq!(sorted[1].name(), "b");
        assert_eq!(sorted[2].name(), "c");
    }

    #[test]
    fn test_topological_sort_diamond() {
        let a = TestPhase { name: "a", deps: &[], output: "a" };
        let b = TestPhase { name: "b", deps: &["a"], output: "b" };
        let c = TestPhase { name: "c", deps: &["a"], output: "c" };
        let d = TestPhase { name: "d", deps: &["b", "c"], output: "d" };
        let phases: Vec<&dyn PipelinePhase> = vec![&d, &c, &b, &a];
        let sorted = topological_sort(&phases).unwrap();
        assert_eq!(sorted[0].name(), "a");
        assert_eq!(sorted[3].name(), "d");
    }

    #[test]
    fn test_cycle_detected() {
        let a = TestPhase { name: "a", deps: &["c"], output: "a" };
        let b = TestPhase { name: "b", deps: &["a"], output: "b" };
        let c = TestPhase { name: "c", deps: &["b"], output: "c" };
        let phases: Vec<&dyn PipelinePhase> = vec![&a, &b, &c];
        assert!(topological_sort(&phases).is_err());
    }

    #[test]
    fn test_missing_dep_detected() {
        let a = TestPhase { name: "a", deps: &["nonexistent"], output: "a" };
        let phases: Vec<&dyn PipelinePhase> = vec![&a];
        assert!(topological_sort(&phases).is_err());
    }

    #[test]
    fn test_run_pipeline() {
        let a = TestPhase { name: "scan", deps: &[], output: "scanned" };
        let b = TestPhase { name: "parse", deps: &["scan"], output: "parsed" };
        let phases: Vec<&dyn PipelinePhase> = vec![&a, &b];
        let ctx = PipelineContext {
            repo_path: "/tmp/test".to_string(),
            options: PipelineOptions::default(),
            on_progress: Arc::new(|_p: PipelineProgress| {}),
            db_path: None,
            incremental: false,
            repo_label: None,
        };
        let shared = PipelineSharedState::default();
        let results = run_pipeline(&phases, &ctx, &shared).unwrap();
        assert_eq!(results.len(), 2);
        let scan_out = results["scan"].output.downcast_ref::<String>().unwrap();
        assert_eq!(scan_out, "scanned");
    }
}
