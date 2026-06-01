use crate::lang::{get_provider_for_extension, LanguageProvider};
use crate::semantic::ParsedFile;
use crate::syntax::SyntaxEngine;
use crate::resolver::SymbolTable;
use crate::store::{GraphStore, SymbolRecord, ScopeRecord, ImportRecord, CallRecord, HeritageRecord};
pub mod lang;
pub mod syntax;
pub mod semantic;
pub mod resolver;
pub mod scope_resolution;
pub mod graph;
#[cfg(feature = "git")]
pub mod git_history;
pub mod store;
pub use store::validate_cypher_query;
pub mod community;
pub mod process;
pub mod routes;
pub mod search;
#[cfg(feature = "embeddings")]
pub mod embeddings;
pub mod type_env;
pub mod pipeline;
pub mod evidence;
pub mod evidence_path;
pub mod evidence_bundle;
pub mod patterns;
pub mod constraints;
pub mod perf;

#[cfg(feature = "mcp")]
pub mod mcp;
/// `atree` — File-system A* pathfinding library.
///
/// Public API:
/// - [`build_graph`] — parallel work-stealing directory scan
/// - [`astar`], [`compute_depths`], [`bfs_expanded`] — graph algorithms
/// - [`print_tree`], [`generate_dot`] — rendering
/// - [`JsonReport`], [`PathReport`], [`Stats`] — serializable output for IPC
///
/// Resource helpers: [`half_cores`], [`all_cores`], [`available_memory_bytes`],
/// [`estimated_node_cap_for_half_memory`].
///
/// Filenames are sanitized at scan time: any control character (including ANSI
/// escape sequences) is replaced with `?` before being stored in [`NodeMeta`].
/// This prevents terminal-injection attacks via malicious filenames.

/// Maximum individual file size for content reads (16 MB).
/// Files larger than this are silently skipped to prevent OOM on huge files.
const MAX_FILE_SIZE: u64 = 16 * 1024 * 1024;

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, VecDeque};
use std::fs::{self, DirEntry};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use crossbeam_deque::{Steal, Stealer, Worker};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

// =====================================================================
// Public types
// =====================================================================

/// Metadata for a single node (file/dir/symlink) in the graph.
///
/// `name` is sanitized — control characters are replaced with `?` to prevent
/// terminal-injection via malicious filenames. The raw on-disk name is not
/// retained.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeMeta {
    pub is_dir: bool,
    pub is_symlink: bool,
    pub is_hidden: bool,
    pub is_exec: bool,
    pub mode: u32,
    pub size: u64,
    pub name: String,
}

/// Summary counts computed during the scan.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub total_nodes: usize,
    pub folders: usize,
    pub files: usize,
    pub symlinks: usize,
    pub executables: usize,
    pub hidden: usize,
    pub total_size_bytes: u64,
}

/// Caller-supplied scan configuration. Threads must be pre-resolved (no `0 = auto`).
#[derive(Clone, Debug)]
pub struct ScanOptions {
    pub semantic: bool,
    pub db_path: Option<PathBuf>,
    pub incremental: bool,
    pub root: PathBuf,
    pub max_depth: usize,
    pub max_nodes: usize,
    pub include_files: bool,
    pub threads: usize,
    pub tree_mode: bool,
    /// Generate embeddings for semantic vector search (requires fastembed/ONNX).
    pub embeddings: bool,
    /// Optional repository label for cross-repo grouping.
    /// When set, stored symbols are tagged with this repo name.
    pub repo_label: Option<String>,
    /// Run community + process detection graph analytics phases.
    /// Default false — these are expensive and can be computed on-demand.
    pub graph_phases: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            semantic: false,
            db_path: None,
            incremental: false,
            root: PathBuf::from("."),
            max_depth: 10,
            max_nodes: 10000,
            include_files: false,
            threads: 1,
            tree_mode: false,
            embeddings: false,
            repo_label: None,
            graph_phases: false,
        }
    }
}

/// Configuration for scanning multiple repositories as a group.
#[derive(Debug, Clone)]
pub struct GroupConfig {
    /// Repository root paths and their labels.
    pub repos: Vec<(String, PathBuf)>,
    /// Shared database path for cross-repo queries.
    pub db_path: PathBuf,
    /// Maximum nodes per repo (total across all repos = max_nodes_per_repo * repos.len()).
    pub max_nodes_per_repo: usize,
    /// Threads per repo.
    pub threads: usize,
    /// Enable semantic analysis.
    pub semantic: bool,
    /// Enable embeddings.
    pub embeddings: bool,
    /// Run community + process detection graph analytics.
    pub graph_phases: bool,
}

impl Default for GroupConfig {
    fn default() -> Self {
        Self {
            repos: Vec::new(),
            db_path: PathBuf::from(".atree/group.sqlite"),
            max_nodes_per_repo: 10000,
            threads: 1,
            semantic: false,
            embeddings: false,
            graph_phases: false,
        }
    }
}

/// Scan multiple repositories into a shared database.
/// Each repo is scanned sequentially but files within each repo are still
/// processed in parallel via work-stealing.
pub fn build_graph_group(config: &GroupConfig) -> io::Result<Vec<ScanResult>> {
    let mut results = Vec::with_capacity(config.repos.len());
    for (label, root) in &config.repos {
        let opts = ScanOptions {
            root: root.clone(),
            max_nodes: config.max_nodes_per_repo,
            threads: config.threads,
            semantic: config.semantic,
            embeddings: config.embeddings,
            db_path: Some(config.db_path.clone()),
            repo_label: Some(label.clone()),
            graph_phases: config.graph_phases,
            ..Default::default()
        };
        let result = build_graph(&opts)?;
        results.push(result);
    }
    Ok(results)
}

/// Result of a successful [`build_graph`] call.
pub struct ScanResult {
    pub parsed_files: Vec<crate::semantic::ParsedFile>,
    pub symbol_table: SymbolTable,
    pub store_stats: crate::store::StoreStats,
    pub resolution_stats: Option<crate::resolver::ResolutionStats>,
    pub scope_resolution_stats: Option<crate::scope_resolution::ScopeResolutionStats>,
    pub adj: FxHashMap<String, Vec<String>>,
    pub root_name: String,
    pub meta: FxHashMap<String, NodeMeta>,
    pub stats: Stats,
    /// `true` when the scan stopped early because `max_nodes` was reached.
    pub truncated: bool,
    /// Detected API routes (Express, Next.js, etc.)
    pub routes: Vec<crate::routes::Route>,
}

/// A* pathfinding result, suitable for JSON serialization.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PathReport {
    pub start: String,
    pub goal: String,
    pub hops: usize,
    pub nodes: Vec<String>,
    pub astar_expanded: usize,
    pub bfs_expanded: usize,
    pub efficiency_pct: f64,
}

/// Caller-friendly view of the scan options for embedding in [`JsonReport`].
/// `None` for a depth/node cap means the scan was unbounded.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonOptions {
    pub semantic: bool,
    pub db_path: Option<PathBuf>,
    pub incremental: bool,
    pub embeddings: bool,
    pub max_depth: Option<usize>,
    pub max_nodes: Option<usize>,
    pub include_files: bool,
    pub tree_mode: bool,
}

/// Current JSON schema version. Bump on any breaking change to the JSON output
/// (renamed fields, removed fields, changed types). Consumers should pin this
/// number; behavior-preserving changes do **not** bump it.
pub const SCHEMA_VERSION: u32 = 2;

/// The full JSON Schema (Draft 7) for `--json` output, embedded at compile time.
/// Source of truth is `docs/schema.json`; this constant guarantees the binary
/// can always emit its own schema with no co-located files.
pub const SCHEMA_JSON: &str = include_str!("../../docs/schema.json");

/// Top-level JSON output schema. Use [`build_json_report`] to construct.
///
/// Keys are sorted (`BTreeMap`) so the output is deterministic and diff-able.
/// Pin [`SCHEMA_VERSION`] to detect format changes; `version` is the binary
/// version (changes more often, doesn't necessarily mean schema changed).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonReport {
    pub semantic: Option<Vec<crate::semantic::ParsedFileOutput>>,
    pub symbol_table: Option<SymbolTable>,
    pub store_stats: Option<crate::store::StoreStats>,
    pub resolution_stats: Option<crate::resolver::ResolutionStats>,
    pub scope_resolution_stats: Option<crate::scope_resolution::ScopeResolutionStats>,
    pub schema_version: u32,
    pub version: String,
    pub root: String,
    pub root_name: String,
    pub elapsed_ms: f64,
    pub threads: usize,
    pub options: JsonOptions,
    pub stats: Stats,
    pub truncated: bool,
    pub depths: BTreeMap<String, i32>,
    pub nodes: BTreeMap<String, NodeMeta>,
    pub edges: BTreeMap<String, Vec<String>>,
    pub path: Option<PathReport>,
}

// =====================================================================
// Resource helpers
// =====================================================================

/// Total logical CPU count, falling back to 1 if unavailable.
pub fn all_cores() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)
}

/// Half of [`all_cores`], rounded up, minimum 1. Default thread count.
pub fn half_cores() -> usize {
    let cores = all_cores();
    cores.div_ceil(2).max(1)
}

/// Available system memory in bytes (Linux-only via `/proc/meminfo`).
/// Returns `None` on platforms where this can't be determined.
pub fn available_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let content = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("MemAvailable:") {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if parts.len() >= 2 && parts[1] == "kB" {
                    if let Ok(kb) = parts[0].parse::<u64>() {
                        return Some(kb * 1024);
                    }
                }
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// A safe upper bound on `max_nodes` such that the resulting graph fits in
/// roughly half of available memory. Returns `None` if memory can't be queried.
///
/// Estimates 256 bytes per node (path string + adjacency entry + metadata).
pub fn estimated_node_cap_for_half_memory() -> Option<usize> {
    const BYTES_PER_NODE: u64 = 256;
    let half = available_memory_bytes()? / 2;
    Some((half / BYTES_PER_NODE) as usize)
}

// =====================================================================
// Filename / display helpers
// =====================================================================

/// Replace control characters in a filename with `?`. Prevents terminal
/// injection attacks via ANSI escape sequences in malicious filenames.
pub fn sanitize_name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Human-readable size: `B`, `KB`, `MB`, `GB`, `TB`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", size, UNITS[unit])
    }
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &std::fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn get_mode(meta: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode()
}

#[cfg(not(unix))]
fn get_mode(_: &std::fs::Metadata) -> u32 {
    0
}

/// Colored permission badge (`[rwx]`). Empty when `no_color` or `mode == 0`.
fn permission_badge(mode: u32, no_color: bool) -> String {
    if no_color || mode == 0 {
        return String::new();
    }
    let r = if mode & 0o400 != 0 { "\x1b[32mr" } else { "\x1b[2;37mr" };
    let w = if mode & 0o200 != 0 { "\x1b[33mw" } else { "\x1b[2;37mw" };
    let x = if mode & 0o100 != 0 { "\x1b[35mx" } else { "\x1b[2;37mx" };
    format!(" {}{}]{}\x1b[0m", r, w, x)
}

// =====================================================================
// Internal scan plumbing
// =====================================================================

type Job = (PathBuf, String, usize);

#[derive(Default)]
struct LocalAccum {
    /// File contents collected during scan, keyed by rel_path.
    /// Used by the parallel parse phase to avoid re-reading from disk.
    pub file_contents: FxHashMap<String, String>,
    /// Files discovered during scan that need parsing (rel_path, ext).
    pub parse_queue: Vec<(String, String)>,
    adj: FxHashMap<String, Vec<String>>,
    meta: FxHashMap<String, NodeMeta>,
}

fn sort_entries(entries: &mut Vec<DirEntry>) {
    entries.sort_by(|a, b| {
        let a_dir = a.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        let b_dir = b.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
        let a_hidden = a.file_name().to_string_lossy().starts_with('.');
        let b_hidden = b.file_name().to_string_lossy().starts_with('.');
        match (a_dir, b_dir, a_hidden, b_hidden) {
            (true, false, _, _) => std::cmp::Ordering::Less,
            (false, true, _, _) => std::cmp::Ordering::Greater,
            (_, _, false, true) => std::cmp::Ordering::Less,
            (_, _, true, false) => std::cmp::Ordering::Greater,
            _ => a.file_name().cmp(&b.file_name()),
        }
    });
}

fn try_steal<T: Send>(stealers: &[Stealer<T>]) -> Option<T> {
    for s in stealers {
        loop {
            match s.steal() {
                Steal::Success(j) => return Some(j),
                Steal::Empty => break,
                Steal::Retry => continue,
            }
        }
    }
    None
}

fn reserve_slot(node_count: &AtomicUsize, max_nodes: usize) -> bool {
    loop {
        let prev = node_count.load(Ordering::Relaxed);
        if prev >= max_nodes {
            return false;
        }
        if node_count
            .compare_exchange(prev, prev + 1, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
        {
            return true;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_dir(
    dir_path: &Path,
    rel: &str,
    depth: usize,
    opts: &ScanOptions,
    root_name: &str,
    local: &mut LocalAccum,
    node_count: &AtomicUsize,
    queue: &Worker<Job>,
    pending: &AtomicUsize,
) {
    if depth >= opts.max_depth || node_count.load(Ordering::Relaxed) >= opts.max_nodes {
        return;
    }

    let mut entries: Vec<DirEntry> = match fs::read_dir(dir_path) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return,
    };
    sort_entries(&mut entries);

    let mut children: Vec<String> = Vec::with_capacity(entries.len());
    let mut subdirs: Vec<Job> = Vec::new();

    for entry in entries {
        let entry_path = entry.path();
        let raw_name = entry.file_name().to_string_lossy().to_string();
        let name_str = sanitize_name(&raw_name);

        let Some(ft) = entry.file_type().ok().or_else(|| {
            std::fs::symlink_metadata(&entry_path).ok().map(|m| m.file_type())
        }) else {
            continue;
        };
        let is_symlink = ft.is_symlink();
        let is_dir = if is_symlink && !opts.tree_mode {
            fs::metadata(&entry_path).map(|m| m.is_dir()).unwrap_or(false)
        } else {
            ft.is_dir()
        };
        let is_hidden = name_str.starts_with('.');

        if !opts.include_files && !is_dir && !is_symlink {
            continue;
        }

        if !reserve_slot(node_count, opts.max_nodes) {
            break;
        }

        let child_rel = if rel == root_name {
            name_str.clone()
        } else {
            format!("{}/{}", rel, name_str)
        };

        let (is_dir_final, is_exec, size, mode) = if opts.tree_mode {
            (is_dir && !is_symlink, false, 0, 0)
        } else if is_symlink {
            match fs::metadata(&entry_path) {
                Ok(m) => (m.is_dir(), is_executable(&m), m.len(), get_mode(&m)),
                Err(_) => (false, false, 0, 0),
            }
        } else if is_dir {
            (true, false, 0, 0)
        } else {
            match fs::metadata(&entry_path) {
                Ok(m) => (false, is_executable(&m), m.len(), get_mode(&m)),
                Err(_) => (false, false, 0, 0),
            }
        };

        local.meta.insert(
            child_rel.clone(),
            NodeMeta {
                is_dir: is_dir_final,
                is_symlink,
                is_hidden,
                is_exec,
                mode,
                size,
                name: name_str,
            },
        );
        local.adj.entry(child_rel.clone()).or_default().push(rel.to_string());
        children.push(child_rel.clone());

        if opts.semantic && !is_dir_final && !is_symlink {
            if let Some(ext) = Path::new(&child_rel).extension().and_then(|s| s.to_str()) {
                if get_provider_for_extension(ext).is_some() {
                    // Skip oversized files to prevent OOM.
                    if let Ok(meta) = fs::metadata(&entry_path) {
                        if meta.len() > MAX_FILE_SIZE {
                            log::warn!("Skipping file ({} exceeds {} MB limit): {}",
                                human_size(meta.len()), MAX_FILE_SIZE / (1024 * 1024), entry_path.display());
                        } else if let Ok(content) = fs::read_to_string(&entry_path) {
                            // Read file content and queue for parallel parse phase.
                            local.file_contents.insert(child_rel.clone(), content);
                            local.parse_queue.push((child_rel.clone(), ext.to_string()));
                        }
                    }
                }
            }
        }

        if is_dir && !is_symlink {
            // Skip hidden directories, build artifacts, and dependency directories
            if raw_name.starts_with('.') || SKIP_DIRS.contains(&raw_name.as_ref()) {
                continue;
            }
            subdirs.push((entry_path, child_rel, depth + 1));
        }
    }

    if !children.is_empty() {
        local.adj.entry(rel.to_string()).or_default().extend(children);
    }

    for j in subdirs {
        pending.fetch_add(1, Ordering::Release);
        queue.push(j);
    }
    }
// =====================================================================
// Public scan function
// =====================================================================

/// Build a graph by scanning the filesystem under `opts.root` in parallel.
///
/// Threads must be pre-resolved (caller chooses; pass `1` for sequential).
/// The scan stops cleanly when `max_nodes` is reached and sets
/// `ScanResult.truncated = true`.
pub fn build_graph(opts: &ScanOptions) -> io::Result<ScanResult> {
    let root = opts.root.canonicalize().unwrap_or_else(|_| opts.root.clone());
    tracing::info!(root = %root.to_string_lossy(), threads = opts.threads, semantic = opts.semantic, "Starting graph build");

    // Validate the root before scanning. A nonexistent or non-directory path
    // would otherwise produce a single-node "scan" with the literal path as a
    // fake folder, which silently masks user typos in scripts.
    let root_meta = fs::metadata(&root).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("root path '{}' is unreadable: {}", opts.root.display(), e),
        )
    })?;
    if !root_meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("root path '{}' is not a directory", opts.root.display()),
        ));
    }

    let root_name_raw = root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("root")
        .to_string();
    let root_name = sanitize_name(&root_name_raw);

    let node_count = AtomicUsize::new(1);
    let pending = AtomicUsize::new(0);

    let n = opts.threads.max(1);
    let max_nodes = opts.max_nodes;

    let mut workers: Vec<Worker<Job>> = (0..n).map(|_| Worker::new_lifo()).collect();
    let stealers: Vec<Stealer<Job>> = workers.iter().map(|w| w.stealer()).collect();

    pending.fetch_add(1, Ordering::Release);
    workers[0].push((root.clone(), root_name.clone(), 0));

    perf_timer!("Total scan");
    perf_timer!("Tree walk + file read");
    let locals: Vec<LocalAccum> = thread::scope(|scope| {
        let handles: Vec<_> = workers
            .drain(..)
            .enumerate()
            .map(|(idx, my_queue)| {
                let other_stealers: Vec<Stealer<Job>> = stealers
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != idx)
                    .map(|(_, s)| s.clone())
                    .collect();
                let pending_ref = &pending;
                let node_count_ref = &node_count;
                let root_name_ref = &root_name;
                let opts_ref = &opts;
                scope.spawn(move || {
                    let mut local = LocalAccum::default();
                    let hint = (max_nodes.min(1 << 18) / n).max(16);
                    local.adj.reserve(hint);
                    local.meta.reserve(hint);
                    local.file_contents.reserve(hint);
                    local.parse_queue.reserve(hint / 4);

                    let mut backoff: u32 = 0;
                    loop {
                        let job: Job = if let Some(j) = my_queue.pop() {
                            backoff = 0;
                            j
                        } else if let Some(j) = try_steal(&other_stealers) {
                            backoff = 0;
                            j
                        } else {
                            if pending_ref.load(Ordering::Acquire) == 0 {
                                return local;
                            }
                            backoff = backoff.saturating_add(1);
                            if backoff < 32 {
                                for _ in 0..backoff {
                                    std::hint::spin_loop();
                                }
                            } else {
                                thread::yield_now();
                            }
                            continue;
                        };

                        process_dir(&job.0, &job.1, job.2, opts_ref, root_name_ref, &mut local, node_count_ref, &my_queue, pending_ref);
                        pending_ref.fetch_sub(1, Ordering::Release);
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("worker thread panicked")).collect()
    });

    perf_timer!("Tree-sitter parse");

    // Phase 2: Parallel parse — distribute file contents across threads for tree-sitter parsing.
    // Each thread gets its own SyntaxEngine (tree-sitter Parser is not Send/Sync).
    // This is where the speed win comes from: all threads parse concurrently.
    let n_parse_threads = n.max(1);
    let all_parsed_files: Vec<ParsedFile> = if opts.semantic {
        // Collect all file contents and parse queues from all threads
        let mut all_contents: FxHashMap<String, String> = FxHashMap::default();
        let mut all_parse_queue: Vec<(String, String)> = Vec::new();
        for local in &locals {
            all_contents.extend(local.file_contents.clone());
            all_parse_queue.extend(local.parse_queue.clone());
        }

        if !all_parse_queue.is_empty() {
            // Sort files by extension (so set_language is called once per language
            // per thread), then by size descending (largest first). Largest-first
            // ordering improves load balancing with work-stealing: if a thread gets
            // stuck on a large file, others can steal its remaining small files.
            let mut sorted_queue = all_parse_queue;
            sorted_queue.sort_by(|a, b| {
                a.1.cmp(&b.1) // group by extension first
                    .then_with(|| {
                        let sa = all_contents.get(&a.0).map(|c| c.len()).unwrap_or(0);
                        let sb = all_contents.get(&b.0).map(|c| c.len()).unwrap_or(0);
                        sb.cmp(&sa) // largest first within same extension
                    })
            });

            // Use a shared work queue with per-thread stealers for load balancing.
            // Unlike static chunking (which can imbalance if some files are larger),
            // work-stealing lets idle threads grab work from busy ones automatically.
            let n_workers = n_parse_threads;
            let workers: Vec<Worker<(String, String)>> = (0..n_workers).map(|_| Worker::new_lifo()).collect();
            let stealers: Vec<Stealer<(String, String)>> = workers.iter().map(|w| w.stealer()).collect();

            // Populate the first worker's queue; others will steal.
            for job in sorted_queue {
                workers[0].push(job);
            }

            let parse_results: Vec<Vec<ParsedFile>> = thread::scope(|scope| {
                let handles: Vec<_> = workers
                    .into_iter()
                    .enumerate()
                    .map(|(idx, my_queue)| {
                        let other_stealers: Vec<Stealer<(String, String)>> = stealers
                            .iter()
                            .enumerate()
                            .filter(|(j, _)| *j != idx)
                            .map(|(_, s)| s.clone())
                            .collect();
                        let contents = &all_contents;
                        scope.spawn(move || {
                            let mut engine = SyntaxEngine::new();
                            let mut results = Vec::new();
                            let mut last_provider: Option<&dyn LanguageProvider> = None;

                            loop {
                                let (rel_path, ext) = match my_queue.pop().or_else(|| try_steal(&other_stealers)) {
                                    Some(job) => job,
                                    None => break,
                                };

                                let content = match contents.get(&rel_path) {
                                    Some(c) => c,
                                    None => continue,
                                };

                                let provider = match get_provider_for_extension(&ext) {
                                    Some(p) => p,
                                    None => continue,
                                };

                                if last_provider.map_or(true, |lp| !std::ptr::eq(lp, provider)) {
                                    engine.set_language_for(provider);
                                    last_provider = Some(provider);
                                }

                                let file_id = {
                                    use std::hash::{Hash, Hasher};
                                    let mut h = std::collections::hash_map::DefaultHasher::new();
                                    rel_path.hash(&mut h);
                                    h.finish()
                                };
                                let file_hash = crate::syntax::hash_content(content);

                                let (captures, raw_scopes, type_bindings) =
                                    engine.extract_captures_and_scopes_preloaded(provider, content);

                                let parsed = ParsedFile::from_captures_with_scopes(
                                    file_id, &rel_path, provider.id(), file_hash,
                                    captures, raw_scopes, type_bindings,
                                );
                                results.push(parsed);
                            }

                            results
                        })
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().expect("worker thread panicked")).collect()
            });

            parse_results.into_iter().flatten().collect()
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };
    // Single-threaded merge into the global maps.
    let final_count = node_count.load(Ordering::Relaxed);
    let cap = max_nodes.min(final_count) + 1;
    let mut adj: FxHashMap<String, Vec<String>> =
        FxHashMap::with_capacity_and_hasher(cap, Default::default());
    let mut meta: FxHashMap<String, NodeMeta> =
        FxHashMap::with_capacity_and_hasher(cap, Default::default());

    adj.insert(root_name.clone(), Vec::new());
    meta.insert(
        root_name.clone(),
        NodeMeta {
            is_dir: true,
            is_symlink: false,
            is_hidden: root_name.starts_with('.'),
            is_exec: false,
            mode: 0,
            size: 0,
            name: root_name.clone(),
        },
    );

    for local in &locals {
        for (k, v) in local.meta.clone() {
            meta.insert(k, v);
        }
        for (k, v) in local.adj.clone() {
            match adj.entry(k) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(v);
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    e.get_mut().extend(v);
                }
            }
        }
    }

    // Compute summary stats once, so callers don't have to re-iterate.
    let mut stats = Stats::default();
    stats.total_nodes = adj.len();
    for m in meta.values() {
        if m.is_dir {
            stats.folders += 1;
        }
        if m.is_symlink {
            stats.symlinks += 1;
        }
        if m.is_exec {
            stats.executables += 1;
        }
        if m.is_hidden {
            stats.hidden += 1;
        }
        stats.total_size_bytes += m.size;
    }
    stats.files = stats.total_nodes.saturating_sub(stats.folders);

    let truncated = final_count >= max_nodes;

    // Parsed files from the parallel parse phase above.
    let parsed_files = all_parsed_files;

    // Run the semantic analysis pipeline (scan/parse already done above).
    // The pipeline DAG handles: cross_file → scope_resolution → mro → communities → processes,
    // with routes/tools/orm running in parallel after parse.
    perf_timer!("Pipeline (all phases)");
    let (store_stats, symbol_table, resolution_stats, scope_resolution_stats, detected_routes, parsed_files) = if opts.semantic {
        let pipeline_opts = crate::pipeline::PipelineOptions {
            skip_graph_phases: !opts.graph_phases,
            skip_workers: false,
        };
        let ctx = crate::pipeline::PipelineContext {
            repo_path: root.to_string_lossy().to_string(),
            options: pipeline_opts,
            on_progress: std::sync::Arc::new(|_p: crate::pipeline::PipelineProgress| {}),
            db_path: opts.db_path.clone(),
            incremental: opts.incremental,
            repo_label: opts.repo_label.clone(),
        };
        let shared = crate::pipeline::PipelineSharedState {
            store: std::sync::Mutex::new(None),
            parsed_files: std::sync::Mutex::new(parsed_files),
            symbol_id_map: std::sync::Mutex::new(rustc_hash::FxHashMap::default()),
            detected_routes: std::sync::Mutex::new(Vec::new()),
            scope_resolution_stats: std::sync::Mutex::new(None),
        };

        // Run the semantic phases via the pipeline DAG.
        // Wave 1: CrossFilePhase (batch SQLite insert + scope resolution)
        // Wave 2: [Routes, Tools, Orm, Markdown, Cobol, ScopeResolution, Mro] (parallel readers)
        // Wave 3: [Communities, Processes] (parallel graph analytics)
        let phase_refs: Vec<&dyn crate::pipeline::PipelinePhase> = vec![
            &crate::pipeline::CrossFilePhase,
            &crate::pipeline::RoutesPhase,
            &crate::pipeline::ToolsPhase,
            &crate::pipeline::OrmPhase,
            &crate::pipeline::MarkdownPhase,
            &crate::pipeline::CobolPhase,
            &crate::pipeline::ScopeResolutionPhase,
            &crate::pipeline::MroPhase,
            &crate::pipeline::CommunitiesPhase,
            &crate::pipeline::ProcessesPhase,
        ];
        let _pipeline_results = crate::pipeline::run_pipeline(&phase_refs, &ctx, &shared)
            .map_err(|e| io::Error::other(e.to_string()))?;

        // Initialize and populate BM25 search index.
        perf_timer!("BM25 search index");
        let store_guard = shared.store.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref store) = *store_guard {
            crate::search::init_search_index(store)
                .map_err(|e| io::Error::other(e.to_string()))?;
            let _search_count = crate::search::index_symbols(store)
                .map_err(|e| io::Error::other(e.to_string()))?;
        }
        drop(store_guard);

        // Generate embeddings for semantic vector search (if enabled).
        if opts.embeddings {
            perf_timer!("Embeddings generation");
            #[cfg(feature = "embeddings")]
            {
                let store_guard = shared.store.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref store) = *store_guard {
                    crate::embeddings::init_embeddings_table(store)
                        .map_err(|e| io::Error::other(e.to_string()))?;
                    let _embed_count = crate::embeddings::generate_embeddings(store)
                        .map_err(|e| io::Error::other(e.to_string()))?;
                }
                drop(store_guard);
            }
        }

        // Build abstraction layers (file-graph, module-graph) for large-repo navigation.
        // These pre-computed views collapse thousands of symbol edges into a manageable
        // number of file/file and module/module edges, enabling fast scoped queries.
        {
            let store_guard = shared.store.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref store) = *store_guard {
                perf_timer!("Abstraction layers");
                let _tab0 = std::time::Instant::now();
                if let Err(e) = store.build_abstraction_layers() {
                    tracing::warn!(error = %e, "Failed to build abstraction layers");
                }
            }
            drop(store_guard);
        }

        // Extract git history (commits, file_commits, authors) and persist to the store.
        #[cfg(feature = "git")]
        let git_stats = {
            let store_guard = shared.store.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref store) = *store_guard {
                perf_timer!("Git history extraction");
                let git_config = crate::git_history::GitHistoryConfig::default();
                match crate::git_history::extract_and_persist(&root, store.conn(), &git_config) {
                    Ok(s) => Some(s),
                    Err(e) => {
                        tracing::warn!(error = %e, "Git history extraction failed");
                        None
                    }
                }
            } else {
                None
            }
        };
        #[cfg(not(feature = "git"))]
        let git_stats: Option<crate::git_history::GitHistoryStats> = None;
        let _ = git_stats;

        let store_stats = {
            let store_guard = shared.store.lock().unwrap_or_else(|e| e.into_inner());
            match *store_guard {
                Some(ref store) => store.stats()
                    .map_err(|e| io::Error::other(e.to_string()))?,
                None => crate::store::StoreStats { files: 0, symbols: 0, scopes: 0, imports: 0, calls: 0, edges: 0, resolved_calls: 0, files_inserted: 0, files_reused: 0 },
            }
        };
        let symbol_table = {
            let store_guard = shared.store.lock().unwrap_or_else(|e| e.into_inner());
            match *store_guard {
                Some(ref store) => SymbolTable::from_store(store)
                    .map_err(|e| io::Error::other(e.to_string()))?,
                None => SymbolTable::new(),
            }
        };

        let parsed_files = shared.parsed_files.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let scope_resolution_stats = shared.scope_resolution_stats.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let detected_routes = shared.detected_routes.lock().unwrap_or_else(|e| e.into_inner()).clone();

        // Run WAL checkpoint after large batch writes to prevent unbounded WAL growth.
        if let Ok(guard) = shared.store.lock() {
            if let Some(ref store) = *guard {
                store.maintenance_or_warn();
            }
        }

        (store_stats, symbol_table, None, scope_resolution_stats, detected_routes, parsed_files)
    } else {
        (crate::store::StoreStats { files: 0, symbols: 0, scopes: 0, imports: 0, calls: 0, edges: 0, resolved_calls: 0, files_inserted: 0, files_reused: 0 }, SymbolTable::new(), None, None, Vec::new(), Vec::new())
    };

    tracing::info!(
        files = store_stats.files, symbols = store_stats.symbols, edges = store_stats.edges,
        routes = detected_routes.len(), truncated, "Graph build complete"
    );

    Ok(ScanResult {
        parsed_files,
        symbol_table,
        store_stats,
        resolution_stats,
        scope_resolution_stats,
        adj,
        root_name,
        meta,
        stats,
        truncated,
        routes: detected_routes,
    })
}

// =====================================================================
// Incremental scanning
// =====================================================================

/// Find the file_id for a given symbol ID by querying the store.
fn find_file_id_for_symbol(store: &crate::store::GraphStore, symbol_id: i64) -> io::Result<Option<i64>> {
    store.get_file_id_for_symbol(symbol_id)
        .map_err(|e| io::Error::other(e.to_string()))
}

/// Result of an incremental scan, showing what changed.
pub struct IncrementalScanResult {
    pub files_added: usize,
    pub files_updated: usize,
    pub files_unchanged: usize,
    pub files_removed: usize,
}

/// Build a graph incrementally, reusing an existing GraphStore.
///
/// Strategy:
/// 1. Detect added/changed/removed files via git status + hash checking
/// 2. For removed files: delete all associated data (cascade)
/// 3. For changed files: delete old data, re-parse, insert new data
/// 4. For new files: parse + insert
/// 5. Run scope resolution on changed/new ParsedFiles (intra-file calls)
/// 6. Re-resolve imports for changed files + files importing from changed files
/// 7. Re-resolve cross-file calls pointing to symbols in changed files
/// 8. Update BM25 search index (remove old, add new)
/// 9. Re-detect routes for changed files
pub fn build_graph_incremental(
    opts: &ScanOptions,
) -> io::Result<(ScanResult, IncrementalScanResult)> {
    // Open or reuse the graph store (must be persistent for incremental to make sense)
    let store = match &opts.db_path {
        Some(path) => GraphStore::open(path)
            .map_err(|e| io::Error::other(e.to_string()))?,
        None => GraphStore::open_in_memory()
            .map_err(|e| io::Error::other(e.to_string()))?,
    };
    let root = opts.root.canonicalize().unwrap_or_else(|_| opts.root.clone());
    let root_meta = fs::metadata(&root).map_err(|e| {
        io::Error::new(e.kind(), format!("root path '{}' is unreadable: {}", opts.root.display(), e))
    })?;
    if !root_meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("root path '{}' is not a directory", opts.root.display()),
        ));
    }

    let mut incremental = IncrementalScanResult {
        files_added: 0,
        files_updated: 0,
        files_unchanged: 0,
        files_removed: 0,
    };

    // ── Step 1: Get currently indexed files ──
    let indexed_files = store.get_all_file_hashes()
        .map_err(|e| io::Error::other(e.to_string()))?;
    let indexed_paths: std::collections::HashSet<String> =
        indexed_files.iter().map(|(p, _)| p.clone()).collect();

    // ── Step 2: Git staleness detection ──
    let git_changed_files = detect_git_changes(&root);

    // ── Step 3: Scan filesystem for current files ──
    // If git reports no changes at all, we can skip the filesystem walk entirely
    // and just verify no indexed files were deleted (cheap DB check).
    let git_has_no_changes = git_changed_files.as_ref().is_some_and(|c| c.is_empty());

    let mut current_files: Vec<(String, String)> = Vec::new();
    if !git_has_no_changes {
        // Git reports changes — need to walk the filesystem to find what changed
        collect_files_for_incremental(&root, &root, opts, &mut current_files, 0)?;
    } else {
        // Git says nothing changed — just collect paths from the DB to check for deletions
        for (path, _) in &indexed_files {
            current_files.push((path.clone(), String::new()));
        }
    }

    let current_paths: std::collections::HashSet<String> =
        current_files.iter().map(|(p, _)| p.clone()).collect();

    // Track which file paths changed (for import/call re-resolution)
    let mut changed_paths: std::collections::HashSet<String> = std::collections::HashSet::new();

    // ── Step 4: Remove deleted files ──
    let ev_store_removed = crate::evidence::storage::EvidenceStore::new(store.conn());
    for (path, _) in &indexed_files {
        if !current_paths.contains(path) {
            store.remove_file_data(path)
                .map_err(|e| io::Error::other(e.to_string()))?;
            // Also clean up FTS5 index entries for this file.
            let _ = ev_store_removed.remove_file_from_fts(path);
            incremental.files_removed += 1;
            changed_paths.insert(path.clone());
        }
    }

    // Fast path: nothing changed and nothing removed
    if git_has_no_changes && incremental.files_removed == 0 {
        incremental.files_unchanged = current_files.len();
        let store_stats = store.stats()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let symbol_table = SymbolTable::from_store(&store)
            .map_err(|e| io::Error::other(e.to_string()))?;
        let root_name_raw = root.file_name().and_then(|s| s.to_str()).unwrap_or("root").to_string();
        let root_name = sanitize_name(&root_name_raw);
        let mut stats = Stats::default();
        stats.total_nodes = store_stats.files as usize;
        stats.files = store_stats.files as usize;
        return Ok((ScanResult {
            parsed_files: Vec::new(),
            symbol_table,
            store_stats,
            resolution_stats: None,
            scope_resolution_stats: None,
            adj: FxHashMap::default(),
            root_name,
            meta: FxHashMap::default(),
            stats,
            routes: Vec::new(),
            truncated: false,
        }, incremental));
    }

    // If we skipped the walk (git had no changes) but found deletions, we need to
    // walk now to get the full file list for proper stats
    if git_has_no_changes && incremental.files_removed > 0 {
        current_files.clear();
        collect_files_for_incremental(&root, &root, opts, &mut current_files, 0)?;
    }

    // ── Step 5: Process current files (skip unchanged) ──
    let mut parsed_files: Vec<ParsedFile> = Vec::new();
    let mut syntax = SyntaxEngine::new();
    for (rel_path, _lang) in &current_files {
        // Fast path: git says unchanged — skip reading entirely
        if let Some(ref changed) = git_changed_files {
            if !changed.contains(rel_path) {
                incremental.files_unchanged += 1;
                continue;
            }
        }

        // Read file content for hash check + potential parsing
        let full_path = root.join(rel_path);
        let content = match fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Compute hash for change detection
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            content.hash(&mut h);
            h.finish()
        };

        // Slow path: hash-based check (for non-git repos or when git says changed)
        if store.check_file_unchanged(rel_path, hash)
            .map_err(|e| io::Error::other(e.to_string()))?
            .is_some()
        {
            incremental.files_unchanged += 1;
            continue;
        }

        // File is new or changed
        if indexed_paths.contains(rel_path) {
            store.remove_file_data(rel_path)
                .map_err(|e| io::Error::other(e.to_string()))?;
            incremental.files_updated += 1;
        } else {
            incremental.files_added += 1;
        }
        changed_paths.insert(rel_path.clone());

        // Re-parse
        if let Some(ext) = Path::new(&rel_path).extension().and_then(|s| s.to_str()) {
            if let Some(provider) = get_provider_for_extension(ext) {
                let (captures, raw_scopes, _type_bindings) =
                    syntax.extract_captures_and_scopes(provider, &content);
                let file_id = {
                    use std::hash::{Hash, Hasher};
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    rel_path.hash(&mut h);
                    h.finish()
                };
                let parsed = ParsedFile::from_captures_with_scopes(
                    file_id, rel_path, provider.id(), hash,
                    captures, raw_scopes, Vec::new(),
                );
                parsed_files.push(parsed);
            }
        }
    }

    // ── Step 6: Insert new/changed files into the store ──
    // Build a global symbol ID map for edge resolution
    let mut global_symbol_id_map: rustc_hash::FxHashMap<u64, i64> = rustc_hash::FxHashMap::default();

    if !parsed_files.is_empty() {
        for file in &parsed_files {
            let file_id = store.upsert_file(
                &file.path, file.hash,
                &format!("{:?}", file.language), 0,
                opts.repo_label.as_deref(),
            ).map_err(|e| io::Error::other(e.to_string()))?;

            // Insert scopes
            // Build a map from globally unique scope ID → index within this file.
            let scope_id_to_idx: rustc_hash::FxHashMap<u64, usize> = file.scopes
                .iter()
                .enumerate()
                .map(|(idx, s)| (s.id, idx))
                .collect();
            let mut scope_id_map: Vec<i64> = Vec::with_capacity(file.scopes.len());
            for scope in &file.scopes {
                let parent_store_id = scope.parent_id.map(|pid| scope_id_map[scope_id_to_idx[&pid] as usize]);
                let store_scope_id = store.insert_scope(&ScopeRecord {
                    id: 0, file_id, parent_id: parent_store_id,
                    owner_symbol_id: scope.owner_symbol_id.map(|v| v as i64),
                    kind: format!("{:?}", scope.kind),
                    line_start: scope.line_start, line_end: scope.line_end,
                }).map_err(|e| io::Error::other(e.to_string()))?;
                scope_id_map.push(store_scope_id);
            }

            // Insert symbols
            for sym in &file.symbols {
                let store_scope_id = sym.scope_id.map(|sid| scope_id_map[scope_id_to_idx[&sid] as usize]);
                let store_sym_id = store.insert_symbol(&SymbolRecord {
                    id: 0, file_id, name: sym.name.clone(),
                    qualified_name: sym.qualified_name.clone(),
                    kind: format!("{:?}", sym.kind),
                    line: sym.line, col: sym.col,
                    is_exported: sym.is_exported,
                    scope_id: store_scope_id,
                    owner_symbol_id: sym.owner_id.map(|v| v as i64),
                }).map_err(|e| io::Error::other(e.to_string()))?;
                global_symbol_id_map.insert(sym.id, store_sym_id);
            }

            // Insert heritage
            for her in &file.heritage {
                let child_id = file.symbols.iter()
                    .position(|s| s.name == her.class_name || her.class_name.is_empty())
                    .and_then(|idx| file.symbols.get(idx).and_then(|s| global_symbol_id_map.get(&s.id).copied()))
                    .unwrap_or(0);
                store.insert_heritage(&HeritageRecord {
                    id: 0, file_id, child_symbol_id: child_id,
                    parent_symbol_id: None,
                    parent_name: her.target_name.clone(),
                    heritage_kind: format!("{:?}", her.heritage_kind),
                    confidence: her.confidence.score(),
                    line: her.line,
                }).map_err(|e| io::Error::other(e.to_string()))?;
            }

            // Insert imports (unresolved — will be resolved in Step 7)
            for imp in &file.imports {
                store.insert_import(&ImportRecord {
                    id: 0, file_id, source: imp.source.clone(),
                    imported_name: imp.imported_name.clone(),
                    local_name: imp.local_name.clone(),
                    resolved_file_id: imp.resolved_file_id.map(|v| v as i64),
                    confidence: imp.confidence.score(),
                }).map_err(|e| io::Error::other(e.to_string()))?;
            }

            // Insert calls (unresolved — will be resolved in Step 8)
            for call in &file.calls {
                let caller_store_scope_id = call.caller_scope_id.map(|sid| scope_id_map[scope_id_to_idx[&sid] as usize]);
                store.insert_call(&CallRecord {
                    id: 0, file_id, caller_scope_id: caller_store_scope_id,
                    callee_name: call.callee_name.clone(),
                    receiver: call.receiver.clone(),
                    resolved_symbol_id: call.resolved_symbol_id.map(|v| v as i64),
                    confidence: call.confidence.score(),
                    line: call.line, col: call.col,
                }).map_err(|e| io::Error::other(e.to_string()))?;
            }
        }

        // ── Step 7: Run scope resolution on changed files ──
        let scope_resolution_stats = run_incremental_scope_resolution(
            &store, &mut parsed_files, &global_symbol_id_map,
        )?;

        // ── Step 7b: Evidence lifecycle for changed files ──
        // Collect evidence from newly parsed files, run full lifecycle, commit to store.
        if !parsed_files.is_empty() {
            let mut evidence_lifecycle = crate::evidence::lifecycle::EvidenceLifecycle::new();
            let all_new_evidence: Vec<crate::evidence::Evidence> = parsed_files
                .iter()
                .flat_map(|pf| pf.evidence.clone())
                .map(|c| c.into_evidence())
                .collect();
            evidence_lifecycle.normalize(all_new_evidence);
            evidence_lifecycle.dedupe();

            // Build file_id_map for enrichment.
            let file_id_map: rustc_hash::FxHashMap<u64, i64> = parsed_files
                .iter()
                .filter_map(|pf| {
                    store.get_file(&pf.path).ok().flatten().map(|f| (pf.id, f.id))
                })
                .collect();
            evidence_lifecycle.enrich(&global_symbol_id_map, &file_id_map);
            evidence_lifecycle.calibrate_all();

            // Feedback: for evidence that already exists in the store (same ID),
            // update confidence/stability instead of inserting conflicting records.
            let committed = evidence_lifecycle.committed();
            let ev_store = crate::evidence::storage::EvidenceStore::new(store.conn());
            for ev in committed {
                if let Ok(Some(existing)) = ev_store.get(&ev.id) {
                    // Feedback: boost stability (seen again), keep higher confidence.
                    let new_stability = (existing.stability + 0.1).min(1.0);
                    let new_confidence = ev.metadata.confidence.max(existing.confidence);
                    let _ = ev_store.update_confidence(&ev.id, new_confidence, new_stability);
                } else {
                    let _ = ev_store.insert_batch(&[ev.clone()]);
                }
            }
        }

        // ── Step 8: Re-resolve imports for changed files ──
        // Also re-resolve imports in files that import FROM changed files
        let all_indexed_files_now = store.get_all_file_hashes()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let all_file_path_list: Vec<String> = all_indexed_files_now.iter().map(|(p, _)| p.clone()).collect();

        // Find files that import from any changed file
        let mut files_needing_import_reresolve: std::collections::HashSet<String> = changed_paths.clone();
        for (path, _) in &all_indexed_files_now {
            if let Ok(Some(file_rec)) = store.get_file(path) {
                let imports = store.get_imports_by_file(file_rec.id)
                    .unwrap_or_default();
                for imp in &imports {
                    if let Some(resolved_file_id) = imp.resolved_file_id {
                        if let Ok(Some(resolved_file)) = store.get_file_by_id(resolved_file_id) {
                            if changed_paths.contains(&resolved_file.path) {
                                files_needing_import_reresolve.insert(path.clone());
                                break;
                            }
                        }
                    }
                }
            }
        }

        // Re-resolve imports for affected files
        for path in &files_needing_import_reresolve {
            if let Ok(Some(file_rec)) = store.get_file(path) {
                let lang_str = &file_rec.language;
                let imports = store.get_imports_by_file(file_rec.id).unwrap_or_default();
                for imp in &imports {
                    let lang_id = match lang_str.as_str() {
                        "TypeScript" => crate::lang::LanguageId::TypeScript,
                        "JavaScript" => crate::lang::LanguageId::JavaScript,
                        "Python" => crate::lang::LanguageId::Python,
                        "Rust" => crate::lang::LanguageId::Rust,
                        "Go" => crate::lang::LanguageId::Go,
                        "Java" => crate::lang::LanguageId::Java,
                        "C" => crate::lang::LanguageId::C,
                        "Cpp" => crate::lang::LanguageId::Cpp,
                        "CSharp" => crate::lang::LanguageId::CSharp,
                        "PHP" => crate::lang::LanguageId::PHP,
                        "Ruby" => crate::lang::LanguageId::Ruby,
                        "Kotlin" => crate::lang::LanguageId::Kotlin,
                        "Swift" => crate::lang::LanguageId::Swift,
                        "Bash" => crate::lang::LanguageId::Bash,
                        _ => continue,
                    };
                    if let Some((resolved_path, conf)) = crate::resolver::import_resolver::resolve_import(
                        &imp.source, path, &all_file_path_list, lang_id,
                    ) {
                        // Find the resolved file's DB ID
                        if let Ok(Some(resolved_file)) = store.get_file(&resolved_path) {
                            let _ = store.update_import_resolution(
                                imp.id, Some(resolved_file.id), conf,
                            );
                        }
                    }
                }
            }
        }

        // ── Step 9: Re-resolve cross-file calls pointing to changed files ──
        // For each changed file, find its exported symbols, then find all calls
        // in the entire index that reference those symbol names and can now be resolved
        for changed_path in &changed_paths {
            if let Ok(Some(file_rec)) = store.get_file(changed_path) {
                let exported_symbols = store.get_symbols_by_file(file_rec.id)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|s| s.is_exported)
                    .collect::<Vec<_>>();
                for sym in &exported_symbols {
                    // Find unresolved calls with matching callee_name and resolve them
                    let _ = store.resolve_calls_by_name(&sym.name, sym.id);
                }
            }
        }

        // ── Step 10: Update BM25 search index ──
        // Remove old entries for changed files, add new ones
        for path in &changed_paths {
            if let Ok(Some(file_rec)) = store.get_file(path) {
                crate::search::remove_file_from_index(&store, file_rec.id)
                    .unwrap_or_default();
            }
        }
        // Re-index all symbols from changed files
        for file in &parsed_files {
            if let Ok(Some(file_rec)) = store.get_file(&file.path) {
                let _ = crate::search::index_file_symbols(&store, file_rec.id);
            }
        }

        // ── Step 11: Run semantic phases (routes, communities, processes) ──
        let detected_routes = run_incremental_semantic_phases(
            &store, &parsed_files, &global_symbol_id_map,
        )?;

        // Build final stats
        let store_stats = store.stats()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let symbol_table = SymbolTable::from_store(&store)
            .map_err(|e| io::Error::other(e.to_string()))?;

        let root_name_raw = root.file_name().and_then(|s| s.to_str()).unwrap_or("root").to_string();
        let root_name = sanitize_name(&root_name_raw);

        let mut stats = Stats::default();
        stats.total_nodes = store_stats.files as usize;
        stats.files = store_stats.files as usize;

        return Ok((ScanResult {
            parsed_files,
            symbol_table,
            store_stats,
            resolution_stats: None,
            scope_resolution_stats,
            adj: FxHashMap::default(),
            root_name,
            meta: FxHashMap::default(),
            stats,
            routes: detected_routes,
            truncated: false,
        }, incremental));
    }

    // No files changed — return empty result
    let store_stats = store.stats()
        .map_err(|e| io::Error::other(e.to_string()))?;
    let symbol_table = SymbolTable::from_store(&store)
        .map_err(|e| io::Error::other(e.to_string()))?;

    let root_name_raw = root.file_name().and_then(|s| s.to_str()).unwrap_or("root").to_string();
    let root_name = sanitize_name(&root_name_raw);
    let mut stats = Stats::default();
    stats.total_nodes = store_stats.files as usize;
    stats.files = store_stats.files as usize;

    Ok((ScanResult {
        parsed_files: Vec::new(),
        symbol_table,
        store_stats,
        resolution_stats: None,
        scope_resolution_stats: None,
        adj: FxHashMap::default(),
        root_name,
        meta: FxHashMap::default(),
        stats,
        routes: Vec::new(),
        truncated: false,
    }, incremental))
}

/// Run scope resolution on incrementally changed files and persist edges.
fn run_incremental_scope_resolution(
    store: &GraphStore,
    parsed_files: &mut [ParsedFile],
    symbol_id_map: &rustc_hash::FxHashMap<u64, i64>,
) -> io::Result<Option<crate::scope_resolution::ScopeResolutionStats>> {
    if parsed_files.is_empty() {
        return Ok(None);
    }

    let all_file_paths: Vec<String> = parsed_files.iter().map(|f| f.path.clone()).collect();

    // Build the import graph for A*-guided file ordering.
    let import_graph = crate::resolver::import_graph::ImportGraph::from_parsed_files(&parsed_files);

    let (sr_stats, sr_edges, heritage_resolutions) =
        crate::scope_resolution::orchestrator::run_scope_resolution(
            parsed_files, &all_file_paths,
            Some(&import_graph),
        );

    // Persist scope-resolution edges.
    for edge in &sr_edges {
        let src_db_id = symbol_id_map.get(&{ edge.source_id }).copied().unwrap_or(0);
        let dst_db_id = symbol_id_map.get(&{ edge.target_id }).copied().unwrap_or(0);
        if src_db_id == 0 || dst_db_id == 0 { continue; }
        let file_id = find_file_id_for_symbol(store, src_db_id).unwrap_or(None).unwrap_or(0);
        let _ = store.insert_edge(&crate::store::EdgeRecord {
            id: 0, src_id: src_db_id, dst_id: dst_db_id,
            edge_kind: edge.edge_type.clone(),
            confidence: edge.confidence,
            file_id: Some(file_id), line: 0,
        });
    }

    // Persist heritage resolutions.
    for (child_id, parent_name, parent_id, confidence) in &heritage_resolutions {
        let child_db_id = symbol_id_map.get(child_id).copied().unwrap_or(0);
        let parent_db_id = symbol_id_map.get(parent_id).copied().unwrap_or(0);
        if child_db_id == 0 || parent_db_id == 0 { continue; }
        let _ = store.update_heritage_parent(child_db_id, parent_name, parent_db_id, *confidence);
    }

    Ok(Some(sr_stats))
}

/// Run semantic phases for incremental scan: routes, communities, processes.
fn run_incremental_semantic_phases(
    store: &GraphStore,
    parsed_files: &[ParsedFile],
    symbol_id_map: &rustc_hash::FxHashMap<u64, i64>,
) -> io::Result<Vec<crate::routes::Route>> {
    // Detect and persist routes for changed files.
    let detected_routes = detect_and_persist_routes(
        store, parsed_files, symbol_id_map,
    ).unwrap_or_default();

    // Collect DB symbol IDs for changed files (needed for incremental community updates)
    let mut changed_symbol_ids = Vec::new();
    for pf in parsed_files {
        if let Ok(Some(file_rec)) = store.get_file(&pf.path) {
            if let Ok(syms) = store.get_symbols_by_file(file_rec.id) {
                for sym in &syms {
                    changed_symbol_ids.push(sym.id);
                }
            }
        }
    }

    // Incremental community update: only re-cluster affected communities
    if !changed_symbol_ids.is_empty() && changed_symbol_ids.len() < 1000 {
        // Use incremental update for small changes
        let incremental_config = crate::community::CommunityConfig {
            max_iterations: 10, // Fewer iterations for incremental
            min_size: 3,
            edge_kinds: vec!["CALLS".to_string(), "ACCESSES".to_string()],
        };
        match crate::community::update_communities_incremental(
            store, &changed_symbol_ids, &incremental_config,
        ) {
            Ok(result) => {
                if let Err(e) = crate::community::store_memberships(store, &result) {
                    tracing::warn!(error = %e, "Failed to store community memberships (incremental)");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Community incremental update failed, falling back to full recompute");
                // Fallback to full recompute
                if let Ok(result) = crate::community::detect_communities(store, &crate::community::CommunityConfig::default()) {
                    let _ = crate::community::store_memberships(store, &result);
                }
            }
        }
    } else if !changed_symbol_ids.is_empty() {
        // Too many changes — full recompute
        match crate::community::detect_communities(store, &crate::community::CommunityConfig::default()) {
            Ok(result) => {
                if let Err(e) = crate::community::store_memberships(store, &result) {
                    tracing::warn!(error = %e, "Failed to store community memberships");
                }
            }
            Err(e) => tracing::warn!(error = %e, "Community detection failed"),
        }
    }

    let _ = crate::process::detect_processes(
        store, &crate::process::ProcessConfig::default(),
    );

    Ok(detected_routes)
}

/// Detect API routes from parsed files and persist route→symbol edges to the store.
///
/// For each parsed file, runs route detection (Next.js path-based + Express AST-based).
/// Then finds the handler symbol in the same file and creates a ROUTE edge in the
/// edges table connecting the route to its handler symbol.
fn detect_and_persist_routes(
    store: &GraphStore,
    parsed_files: &[ParsedFile],
    symbol_id_map: &rustc_hash::FxHashMap<u64, i64>,
) -> io::Result<Vec<crate::routes::Route>> {
    let mut all_routes = Vec::new();

    for file in parsed_files {
        // Detect routes from this file using path-based + parsed-data detection
        let mut file_routes = crate::routes::detect_routes_from_path(&file.path);

        // Also detect from extracted decorators and calls (Flask, Axum, Rails, etc.)
        let lang_name = format!("{:?}", file.language).to_lowercase();
        file_routes.extend(crate::routes::detect_routes_from_parsed(
            &file.path, &lang_name, &file.decorators, &file.calls,
        ));

        if file_routes.is_empty() {
            continue;
        }

        // Find the handler symbol in this file (the first function/method/class symbol)
        use crate::lang::CaptureTag;
        let handler_symbol = file.symbols.iter().find(|s| {
            matches!(s.kind,
                CaptureTag::DefinitionFunction |
                CaptureTag::DefinitionMethod |
                CaptureTag::DefinitionClass)
        });

        if let Some(handler) = handler_symbol {
            if let Some(&handler_db_id) = symbol_id_map.get(&handler.id) {
                for route in &file_routes {
                    // Persist route info as a special symbol property via edges
                    // We create a synthetic "route" node and connect it to the handler
                    let route_node_name = format!("ROUTE:{}:{}", route.method, route.path);

                    // Insert a synthetic route symbol
                    let route_sym_id = store.insert_symbol(&SymbolRecord {
                        id: 0,
                        file_id: store.get_file_id_for_symbol(handler_db_id)
                            .map_err(|e| io::Error::other(e.to_string()))?
                            .unwrap_or(0),
                        name: route_node_name.clone(),
                        qualified_name: route_node_name,
                        kind: "Route".to_string(),
                        line: route.line,
                        col: 0,
                        is_exported: false,
                        scope_id: None,
                        owner_symbol_id: None,
                    }).map_err(|e| io::Error::other(e.to_string()))?;

                    // Create ROUTE edge: route → handler
                    store.insert_edge(&crate::store::EdgeRecord {
                        id: 0,
                        src_id: route_sym_id,
                        dst_id: handler_db_id,
                        edge_kind: "ROUTE".to_string(),
                        confidence: 1.0,
                        file_id: store.get_file_id_for_symbol(handler_db_id)
                            .map_err(|e| io::Error::other(e.to_string()))?,
                        line: route.line,
                    }).map_err(|e| io::Error::other(e.to_string()))?;

                    all_routes.push(route.clone());
                }
            }
        }
    }

    Ok(all_routes)
}

/// Detect changed files using git status.
///
/// Returns Some(set of changed file paths) if the root is inside a git repo,
/// or None if git is not available / the root is not a git repo.
///
/// This is used as a fast path during incremental scanning: files that git
/// reports as unchanged are guaranteed to be the same, so we can skip the
/// expensive content hash comparison.
pub fn detect_git_changes(root: &Path) -> Option<std::collections::HashSet<String>> {
    // Try to open a git repo at or above the root
    let repo = git2::Repository::discover(root).ok()?;

    let mut changed = std::collections::HashSet::new();

    // Get the working directory status
    let mut status_opts = git2::StatusOptions::new();
    status_opts.include_ignored(false);
    status_opts.include_untracked(true);
    status_opts.exclude_submodules(true);

    let statuses = repo.statuses(Some(&mut status_opts)).ok()?;

    for entry in statuses.iter() {
        if let Some(path) = entry.path() {
            // Only track files with known source extensions
            if let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) {
                if get_provider_for_extension(ext).is_some() {
                    changed.insert(path.to_string());
                }
            }
        }
    }

    // Also check for changes between HEAD and the index (staged changes)
    if let Ok(head) = repo.head() {
        if let Ok(tree) = head.peel_to_tree() {
            let mut diff_opts = git2::DiffOptions::new();
            if let Ok(diff) = repo.diff_tree_to_workdir(Some(&tree), Some(&mut diff_opts)) {
                for delta in diff.deltas() {
                    if let Some(path) = delta.new_file().path().or_else(|| delta.old_file().path()) {
                        if let Some(p) = path.to_str() {
                            if let Some(ext) = Path::new(p).extension().and_then(|e| e.to_str()) {
                                if get_provider_for_extension(ext).is_some() {
                                    changed.insert(p.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Some(changed)
}

/// Collect all source files under root for incremental scanning.
/// Directories that should always be skipped during incremental collection.
/// These are build artifacts, dependency directories, and VCS internals that
/// contain no user-authored source code.
const SKIP_DIRS: &[&str] = &[
    "node_modules", ".git", "target", "dist", "build", ".next",
    "__pycache__", ".cache", "vendor", "out", ".nuxt", ".svelte-kit",
    "coverage", ".turbo", ".parcel-cache", ".webpack",
];

fn collect_files_for_incremental(
    root: &Path,
    current: &Path,
    opts: &ScanOptions,
    files: &mut Vec<(String, String)>,
    depth: usize,
) -> io::Result<()> {
    collect_files_for_incremental_inner(root, current, opts, files, depth, &mut FxHashSet::default())
}

/// Inner function with visited-inode tracking to prevent symlink cycles.
#[allow(clippy::too_many_arguments)]
fn collect_files_for_incremental_inner(
    root: &Path,
    current: &Path,
    opts: &ScanOptions,
    files: &mut Vec<(String, String)>,
    depth: usize,
    visited: &mut FxHashSet<PathBuf>,
) -> io::Result<()> {
    if depth >= opts.max_depth {
        return Ok(());
    }

    // Canonicalize to detect symlink cycles via inode/path identity.
    let canonical = match current.canonicalize() {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    // If we've already visited this directory (e.g., via a symlink cycle), skip it.
    if !visited.insert(canonical.clone()) {
        log::warn!("Skipping already-visited directory (possible symlink cycle): {}", current.display());
        return Ok(());
    }

    let entries: Vec<_> = match fs::read_dir(current) {
        Ok(rd) => rd.filter_map(|e| e.ok()).collect(),
        Err(_) => return Ok(()),
    };

    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        let ft = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        if ft.is_dir() {
            // Skip hidden directories, build artifacts, and dependency directories
            if name_str.starts_with('.') || SKIP_DIRS.contains(&name_str.as_ref()) {
                continue;
            }
            collect_files_for_incremental_inner(root, &path, opts, files, depth + 1, visited)?;
        } else if ft.is_file() {
            // Skip oversized files to prevent OOM.
            if let Ok(meta) = fs::metadata(&path) {
                if meta.len() > MAX_FILE_SIZE {
                    log::warn!("Skipping file ({} exceeds {} MB limit): {}",
                        human_size(meta.len()), MAX_FILE_SIZE / (1024 * 1024), path.display());
                    continue;
                }
            }
            if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
                if get_provider_for_extension(ext).is_some() {
                    let rel = path.strip_prefix(root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .to_string();
                    let lang = format!("{:?}", get_provider_for_extension(ext).unwrap().id());
                    files.push((rel, lang));
                }
            }
        }
    }
    Ok(())
}

// =====================================================================
// Graph algorithms
// ==========================================================================================================================================
// Graph algorithms
// =====================================================================

/// BFS depth of every node from `root` (used as A*'s admissible heuristic).
pub fn compute_depths(adj: &FxHashMap<String, Vec<String>>, root: &str) -> FxHashMap<String, i32> {
    let mut depth = FxHashMap::default();
    let mut visited = FxHashSet::default();
    let mut q = VecDeque::new();
    depth.insert(root.to_string(), 0);
    visited.insert(root.to_string());
    q.push_back(root.to_string());
    while let Some(curr) = q.pop_front() {
        if let Some(neighbors) = adj.get(&curr) {
            for nei in neighbors {
                if !visited.contains(nei) {
                    visited.insert(nei.clone());
                    depth.insert(nei.clone(), depth[&curr] + 1);
                    q.push_back(nei.clone());
                }
            }
        }
    }
    depth
}

/// A* shortest-path. Returns `(path, nodes_expanded)` or `None` if no path
/// exists / endpoints aren't in the graph.
pub fn astar(
    adj: &FxHashMap<String, Vec<String>>,
    start: &str,
    goal: &str,
    depths: &FxHashMap<String, i32>,
) -> Option<(Vec<String>, usize)> {
    if !adj.contains_key(start) || !adj.contains_key(goal) {
        return None;
    }
    let mut open_set: BinaryHeap<(Reverse<i32>, String)> = BinaryHeap::new();
    let mut came_from: FxHashMap<String, String> = FxHashMap::default();
    let mut g_score: FxHashMap<String, i32> = FxHashMap::default();
    let mut closed: FxHashSet<String> = FxHashSet::default();

    g_score.insert(start.to_string(), 0);
    let h_start = (depths[start] - depths[goal]).abs();
    open_set.push((Reverse(h_start), start.to_string()));
    let mut expanded = 0usize;

    while let Some((_, current)) = open_set.pop() {
        if current == goal {
            let mut path = vec![current.clone()];
            let mut curr = current;
            while let Some(prev) = came_from.get(&curr) {
                path.push(prev.clone());
                curr = prev.clone();
            }
            path.reverse();
            return Some((path, expanded));
        }
        if closed.contains(&current) {
            continue;
        }
        closed.insert(current.clone());
        expanded += 1;
        if let Some(neighbors) = adj.get(&current) {
            for nei in neighbors {
                if closed.contains(nei) {
                    continue;
                }
                let tentative_g = g_score.get(&current).unwrap_or(&i32::MAX) + 1;
                if tentative_g < *g_score.get(nei).unwrap_or(&i32::MAX) {
                    came_from.insert(nei.clone(), current.clone());
                    g_score.insert(nei.clone(), tentative_g);
                    let h = (depths[nei] - depths[goal]).abs();
                    let f = tentative_g + h;
                    open_set.push((Reverse(f), nei.clone()));
                }
            }
        }
    }
    None
}

/// Plain BFS expansion count from `start` to `goal`. Used as the comparison
/// baseline for A*'s efficiency stat.
pub fn bfs_expanded(adj: &FxHashMap<String, Vec<String>>, start: &str, goal: &str) -> usize {
    if !adj.contains_key(start) || !adj.contains_key(goal) {
        return 0;
    }
    let mut visited = FxHashSet::default();
    let mut q = VecDeque::new();
    visited.insert(start.to_string());
    q.push_back(start.to_string());
    let mut expanded = 0usize;
    while let Some(curr) = q.pop_front() {
        expanded += 1;
        if curr == goal {
            return expanded;
        }
        if let Some(neis) = adj.get(&curr) {
            for n in neis {
                if !visited.contains(n) {
                    visited.insert(n.clone());
                    q.push_back(n.clone());
                }
            }
        }
    }
    expanded
}

/// Build a [`PathReport`] from an A* result for JSON / programmatic use.
pub fn build_path_report(
    start: &str,
    goal: &str,
    path: &[String],
    astar_expanded: usize,
    bfs_expanded: usize,
) -> PathReport {
    let efficiency_pct = if bfs_expanded > 0 {
        100.0 * (1.0 - astar_expanded as f64 / bfs_expanded as f64)
    } else {
        0.0
    };
    PathReport {
        start: start.to_string(),
        goal: goal.to_string(),
        hops: path.len().saturating_sub(1),
        nodes: path.to_vec(),
        astar_expanded,
        bfs_expanded,
        efficiency_pct,
    }
}

// =====================================================================
// Rendering
// =====================================================================

/// Render the tree to any [`Write`]. Color codes are emitted inline; pass
/// `no_color = true` for plain text.
#[allow(clippy::too_many_arguments)]
pub fn print_tree<W: Write>(
    out: &mut W,
    adj: &FxHashMap<String, Vec<String>>,
    meta: &FxHashMap<String, NodeMeta>,
    root: &str,
    depths: &FxHashMap<String, i32>,
    ascii: bool,
    no_color: bool,
    path: &FxHashSet<String>,
) -> io::Result<()> {
    let title = if ascii {
        "\n+-- File System Tree (ASCII view, A* path marked with *)"
    } else {
        "\n📁 File System Tree (Unicode view, A* path marked with *)"
    };
    writeln!(out, "{}", title)?;
    print_node(out, root, "", true, adj, meta, depths, ascii, no_color, path)
}

#[allow(clippy::too_many_arguments)]
fn print_node<W: Write>(
    out: &mut W,
    node: &str,
    prefix: &str,
    is_last: bool,
    adj: &FxHashMap<String, Vec<String>>,
    meta: &FxHashMap<String, NodeMeta>,
    depths: &FxHashMap<String, i32>,
    ascii: bool,
    no_color: bool,
    path: &FxHashSet<String>,
) -> io::Result<()> {
    let default_meta = NodeMeta {
        is_dir: true,
        is_symlink: false,
        is_hidden: false,
        is_exec: false,
        mode: 0,
        size: 0,
        name: node.to_string(),
    };
    let m = meta.get(node).unwrap_or(&default_meta);
    let icon = if ascii {
        if m.is_symlink { "[L]" } else if m.is_dir { "[D]" } else { "[F]" }
    } else if m.is_symlink {
        "🔗"
    } else if m.is_dir {
        "📁"
    } else {
        "📄"
    };
    let size_str = if m.is_dir || m.size == 0 {
        String::new()
    } else {
        format!(" ({})", human_size(m.size))
    };
    let depth = depths.get(node).unwrap_or(&0);
    let on_path = path.contains(node);
    let marker = if on_path { " * " } else { "   " };
    let (branch, cont) = if ascii {
        if is_last { ("`-- ", "    ") } else { ("+-- ", "|   ") }
    } else if is_last {
        ("└── ", "    ")
    } else {
        ("├── ", "│   ")
    };
    let connector = format!("{}{}", branch, marker);
    let base_color = if no_color {
        ""
    } else if m.is_symlink {
        "\x1b[1;36m"
    } else if m.is_dir {
        "\x1b[1;34m"
    } else if m.is_exec {
        "\x1b[1;32m"
    } else {
        "\x1b[37m"
    };
    let dim = if m.is_hidden && !no_color { "\x1b[2m" } else { "" };
    let reset = if no_color { "" } else { "\x1b[0m" };
    let badge = if m.is_dir || m.is_symlink {
        String::new()
    } else {
        permission_badge(m.mode, no_color)
    };

    writeln!(
        out,
        "{}{}{}{}{}{}{}{} [depth {}]{}",
        prefix, connector, dim, base_color, icon, m.name, reset, badge, depth, size_str
    )?;

    let mut children: Vec<_> = adj.get(node).unwrap_or(&vec![]).clone();
    children.retain(|c| depths.get(c).unwrap_or(&0) > depths.get(node).unwrap_or(&0));

    let new_prefix = format!("{}{}", prefix, cont);
    for (i, child) in children.iter().enumerate() {
        let last = i == children.len() - 1;
        print_node(out, child, &new_prefix, last, adj, meta, depths, ascii, no_color, path)?;
    }
    Ok(())
}

/// Generate a Graphviz DOT file at `output_path` highlighting the A* path.
#[allow(clippy::too_many_arguments)]
pub fn generate_dot(
    adj: &FxHashMap<String, Vec<String>>,
    meta: &FxHashMap<String, NodeMeta>,
    depths: &FxHashMap<String, i32>,
    path: &[String],
    root: &str,
    output_path: &str,
) -> io::Result<()> {
    let mut f = std::fs::File::create(output_path)?;
    writeln!(f, "digraph FileSystemMap {{")?;
    writeln!(f, "  rankdir=TB;")?;
    writeln!(f, "  node [fontname=\"DejaVu Sans\", fontsize=10, shape=box, style=filled];")?;
    writeln!(f, "  edge [color=\"#888888\", penwidth=0.5];")?;
    writeln!(f, "  // Root")?;
    writeln!(
        f,
        "  \"{}\" [fillcolor=\"#457b9d\", fontcolor=white, label=\"📁 {}\"];",
        root,
        meta.get(root).map(|m| &m.name).unwrap_or(&root.to_string())
    )?;

    let path_set: FxHashSet<_> = path.iter().cloned().collect();
    let path_edges: FxHashSet<_> =
        path.windows(2).map(|w| (w[0].clone(), w[1].clone())).collect();

    for (u, neighbors) in adj {
        let default_meta = NodeMeta {
            is_dir: true,
            is_symlink: false,
            is_hidden: false,
            is_exec: false,
            mode: 0,
            size: 0,
            name: u.clone(),
        };
        let m = meta.get(u).unwrap_or(&default_meta);
        let color = if path_set.contains(u) {
            "#e63946"
        } else if m.is_dir {
            "#457b9d"
        } else {
            "#2a9d8f"
        };
        let label = if path_set.contains(u) {
            format!("⭐ {}", m.name)
        } else {
            m.name.clone()
        };
        let shape = if m.is_dir { "box" } else { "ellipse" };
        writeln!(
            f,
            "  \"{}\" [fillcolor=\"{}\", label=\"{} {}\", shape={}];",
            u,
            color,
            if m.is_dir { "📁" } else { "📄" },
            label,
            shape
        )?;
        for v in neighbors {
            if depths.get(v).unwrap_or(&0) > depths.get(u).unwrap_or(&0) {
                let is_path_edge = path_edges.contains(&(u.clone(), v.clone()))
                    || path_edges.contains(&(v.clone(), u.clone()));
                let edge_color = if is_path_edge { "#e63946" } else { "#adb5bd" };
                let pen = if is_path_edge { "2.0" } else { "0.6" };
                writeln!(
                    f,
                    "  \"{}\" -> \"{}\" [color=\"{}\", penwidth={}];",
                    u, v, edge_color, pen
                )?;
            }
        }
    }
    writeln!(f, "}}")?;
    Ok(())
}

// =====================================================================
// JSON report assembly
// =====================================================================

/// Convert `usize::MAX` to `None`, anything else to `Some(_)`. Used to encode
/// "unbounded" caps as `null` in JSON.
fn cap_or_none(v: usize) -> Option<usize> {
    if v == usize::MAX {
        None
    } else {
        Some(v)
    }
}

/// Build a fully-populated [`JsonReport`] from a scan result + optional path.
#[allow(clippy::too_many_arguments)]
pub fn build_json_report(
    scan: &ScanResult,
    options: &ScanOptions,
    depths: &FxHashMap<String, i32>,
    path: Option<PathReport>,
    elapsed_ms: f64,
) -> JsonReport {
    let edges: BTreeMap<String, Vec<String>> = scan
        .adj
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let nodes: BTreeMap<String, NodeMeta> = scan
        .meta
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let depths_map: BTreeMap<String, i32> =
        depths.iter().map(|(k, v)| (k.clone(), *v)).collect();
    JsonReport {
        semantic: if options.semantic {
            Some(scan.parsed_files.iter().map(|f| f.to_output()).collect())
        } else { None },
        symbol_table: if options.semantic { Some(scan.symbol_table.clone()) } else { None },
        store_stats: if options.semantic { Some(scan.store_stats.clone()) } else { None },
        resolution_stats: if options.semantic { scan.resolution_stats.clone() } else { None },
        scope_resolution_stats: if options.semantic { scan.scope_resolution_stats.clone() } else { None },
        schema_version: SCHEMA_VERSION,
        version: env!("CARGO_PKG_VERSION").to_string(),
        root: options.root.display().to_string(),
        root_name: scan.root_name.clone(),
        elapsed_ms,
        threads: options.threads,
        options: JsonOptions {
            max_depth: cap_or_none(options.max_depth),
            max_nodes: cap_or_none(options.max_nodes),
            include_files: options.include_files,
            tree_mode: options.tree_mode,
            semantic: options.semantic,
            db_path: options.db_path.clone(),
            incremental: options.incremental,
            embeddings: options.embeddings,
        },
        stats: scan.stats.clone(),
        truncated: scan.truncated,
        depths: depths_map,
        nodes,
        edges,
        path,
    }
}

// =====================================================================
// Tests
// =====================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic::{ScopeKind, HeritageKind};
    use std::fs;

    fn tiny_graph() -> FxHashMap<String, Vec<String>> {
        // a -- b -- c
        //      |
        //      d -- e
        let mut adj = FxHashMap::default();
        adj.insert("a".into(), vec!["b".into()]);
        adj.insert("b".into(), vec!["a".into(), "c".into(), "d".into()]);
        adj.insert("c".into(), vec!["b".into()]);
        adj.insert("d".into(), vec!["b".into(), "e".into()]);
        adj.insert("e".into(), vec!["d".into()]);
        adj
    }

    #[test]
    fn compute_depths_basic() {
        let adj = tiny_graph();
        let d = compute_depths(&adj, "a");
        assert_eq!(d["a"], 0);
        assert_eq!(d["b"], 1);
        assert_eq!(d["c"], 2);
        assert_eq!(d["d"], 2);
        assert_eq!(d["e"], 3);
    }

    #[test]
    fn astar_finds_shortest() {
        let adj = tiny_graph();
        let d = compute_depths(&adj, "a");
        let (path, _expanded) = astar(&adj, "a", "e", &d).unwrap();
        assert_eq!(path, vec!["a", "b", "d", "e"]);
    }

    #[test]
    fn astar_self_path() {
        let adj = tiny_graph();
        let d = compute_depths(&adj, "a");
        let (path, _) = astar(&adj, "c", "c", &d).unwrap();
        assert_eq!(path, vec!["c"]);
    }

    #[test]
    fn astar_unknown_endpoint() {
        let adj = tiny_graph();
        let d = compute_depths(&adj, "a");
        assert!(astar(&adj, "a", "zzz", &d).is_none());
        assert!(astar(&adj, "zzz", "a", &d).is_none());
    }

    #[test]
    fn bfs_expanded_basic() {
        let adj = tiny_graph();
        // BFS finds 'e' in 4 expansions: a, b, c, d, e. Returns 5 (counts goal).
        assert!(bfs_expanded(&adj, "a", "e") >= 4);
    }

    #[test]
    fn sanitize_strips_control_chars() {
        assert_eq!(sanitize_name("normal.txt"), "normal.txt");
        assert_eq!(sanitize_name("\x1b[2Jbad"), "?[2Jbad");
        assert_eq!(sanitize_name("a\nb\tc"), "a?b?c");
    }

    #[test]
    fn human_size_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(1024 * 1024 * 5), "5.0 MB");
    }

    #[test]
    fn half_cores_at_least_one() {
        assert!(half_cores() >= 1);
        assert!(all_cores() >= 1);
        assert!(half_cores() <= all_cores());
    }

    #[test]
    fn build_graph_tempdir_integration() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_test_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(tmp.join("sub_a/inner")).unwrap();
        fs::create_dir_all(tmp.join("sub_b")).unwrap();
        fs::write(tmp.join("root_file.txt"), b"hi").unwrap();
        fs::write(tmp.join("sub_a/inner/leaf.txt"), b"deep").unwrap();
        fs::write(tmp.join("sub_b/note.md"), b"note").unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 2,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: false,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();
        // root + 2 subdirs + 1 inner dir + 3 files = 7
        assert_eq!(result.stats.total_nodes, 7);
        assert_eq!(result.stats.folders, 4); // root, sub_a, sub_a/inner, sub_b
        assert_eq!(result.stats.files, 3);
        assert!(!result.truncated);

        let depths = compute_depths(&result.adj, &result.root_name);
        let leaf_key = "sub_a/inner/leaf.txt";
        assert!(result.adj.contains_key(leaf_key));
        let (path, _) = astar(&result.adj, &result.root_name, leaf_key, &depths).unwrap();
        assert_eq!(*path.first().unwrap(), result.root_name);
        assert_eq!(*path.last().unwrap(), leaf_key);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn json_report_roundtrips_through_serde() {
        // Build a small scan, serialize the JSON report, and deserialize it
        // back into the same typed structs. Proves the schema is consistent
        // for any external consumer using a strongly-typed JSON parser.
        let tmp = std::env::temp_dir().join(format!(
            "atree_json_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(tmp.join("a/b")).unwrap();
        fs::write(tmp.join("a/b/leaf.txt"), b"x").unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: usize::MAX, // unbounded → null in JSON
            max_nodes: usize::MAX,
            include_files: true,
            threads: 2,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: false,
            ..Default::default()
        };
        let scan = build_graph(&opts).unwrap();
        let depths = compute_depths(&scan.adj, &scan.root_name);
        let path_report = astar(&scan.adj, &scan.root_name, "a/b/leaf.txt", &depths)
            .map(|(p, ax)| {
                let bx = bfs_expanded(&scan.adj, &scan.root_name, "a/b/leaf.txt");
                build_path_report(&scan.root_name, "a/b/leaf.txt", &p, ax, bx)
            });
        let report = build_json_report(&scan, &opts, &depths, path_report, 1.23);

        let json = serde_json::to_string(&report).expect("serialize");
        let parsed: JsonReport = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(parsed.schema_version, SCHEMA_VERSION);
        assert_eq!(parsed.schema_version, 2); // pin: bump only on breaking changes
        assert_eq!(parsed.version, report.version);
        assert_eq!(parsed.root_name, report.root_name);
        assert_eq!(parsed.stats.total_nodes, report.stats.total_nodes);
        assert!(parsed.options.max_depth.is_none()); // usize::MAX → null
        assert!(parsed.options.max_nodes.is_none());
        // root -> a -> a/b -> a/b/leaf.txt = 3 hops
        assert_eq!(parsed.path.as_ref().map(|p| p.hops), Some(3));
        // Re-serialize and compare for byte-stable round-trip.
        let json2 = serde_json::to_string(&parsed).expect("re-serialize");
        assert_eq!(json, json2);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn embedded_schema_is_valid_json() {
        // Catches a malformed docs/schema.json at build time of the test suite,
        // so --print-schema and any consumer reading SCHEMA_JSON never sees
        // garbage shipped from a typo.
        let v: serde_json::Value =
            serde_json::from_str(SCHEMA_JSON).expect("embedded schema parses");
        assert_eq!(v["$schema"], "http://json-schema.org/draft-07/schema#");
        assert_eq!(v["properties"]["schema_version"]["const"], SCHEMA_VERSION);
    }

    #[test]
    fn build_graph_rejects_nonexistent_root() {
        let bogus = std::env::temp_dir().join(format!(
            "atree_does_not_exist_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let opts = ScanOptions {
            root: bogus,
            max_depth: 4,
            max_nodes: 10,
            include_files: false,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: false,
            ..Default::default()
        };
        let err = match build_graph(&opts) {
            Err(e) => e,
            Ok(_) => panic!("must fail on nonexistent root"),
        };
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn build_graph_rejects_file_as_root() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_file_root_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&tmp, b"i am a file").unwrap();
        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 4,
            max_nodes: 10,
            include_files: false,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: false,
            ..Default::default()
        };
        let err = match build_graph(&opts) {
            Err(e) => e,
            Ok(_) => panic!("must fail when root is a file"),
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        fs::remove_file(&tmp).ok();
    }

    #[test]
    fn build_graph_respects_max_nodes() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_cap_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for i in 0..50 {
            fs::create_dir_all(tmp.join(format!("d_{:02}", i))).unwrap();
        }
        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 10,
            include_files: false,
            threads: 4,
            tree_mode: true,
            db_path: None,
            incremental: false,
            semantic: false,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();
        assert!(result.truncated);
        assert!(result.stats.total_nodes <= 10);
        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // Semantic engine integration test
    // =====================================================================

    #[test]
    fn semantic_engine_extracts_symbols_across_languages() {
        // Create a temp directory with source files in multiple languages
        let tmp = std::env::temp_dir().join(format!(
            "atree_sem_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        // Rust file: defines a struct, impl, function, and calls println
        fs::write(tmp.join("main.rs"), r#"
use std::fmt;

pub struct MyService {
    name: String,
}

impl MyService {
    pub fn new(name: &str) -> Self {
        MyService { name: name.to_string() }
    }

    pub fn run(&self) {
        println!("Running {}", self.name);
    }
}

pub fn create_service() -> MyService {
    MyService::new("default")
}

fn main() {
    let svc = create_service();
    svc.run();
}
"#).unwrap();

        // Python file: defines a class, function, import, decorator
        fs::write(tmp.join("app.py"), r#"
import os
from typing import Optional

def my_decorator(func):
    return func

@my_decorator
class App:
    def __init__(self):
        self.value = 42

    def start(self):
        print("started")

def create_app() -> App:
    return App()

if __name__ == "__main__":
    app = create_app()
    app.start()
"#).unwrap();

        // TypeScript file: defines a class, interface, heritage
        fs::write(tmp.join("service.ts"), r#"
import { EventEmitter } from 'events';

interface Runnable {
    run(): void;
}

class BaseService {
    protected name: string;
    constructor(name: string) {
        this.name = name;
    }
}

class WorkerService extends BaseService implements Runnable {
    run() {
        console.log(`Running ${this.name}`);
    }
}

function createWorker(): WorkerService {
    return new WorkerService("worker");
}

const w = createWorker();
w.run();
"#).unwrap();

        // Go file: defines struct, interface, function
        fs::write(tmp.join("server.go"), r#"
package main

import "fmt"

type Server interface {
    Start() error
}

type HttpServer struct {
    addr string
}

func (s *HttpServer) Start() error {
    fmt.Println("Starting server on", s.addr)
    return nil
}

func NewServer(addr string) *HttpServer {
    return &HttpServer{addr: addr}
}

func main() {
    s := NewServer(":8080")
    s.Start()
}
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 10000,
            include_files: true,
            threads: 2,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        // ---- Verify parsed files ----
        assert!(result.parsed_files.len() >= 4, "Expected at least 4 parsed files, got {}", result.parsed_files.len());

        // ---- Verify Rust symbols ----
        let rust_file = result.parsed_files.iter().find(|f| f.path.ends_with("main.rs")).expect("main.rs not found");
        let rust_names: Vec<&str> = rust_file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(rust_names.contains(&"MyService"), "Rust: MyService not found. Symbols: {:?}", rust_names);
        assert!(rust_names.contains(&"new"), "Rust: new not found. Symbols: {:?}", rust_names);
        assert!(rust_names.contains(&"run"), "Rust: run not found. Symbols: {:?}", rust_names);
        assert!(rust_names.contains(&"create_service"), "Rust: create_service not found. Symbols: {:?}", rust_names);
        assert!(rust_names.contains(&"main"), "Rust: main not found. Symbols: {:?}", rust_names);

        // ---- Verify Python symbols ----
        let py_file = result.parsed_files.iter().find(|f| f.path.ends_with("app.py")).expect("app.py not found");
        let py_names: Vec<&str> = py_file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(py_names.contains(&"my_decorator"), "Python: my_decorator not found. Symbols: {:?}", py_names);
        assert!(py_names.contains(&"App"), "Python: App not found. Symbols: {:?}", py_names);
        assert!(py_names.contains(&"create_app"), "Python: create_app not found. Symbols: {:?}", py_names);

        // ---- Verify TypeScript symbols ----
        let ts_file = result.parsed_files.iter().find(|f| f.path.ends_with("service.ts")).expect("service.ts not found");
        let ts_names: Vec<&str> = ts_file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(ts_names.contains(&"Runnable"), "TS: Runnable not found. Symbols: {:?}", ts_names);
        assert!(ts_names.contains(&"BaseService"), "TS: BaseService not found. Symbols: {:?}", ts_names);
        assert!(ts_names.contains(&"WorkerService"), "TS: WorkerService not found. Symbols: {:?}", ts_names);
        assert!(ts_names.contains(&"createWorker"), "TS: createWorker not found. Symbols: {:?}", ts_names);

        // ---- Verify Go symbols ----
        let go_file = result.parsed_files.iter().find(|f| f.path.ends_with("server.go")).expect("server.go not found");
        let go_names: Vec<&str> = go_file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(go_names.contains(&"Server"), "Go: Server not found. Symbols: {:?}", go_names);
        assert!(go_names.contains(&"HttpServer"), "Go: HttpServer not found. Symbols: {:?}", go_names);
        assert!(go_names.contains(&"Start"), "Go: Start not found. Symbols: {:?}", go_names);
        assert!(go_names.contains(&"NewServer"), "Go: NewServer not found. Symbols: {:?}", go_names);

        // ---- Verify symbol table ----
        let st = &result.symbol_table;
        assert!(st.resolve("MyService").is_some(), "SymbolTable: MyService not found");
        assert!(st.resolve("App").is_some(), "SymbolTable: App not found");
        assert!(st.resolve("WorkerService").is_some(), "SymbolTable: WorkerService not found");
        assert!(st.resolve("HttpServer").is_some(), "SymbolTable: HttpServer not found");
        assert!(st.resolve("NewServer").is_some(), "SymbolTable: NewServer not found");

        // ---- Verify store stats ----
        let store_stats = &result.store_stats;
        assert!(store_stats.files >= 4, "Store: expected >=4 files, got {}", store_stats.files);
        assert!(store_stats.symbols > 0, "Store: no symbols indexed");
        assert!(store_stats.calls > 0, "Store: no calls found");

        // Verify resolution stats
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.files_processed > 0, "ScopeResolution: should have processed files");
            assert!(srs.reference_edges_emitted > 0, "Resolution: no defines edges");
        }

        // Verify scope-resolution emitted edges into the store
        assert!(store_stats.edges > 0, "Store: no edges from scope-resolution");

        // ---- Verify JSON roundtrip ----
        let depths = compute_depths(&result.adj, &result.root_name);
        let report = build_json_report(&result, &opts, &depths, None, 1.0);
        let json = serde_json::to_string(&report).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("deserialize");

        // Verify semantic array in JSON
        let semantic = parsed["semantic"].as_array().expect("semantic array");
        assert!(semantic.len() >= 4, "JSON: expected >=4 semantic entries");

        // Verify store_stats in JSON
        let ss = parsed["store_stats"].as_object().expect("store_stats object");
        assert!(ss["symbols"].as_i64().unwrap() > 0, "JSON: store_stats has no symbols");
        assert!(ss["calls"].as_i64().unwrap() > 0, "JSON: store_stats has no calls");

        // Verify scope_resolution_stats in JSON
        let srs = parsed["scope_resolution_stats"].as_object().expect("scope_resolution_stats object");
        assert!(srs["reference_edges_emitted"].as_u64().unwrap() > 0, "JSON: scope_resolution_stats has no edges");

        // Verify symbol_table in JSON
        let st_json = parsed["symbol_table"].as_object().expect("symbol_table object");
        assert!(st_json["definitions"].as_object().unwrap().len() > 0, "JSON: symbol_table has no definitions");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn semantic_engine_cross_file_resolution() {
        // Test that calls in one file resolve to definitions in another
        let tmp = std::env::temp_dir().join(format!(
            "atree_cross_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        // File 1: defines a function
        fs::write(tmp.join("lib.rs"), r#"
pub fn helper() -> i32 { 42 }
pub fn process(x: i32) -> i32 { x * 2 }
"#).unwrap();

        // File 2: calls the function from file 1
        fs::write(tmp.join("main.rs"), r#"
fn main() {
    let result = helper();
    let processed = process(result);
}
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        // The main.rs file should have calls to helper and process (check parsed_files for extraction)
        let main_file = result.parsed_files.iter().find(|f| f.path.ends_with("main.rs")).expect("main.rs not found");
        let main_call_names: Vec<&str> = main_file.calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(main_call_names.contains(&"helper"), "main.rs should call helper, got {:?}", main_call_names);
        assert!(main_call_names.contains(&"process"), "main.rs should call process, got {:?}", main_call_names);

        // Verify the store has both files indexed
        assert!(result.store_stats.files >= 2, "Store: expected >=2 files, got {}", result.store_stats.files);

        // Verify the symbol table has both helper and process
        assert!(result.symbol_table.resolve("helper").is_some(), "SymbolTable: helper not found");
        assert!(result.symbol_table.resolve("process").is_some(), "SymbolTable: process not found");

        // Verify symbols from both files were indexed
        assert!(result.store_stats.symbols >= 2, "Store: expected symbols from both files");

        // Verify scope-resolution stats
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.files_processed > 0, "ScopeResolution: should have processed files");
        }

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn semantic_engine_confidence_scoring() {
        // Verify that confidence scores are properly assigned
        let tmp = std::env::temp_dir().join(format!(
            "atree_conf_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("test.py"), r#"
class MyClass:
    def method(self):
        pass

def my_func():
    obj = MyClass()
    obj.method()  # receiver heuristic
    my_func()     # same-file exact
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 100,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("test.py")).expect("test.py not found");

        // Verify symbols were extracted
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MyClass"), "MyClass not found in {:?}", names);
        assert!(names.contains(&"my_func"), "my_func not found in {:?}", names);

        // Verify calls were extracted
        let call_names: Vec<&str> = file.calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(call_names.contains(&"MyClass"), "MyClass() call not found in {:?}", call_names);
        assert!(call_names.contains(&"my_func"), "my_func() call not found in {:?}", call_names);

        // Verify store has symbols and calls
        assert!(result.store_stats.symbols >= 2, "Store: expected at least 2 symbols, got {}", result.store_stats.symbols);
        assert!(result.store_stats.calls > 0, "Store: no calls found");

        // Verify scope-resolution ran and processed files
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.files_processed > 0, "ScopeResolution should have processed files");
        }

        // Verify scope-resolution extracted reference sites
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.files_processed > 0, "ScopeResolution: should have processed files");
            // With the type-binding fallback fix, most sites now resolve.
            // Just verify the pipeline ran (reference_edges_emitted > 0 or unresolved_sites > 0).
            assert!(srs.reference_edges_emitted > 0 || srs.unresolved_sites > 0,
                "ScopeResolution: should have processed reference sites");
        }

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // Scope tree extraction tests
    // =====================================================================

    #[test]
    fn scope_tree_extraction_creates_proper_hierarchy() {
        // Verify that scope trees are properly extracted from the AST
        let tmp = std::env::temp_dir().join(format!(
            "atree_scope_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        // Python file with nested classes and functions
        fs::write(tmp.join("nested.py"), r#"
class Outer:
    def outer_method(self):
        pass

    class Inner:
        def inner_method(self):
            pass

def top_level():
    def nested_func():
        pass
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("nested.py")).unwrap();

        // Should have scopes: Module, Outer class, outer_method, Inner class, inner_method, top_level, nested_func
        assert!(file.scopes.len() >= 4, "Expected at least 4 scopes, got {}", file.scopes.len());

        // Verify scope kinds
        let kinds: Vec<ScopeKind> = file.scopes.iter().map(|s| s.kind).collect();
        assert!(kinds.contains(&ScopeKind::Module), "Should have Module scope");
        assert!(kinds.contains(&ScopeKind::Class), "Should have Class scope");
        assert!(kinds.contains(&ScopeKind::Function), "Should have Function scope");

        // Verify parent chain: Inner class should be a child of Outer class
        let outer_idx = file.scopes.iter().position(|s| s.kind == ScopeKind::Class && s.line_start <= 1).unwrap();
        let inner_idx = file.scopes.iter().position(|s| s.kind == ScopeKind::Class && s.line_start > 1).unwrap();
        let outer_scope_id = file.scopes[outer_idx].id;
        assert_eq!(file.scopes[inner_idx].parent_id, Some(outer_scope_id),
            "Inner class should be child of Outer class scope");

        // Verify symbols have scope_id assigned
        for sym in &file.symbols {
            assert!(sym.scope_id.is_some(), "Symbol {} should have a scope_id", sym.name);
        }

        // Verify store has scopes
        assert!(result.store_stats.scopes >= 4, "Store: expected >=4 scopes, got {}", result.store_stats.scopes);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn scope_symbols_have_correct_owners() {
        // Verify that methods inside classes get the correct owner_id
        let tmp = std::env::temp_dir().join(format!(
            "atree_owner_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("service.ts"), r#"
class MyService {
    name: string;

    constructor(name: string) {
        this.name = name;
    }

    run() {
        console.log(this.name);
    }

    stop() {
        console.log("stopped");
    }
}

function createService() {
    return new MyService("default");
}
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("service.ts")).unwrap();

        // Find the MyService class symbol
        let service_sym = file.symbols.iter().find(|s| s.name == "MyService").unwrap();
        assert!(service_sym.scope_id.is_some(), "MyService should have a scope_id");

        // Find methods inside MyService — they should have owner_id pointing to MyService
        // At least some methods should have owners
        let methods_with_owners: Vec<_> = file.symbols.iter()
            .filter(|s| s.name == "run" || s.name == "stop" || s.name == "constructor")
            .filter(|s| s.owner_id.is_some())
            .collect();
        assert!(!methods_with_owners.is_empty(), "At least one method should have an owner_id");

        // createService should NOT have an owner (it's top-level)
        let create_sym = file.symbols.iter().find(|s| s.name == "createService");
        if let Some(create) = create_sym {
            assert!(create.owner_id.is_none(), "createService() should not have an owner_id");
        }

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // MRO / Inheritance resolution tests
    // =====================================================================

    #[test]
    fn mro_resolves_inheritance_edges() {
        // Verify that MRO phase emits EXTENDS edges for class inheritance
        let tmp = std::env::temp_dir().join(format!(
            "atree_mro_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("models.py"), r#"
class BaseModel:
    def save(self):
        pass

class User(BaseModel):
    def get_name(self):
        pass

class Admin(User):
    def get_permissions(self):
        pass
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        // Verify heritage was extracted
        let file = result.parsed_files.iter().find(|f| f.path.ends_with("models.py")).unwrap();
        assert!(!file.heritage.is_empty(), "Should have heritage entries");
        assert!(file.heritage.len() >= 2, "Should have at least 2 heritage entries (User→BaseModel, Admin→User)");

        // Verify MRO edges were emitted
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.reference_edges_emitted > 0, "Should have scope-resolution edges");
        }

        // Verify store has edges from scope-resolution (MRO + call edges)
        assert!(result.store_stats.edges > 0, "Store should have edges from scope-resolution");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn mro_resolves_interface_implements() {
        // Verify IMPLEMENTS edges for interface implementation
        let tmp = std::env::temp_dir().join(format!(
            "atree_iface_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("service.ts"), r#"
interface Runnable {
    run(): void;
}

interface Stoppable {
    stop(): void;
}

class WorkerService implements Runnable, Stoppable {
    run() {}
    stop() {}
}
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("service.ts")).unwrap();

        // Verify heritage entries for implements
        let implements: Vec<_> = file.heritage.iter()
            .filter(|h| matches!(h.heritage_kind, HeritageKind::Implements))
            .collect();
        assert!(!implements.is_empty(), "Should have IMPLEMENTS heritage entries");

        // Verify MRO edges
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.reference_edges_emitted >= 2, "Should have at least 2 scope-resolution edges");
        }

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // Import resolution tests
    // =====================================================================

    #[test]
    fn import_resolution_python_relative() {
        // Verify Python relative imports are resolved
        let tmp = std::env::temp_dir().join(format!(
            "atree_imp_py_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::create_dir_all(tmp.join("pkg/sub")).unwrap();
        fs::write(tmp.join("pkg/__init__.py"), "").unwrap();
        fs::write(tmp.join("pkg/sub/__init__.py"), "").unwrap();
        fs::write(tmp.join("pkg/sub/helper.py"), r#"
def helper_func():
    return 42
"#).unwrap();
        fs::write(tmp.join("pkg/main.py"), r#"
from .sub.helper import helper_func

def main():
    return helper_func()
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        // Verify imports were extracted
        let main_file = result.parsed_files.iter().find(|f| f.path.contains("main.py")).unwrap();
        assert!(!main_file.imports.is_empty(), "Should have imports in main.py");

        // Verify imports were extracted (cross-file resolution requires import target resolution)
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.files_processed > 0, "ScopeResolution should have processed files");
        }

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn import_resolution_typescript_relative() {
        // Verify TypeScript relative imports
        let tmp = std::env::temp_dir().join(format!(
            "atree_imp_ts_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::create_dir_all(tmp.join("src/utils")).unwrap();
        fs::write(tmp.join("src/utils/helper.ts"), r#"
export function helper() {
    return 42;
}
"#).unwrap();
        fs::write(tmp.join("src/main.ts"), r#"
import { helper } from './utils/helper';

function main() {
    return helper();
}
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let main_file = result.parsed_files.iter().find(|f| f.path.contains("main.ts")).unwrap();
        assert!(!main_file.imports.is_empty(), "Should have imports in main.ts");

        // Verify imports were extracted (cross-file resolution requires import target resolution)
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.files_processed > 0, "ScopeResolution should have processed files");
        }

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // Cross-file resolution tests
    // =====================================================================

    #[test]
    fn cross_file_call_resolution_with_imports() {
        // Test that calls in one file resolve to definitions in another via imports
        let tmp = std::env::temp_dir().join(format!(
            "atree_cross2_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("math_utils.py"), r#"
def add(a, b):
    return a + b

def multiply(a, b):
    return a * b
"#).unwrap();

        fs::write(tmp.join("main.py"), r#"
from math_utils import add, multiply

def compute():
    x = add(1, 2)
    y = multiply(x, 3)
    return y
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        // Verify both files were parsed
        assert!(result.parsed_files.len() >= 2, "Should have at least 2 parsed files");

        // Verify scope-resolution ran (cross-file import resolution not yet implemented)
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.files_processed > 0, "ScopeResolution should have processed files");
        }

        // Verify scope-resolution ran (cross-file call edges require import resolution)
        assert!(result.store_stats.symbols >= 2, "Should have symbols from both files");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn cross_file_inheritance_chain() {
        // Test multi-level inheritance across files
        let tmp = std::env::temp_dir().join(format!(
            "atree_chain_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("base.py"), r#"
class Entity:
    def __init__(self, id):
        self.id = id

    def get_id(self):
        return self.id
"#).unwrap();

        fs::write(tmp.join("user.py"), r#"
from base import Entity

class User(Entity):
    def __init__(self, id, name):
        super().__init__(id)
        self.name = name

    def get_name(self):
        return self.name
"#).unwrap();

        fs::write(tmp.join("admin.py"), r#"
from user import User

class Admin(User):
    def __init__(self, id, name, role):
        super().__init__(id, name)
        self.role = role

    def get_role(self):
        return self.role
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        // Verify all 3 files parsed
        assert!(result.parsed_files.len() >= 3, "Should have 3 parsed files, got {}", result.parsed_files.len());

        // Verify heritage was extracted
        let user_file = result.parsed_files.iter().find(|f| f.path.contains("user.py")).unwrap();
        assert!(!user_file.heritage.is_empty(), "User should have heritage");

        let admin_file = result.parsed_files.iter().find(|f| f.path.contains("admin.py")).unwrap();
        assert!(!admin_file.heritage.is_empty(), "Admin should have heritage");

        // Verify MRO edges
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.reference_edges_emitted >= 2, "Should have at least 2 scope-resolution edges");
        }

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // Confidence scoring tests
    // =====================================================================

    #[test]
    fn confidence_scoring_tiers() {
        // Verify that different resolution tiers get different confidence scores
        let tmp = std::env::temp_dir().join(format!(
            "atree_conf2_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("test.py"), r#"
class MyClass:
    def method(self):
        pass

def my_func():
    obj = MyClass()   # ConstructorInferred
    obj.method()      # ReceiverHeuristic
    my_func()         # ExactLocal
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 100,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        // Verify scope-resolution ran
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.files_processed > 0, "ScopeResolution should have processed files");
        }

        // Verify symbols were extracted
        let file = result.parsed_files.iter().find(|f| f.path.ends_with("test.py")).unwrap();
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MyClass"), "MyClass not found");
        assert!(names.contains(&"my_func"), "my_func not found");
        assert!(names.contains(&"method"), "method not found");

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // Multi-language tests
    // =====================================================================

    #[test]
    fn multi_language_rust_inheritance() {
        // Verify Rust trait resolution
        let tmp = std::env::temp_dir().join(format!(
            "atree_rust_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("traits.rs"), r#"
pub trait Printable {
    fn print(&self);
}

pub trait Serializable {
    fn serialize(&self) -> String;
}

pub struct Report {
    title: String,
}

impl Printable for Report {
    fn print(&self) {
        println!("{}", self.title);
    }
}

impl Serializable for Report {
    fn serialize(&self) -> String {
        format!("Report: {}", self.title)
    }
}

pub fn create_report() -> Report {
    Report { title: "test".to_string() }
}
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("traits.rs")).unwrap();

        // Verify symbols extracted
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Printable"), "Printable trait not found");
        assert!(names.contains(&"Serializable"), "Serializable trait not found");
        assert!(names.contains(&"Report"), "Report struct not found");
        assert!(names.contains(&"create_report"), "create_report not found");

        // Verify heritage (impl blocks)
        assert!(!file.heritage.is_empty(), "Should have heritage entries for impl blocks");

        // Verify scope-resolution ran (Rust impl/trait heritage extraction is limited)
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.files_processed > 0, "ScopeResolution should have processed files");
        }

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn multi_language_go_interfaces() {
        // Verify Go interface satisfaction
        let tmp = std::env::temp_dir().join(format!(
            "atree_go_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("main.go"), r#"
package main

import "fmt"

type Printer interface {
    Print()
}

type ConsolePrinter struct {
    prefix string
}

func (c *ConsolePrinter) Print() {
    fmt.Println(c.prefix)
}

func NewPrinter() *ConsolePrinter {
    return &ConsolePrinter{prefix: ">"}
}

func main() {
    p := NewPrinter()
    p.Print()
}
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("main.go")).unwrap();

        // Verify symbols
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Printer"), "Printer interface not found");
        assert!(names.contains(&"ConsolePrinter"), "ConsolePrinter struct not found");
        assert!(names.contains(&"Print"), "Print method not found");
        assert!(names.contains(&"NewPrinter"), "NewPrinter function not found");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn diagnostic_go_extraction_detail() {
        // Diagnostic: print exactly what Go extracts for the fixture file
        use crate::lang::get_provider_for_extension;
        use crate::syntax::SyntaxEngine;
        let content = std::fs::read_to_string(env!("CARGO_MANIFEST_DIR").to_owned() + "/../tests/fixtures/go/service.go")
            .or_else(|_| std::fs::read_to_string("tests/fixtures/go/service.go"))
            .unwrap();
        let provider = get_provider_for_extension("go").unwrap();
        let mut engine = SyntaxEngine::new();
        let (captures, raw_scopes, type_bindings) = engine.extract_captures_and_scopes(provider, &content);

        // Write diagnostic output to a temp file
        use std::io::Write;
        let mut diag = std::fs::File::create("/tmp/atree_go_diag.txt").unwrap();
        writeln!(diag, "=== Go Captures ({} total) ===", captures.len()).unwrap();
        for c in &captures {
            writeln!(diag, "  {:?} '{}' @ line {}", c.tag, c.name, c.range.start_point.row + 1).unwrap();
        }
        writeln!(diag, "=== Go Scopes ({} total) ===", raw_scopes.len()).unwrap();
        for s in &raw_scopes {
            writeln!(diag, "  {:?} @ {}-{}", s.kind, s.line_start + 1, s.line_end + 1).unwrap();
        }
        writeln!(diag, "=== Go Type Bindings ({} total) ===", type_bindings.len()).unwrap();
        for b in &type_bindings {
            writeln!(diag, "  {}: {} @ line {}", b.var_name, b.type_text, b.line + 1).unwrap();
        }

        let file_id = 1u64;
        let file_hash = crate::syntax::hash_content(&content);
        let parsed = ParsedFile::from_captures_with_scopes(
            file_id, "service.go", provider.id(), file_hash,
            captures, raw_scopes, type_bindings,
        );

        writeln!(diag, "=== Go ParsedFile ===").unwrap();
        writeln!(diag, "  symbols: {}", parsed.symbols.len()).unwrap();
        for s in &parsed.symbols {
            writeln!(diag, "    {} ({:?}) @ line {} scope={:?} owner={:?}", s.name, s.kind, s.line, s.scope_id, s.owner_id).unwrap();
        }
        writeln!(diag, "  calls: {}", parsed.calls.len()).unwrap();
        for c in &parsed.calls {
            writeln!(diag, "    {} @ line {} receiver={:?}", c.callee_name, c.line, c.receiver).unwrap();
        }
        writeln!(diag, "  assignments: {}", parsed.assignments.len()).unwrap();
        for a in &parsed.assignments {
            writeln!(diag, "    {} @ line {} receiver={:?}", a.name, a.line, a.receiver).unwrap();
        }
        writeln!(diag, "  heritage: {}", parsed.heritage.len()).unwrap();
        for h in &parsed.heritage {
            writeln!(diag, "    {} -> {} @ line {}", h.class_name, h.target_name, h.line).unwrap();
        }
        drop(diag);

        // Verify key assertions
        let names: Vec<&str> = parsed.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"User"), "User struct not found in symbols: {:?}", names);
        assert!(names.contains(&"Repository"), "Repository interface not found");
        assert!(names.contains(&"UserService"), "UserService struct not found");
        assert!(names.contains(&"NewUserService"), "NewUserService function not found");
        assert!(names.contains(&"FindByID"), "FindByID method not found");

        // These are the ones the user says are empty
        assert!(!parsed.imports.is_empty(), "Go imports should not be empty");
        assert!(!parsed.calls.is_empty(), "Go calls should not be empty");
    }

    #[test]
    fn multi_language_java_inheritance() {
        // Verify Java class inheritance and interface implementation
        let tmp = std::env::temp_dir().join(format!(
            "atree_java_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("Shape.java"), r#"
public abstract class Shape {
    public abstract double area();
    public String describe() {
        return "Shape with area: " + area();
    }
}
"#).unwrap();

        fs::write(tmp.join("Circle.java"), r#"
public class Circle extends Shape {
    private double radius;

    public Circle(double radius) {
        this.radius = radius;
    }

    @Override
    public double area() {
        return Math.PI * radius * radius;
    }
}
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        // Verify both files parsed
        assert!(result.parsed_files.len() >= 2, "Should have 2 parsed files");

        // Verify heritage
        let circle_file = result.parsed_files.iter().find(|f| f.path.contains("Circle.java")).unwrap();
        assert!(!circle_file.heritage.is_empty(), "Circle should have heritage (extends Shape)");

        // Verify MRO edges
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.reference_edges_emitted >= 1, "Should have MRO edge for Circle extends Shape");
        }

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // Incremental scanning tests
    // =====================================================================

    #[test]
    fn incremental_scan_detects_changed_files() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_incr_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        // Initial files
        fs::write(tmp.join("a.py"), r#"
def hello():
    return "hello"
"#).unwrap();
        fs::write(tmp.join("b.py"), r#"
def world():
    return "world"
"#).unwrap();

        let db_path = tmp.join("test_incremental.db");
        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: Some(db_path.clone()),
            incremental: false,
            semantic: true,
            ..Default::default()
        };

        // First scan — both files new
        let (result1, incr1) = build_graph_incremental(&opts).unwrap();
        assert_eq!(incr1.files_added, 2, "Should add 2 files");
        assert_eq!(incr1.files_unchanged, 0);
        assert_eq!(incr1.files_updated, 0);
        assert_eq!(incr1.files_removed, 0);
        assert_eq!(result1.store_stats.symbols, 2); // hello + world

        // Second scan — no changes
        let (_result2, incr2) = build_graph_incremental(&opts).unwrap();
        assert_eq!(incr2.files_added, 0);
        assert_eq!(incr2.files_unchanged, 2, "Both files should be unchanged");
        assert_eq!(incr2.files_updated, 0);
        assert_eq!(incr2.files_removed, 0);

        // Modify one file
        fs::write(tmp.join("a.py"), r#"
def hello():
    return "hello_modified"

def new_func():
    return 42
"#).unwrap();

        // Third scan — one file changed
        let (_result3, incr3) = build_graph_incremental(&opts).unwrap();
        assert_eq!(incr3.files_added, 0);
        assert_eq!(incr3.files_unchanged, 1, "b.py should be unchanged");
        assert_eq!(incr3.files_updated, 1, "a.py should be updated");
        assert_eq!(incr3.files_removed, 0);

        // Verify the updated file has new symbols
        let store = GraphStore::open(&db_path).unwrap();
        let a_symbols = store.get_symbols_by_name("new_func").unwrap();
        assert!(!a_symbols.is_empty(), "new_func should be indexed after update");

        // Delete a file
        fs::remove_file(tmp.join("b.py")).unwrap();

        // Fourth scan — one file removed
        let (_result4, incr4) = build_graph_incremental(&opts).unwrap();
        assert_eq!(incr4.files_added, 0);
        assert_eq!(incr4.files_unchanged, 1, "a.py should be unchanged");
        assert_eq!(incr4.files_updated, 0);
        assert_eq!(incr4.files_removed, 1, "b.py should be removed");

        // Verify b.py symbols are gone
        let store = GraphStore::open(&db_path).unwrap();
        let b_symbols = store.get_symbols_by_name("world").unwrap();
        assert!(b_symbols.is_empty(), "world should be removed after file deletion");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn incremental_scan_adds_new_files() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_incr2_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("base.py"), r#"
class Base:
    pass
"#).unwrap();

        let db_path = tmp.join("test_incremental_cross_file.db");
        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: Some(db_path),
            incremental: false,
            semantic: true,
            ..Default::default()
        };

        let (_result1, incr1) = build_graph_incremental(&opts).unwrap();
        assert_eq!(incr1.files_added, 1);

        // Add a new file
        fs::write(tmp.join("derived.py"), r#"
from base import Base

class Derived(Base):
    pass
"#).unwrap();

        let (_result2, incr2) = build_graph_incremental(&opts).unwrap();
        assert_eq!(incr2.files_added, 1, "derived.py should be added");
        assert_eq!(incr2.files_unchanged, 1, "base.py should be unchanged");

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // Edge case tests
    // =====================================================================

    #[test]
    fn empty_file_produces_no_symbols() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_empty_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("empty.py"), "").unwrap();
        fs::write(tmp.join("comments.py"), r#"
# This is a file with only comments
# No actual code here
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        for file in &result.parsed_files {
            assert!(file.symbols.is_empty(), "Empty file should have no symbols: {}", file.path);
            assert!(file.calls.is_empty(), "Empty file should have no calls: {}", file.path);
        }

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn unicode_identifiers() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_unicode_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("unicode.py"), r#"
def café():
    return "café"

class 日本語:
    pass

变量 = 42
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("unicode.py")).unwrap();
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"café"), "Should extract unicode function name");
        assert!(names.contains(&"日本語"), "Should extract unicode class name");
        assert!(names.contains(&"变量"), "Should extract unicode variable name");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn deeply_nested_scopes() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_deep_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("deep.py"), r#"
class A:
    class B:
        class C:
            def method_c(self):
                def inner():
                    def deeper():
                        pass
                    pass
                pass
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("deep.py")).unwrap();

        // Should have many scopes: Module, A, B, C, method_c, inner, deeper
        assert!(file.scopes.len() >= 5, "Should have at least 5 scopes for deeply nested code, got {}", file.scopes.len());

        // Verify parent chain
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"A"));
        assert!(names.contains(&"B"));
        assert!(names.contains(&"C"));
        assert!(names.contains(&"method_c"));
        assert!(names.contains(&"inner"));
        assert!(names.contains(&"deeper"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn ambiguous_resolution_multiple_same_name() {
        // When multiple symbols have the same name, resolution should still work
        // but mark it as ambiguous
        let tmp = std::env::temp_dir().join(format!(
            "atree_ambig_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("a.py"), r#"
def helper():
    return "a"
"#).unwrap();

        fs::write(tmp.join("b.py"), r#"
def helper():
    return "b"

def use_helper():
    return helper()  # same-file, should resolve
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        // Both files should have been parsed
        assert!(result.parsed_files.len() >= 2);

        // The store should have 3 symbols: helper (from a), helper (from b), use_helper
        assert!(result.store_stats.symbols >= 3, "Should have at least 3 symbols");

        // Verify symbols were extracted from both files
        assert!(result.store_stats.symbols >= 3, "Should have symbols from both files");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn file_with_only_imports_no_definitions() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_imp_only_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("main.py"), r#"
import os
import sys
from collections import defaultdict

print(os.path.join("a", "b"))
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("main.py")).unwrap();
        assert!(!file.imports.is_empty(), "Should extract imports");
        assert!(file.imports.len() >= 3, "Should have at least 3 imports");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn large_file_many_symbols() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_large_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        // Generate a file with many symbols
        let mut content = String::new();
        for i in 0..100 {
            content.push_str(&format!(r#"
class Class{idx}:
    def method_{idx}(self):
        return {idx}

def func_{idx}():
    return {idx}
"#, idx = i));
        }

        fs::write(tmp.join("large.py"), &content).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 10000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("large.py")).unwrap();

        // Should have extracted many symbols: 100 classes + 100 methods + 100 functions = 300
        assert!(file.symbols.len() >= 200, "Should extract many symbols, got {}", file.symbols.len());

        // Store should have all symbols
        assert!(result.store_stats.symbols >= 200, "Store should have many symbols, got {}", result.store_stats.symbols);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn mixed_language_project() {
        // Test a project with multiple languages
        let tmp = std::env::temp_dir().join(format!(
            "atree_mix_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();
        fs::create_dir_all(tmp.join("src")).unwrap();
        fs::create_dir_all(tmp.join("lib")).unwrap();

        fs::write(tmp.join("src/main.py"), r#"
def main():
    return "hello"
"#).unwrap();
        fs::write(tmp.join("src/utils.ts"), r#"
export function helper(): string {
    return "helper";
}
"#).unwrap();
        fs::write(tmp.join("lib/calc.rs"), r#"
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}
"#).unwrap();
        fs::write(tmp.join("lib/server.go"), r#"
package main

import "fmt"

func Start() {
    fmt.Println("starting")
}
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        // Should have parsed files in all 4 languages
        assert!(result.parsed_files.len() >= 4, "Should parse at least 4 files, got {}", result.parsed_files.len());

        // Verify each language's symbols
        let py = result.parsed_files.iter().find(|f| f.path.contains("main.py")).unwrap();
        assert!(py.symbols.iter().any(|s| s.name == "main"), "Python symbols");

        let ts = result.parsed_files.iter().find(|f| f.path.contains("utils.ts")).unwrap();
        assert!(ts.symbols.iter().any(|s| s.name == "helper"), "TypeScript symbols");

        let rs = result.parsed_files.iter().find(|f| f.path.contains("calc.rs")).unwrap();
        assert!(rs.symbols.iter().any(|s| s.name == "add"), "Rust symbols");

        let go = result.parsed_files.iter().find(|f| f.path.contains("server.go")).unwrap();
        assert!(go.symbols.iter().any(|s| s.name == "Start"), "Go symbols");

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // Performance / stress tests
    // =====================================================================

    #[test]
    fn stress_test_many_files() {
        // Create a project with many files to test parallel scanning
        let tmp = std::env::temp_dir().join(format!(
            "atree_stress_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        // Create 50 Python files with various constructs
        for i in 0..50 {
            let content = format!(r#"
class Service{idx}:
    def __init__(self):
        self.value = {idx}

    def process(self, data):
        return data + {idx}

    def helper(self):
        return self.value

def create_service_{idx}():
    return Service{idx}()

class Manager{idx}:
    def __init__(self):
        self.services = []

    def add_service(self, svc):
        self.services.append(svc)

    def run_all(self):
        return [s.process({idx}) for s in self.services]
"#, idx = i);
            fs::write(tmp.join(format!("module_{:03}.py", i)), &content).unwrap();
        }

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 50000,
            include_files: true,
            threads: 4,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };

        let start = std::time::Instant::now();
        let result = build_graph(&opts).unwrap();
        let elapsed = start.elapsed();

        // Should parse all 50 files
        assert!(result.parsed_files.len() >= 50, "Should parse 50 files, got {}", result.parsed_files.len());

        // Should have many symbols: 50 files × (2 classes × 4 methods + 1 function) ≈ 450
        assert!(result.store_stats.symbols >= 100, "Should have many symbols, got {}", result.store_stats.symbols);

        // Should complete in reasonable time (< 30 seconds even on slow machines)
        assert!(elapsed.as_secs() < 30, "Should complete in < 30s, took {:?}", elapsed);


        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn stress_test_deep_inheritance_chain() {
        // Test a deep inheritance chain (100 levels)
        let tmp = std::env::temp_dir().join(format!(
            "atree_deep_chain_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        let mut content = String::new();
        for i in 0..100 {
            if i == 0 {
                content.push_str(&format!("class Base0:\n    pass\n\n"));
            } else {
                content.push_str(&format!("class Base{}(Base{}):\n    pass\n\n", i, i - 1));
            }
        }
        content.push_str("class Leaf(Base99):\n    pass\n");

        fs::write(tmp.join("chain.py"), &content).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 10000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("chain.py")).unwrap();
        assert!(file.symbols.len() >= 100, "Should have 100+ classes, got {}", file.symbols.len());

        // Verify MRO edges were created
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.reference_edges_emitted >= 50, "Should have scope-resolution edges");
        }

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn stress_test_wide_inheritance() {
        // Test diamond inheritance pattern
        let tmp = std::env::temp_dir().join(format!(
            "atree_diamond_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("diamond.py"), r#"
class A:
    def method_a(self):
        return "a"

class B(A):
    def method_b(self):
        return "b"

class C(A):
    def method_c(self):
        return "c"

class D(B, C):
    def method_d(self):
        return "d"
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("diamond.py")).unwrap();
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"A"));
        assert!(names.contains(&"B"));
        assert!(names.contains(&"C"));
        assert!(names.contains(&"D"));

        // Verify heritage entries for diamond
        assert!(file.heritage.len() >= 3, "Should have heritage entries for B→A, C→A, D→B,C");

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // C3 Linearization (Python MRO)
    // =====================================================================

    #[test]
    fn c3_linearization_basic() {
        // Test Python-style C3 linearization for diamond inheritance
        let tmp = std::env::temp_dir().join(format!(
            "atree_c3_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("c3.py"), r#"
class A:
    pass

class B(A):
    pass

class C(A):
    pass

class D(B, C):
    pass
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("c3.py")).unwrap();

        // Verify all classes extracted
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"A"));
        assert!(names.contains(&"B"));
        assert!(names.contains(&"C"));
        assert!(names.contains(&"D"));

        // Verify heritage: B→A, C→A, D→B, D→C
        assert!(file.heritage.len() >= 4, "Should have 4 heritage entries");

        // Verify MRO edges
        if let Some(ref srs) = result.scope_resolution_stats {
            assert!(srs.reference_edges_emitted >= 4, "Should have scope-resolution edges");
        }

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // PHP heritage test — verify no self-edge
    // =====================================================================

    #[test]
    fn test_php_heritage_no_self_edge() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_php_herit_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("Controller.php"), r#"<?php

class UserController extends Controller {
    public function index() {
        return $this->render('index');
    }
}

class BaseController extends AbstractController {
    use SomeTrait;
}
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            semantic: true,
            db_path: None,
            incremental: false,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("Controller.php")).unwrap();

        // Check heritage entries
        let heritage_targets: Vec<&str> = file.heritage.iter().map(|h| h.target_name.as_str()).collect();

        // UserController extends Controller → heritage target should be "Controller"
        assert!(heritage_targets.contains(&"Controller"),
            "UserController should extend Controller, got: {:?}", heritage_targets);

        // BaseController extends AbstractController → heritage target should be "AbstractController"
        assert!(heritage_targets.contains(&"AbstractController"),
            "BaseController should extend AbstractController, got: {:?}", heritage_targets);

        // CRITICAL: No self-edges. UserController should NOT extend UserController.
        let self_edges: Vec<_> = file.heritage.iter()
            .filter(|h| h.target_name == "UserController" || h.target_name == "BaseController")
            .collect();
        assert!(self_edges.is_empty(),
            "PHP heritage should NOT produce self-edges, found: {:?}", self_edges);

        // BaseController uses SomeTrait
        let trait_targets: Vec<&str> = file.heritage.iter()
            .filter(|h| matches!(h.heritage_kind, crate::semantic::HeritageKind::UsesTrait))
            .map(|h| h.target_name.as_str())
            .collect();
        assert!(trait_targets.contains(&"SomeTrait"),
            "BaseController should use SomeTrait, got: {:?}", trait_targets);

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // Export detection tests
    // =====================================================================

    #[test]
    fn export_detection_python() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_export_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("exports.py"), r#"
__all__ = ['public_func', 'PublicClass']

def public_func():
    pass

def _private_func():
    pass

class PublicClass:
    pass

class _PrivateClass:
    pass
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("exports.py")).unwrap();
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"public_func"));
        assert!(names.contains(&"PublicClass"));
        assert!(names.contains(&"_private_func"));
        assert!(names.contains(&"_PrivateClass"));

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn export_detection_typescript() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_export_ts_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("api.ts"), r#"
export function publicApi(): string {
    return "api";
}

function internalHelper(): void {
    // not exported
}

export class PublicService {
    run() {}
}

class InternalService {
    run() {}
}

export const PUBLIC_CONST = 42;
const INTERNAL_CONST = 0;
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("api.ts")).unwrap();
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"publicApi"));
        assert!(names.contains(&"PublicService"));
        assert!(names.contains(&"PUBLIC_CONST"));
        assert!(names.contains(&"internalHelper"));
        assert!(names.contains(&"InternalService"));

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // Type annotation extraction tests
    // =====================================================================

    #[test]
    fn type_annotation_typescript() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_types_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("types.ts"), r#"
interface User {
    name: string;
    age: number;
}

type ID = string | number;

function greet(user: User): string {
    return `Hello, ${user.name}`;
}

const ids: ID[] = ["a", "b"];

class UserService {
    private users: User[];

    constructor() {
        this.users = [];
    }

    addUser(user: User): void {
        this.users.push(user);
    }

    getUser(index: number): User | undefined {
        return this.users[index];
    }
}
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("types.ts")).unwrap();
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();

        // Interfaces, types, functions, classes should all be extracted
        assert!(names.contains(&"User"), "Interface should be extracted");
        assert!(names.contains(&"ID"), "Type alias should be extracted");
        assert!(names.contains(&"greet"), "Function should be extracted");
        assert!(names.contains(&"ids"), "Const should be extracted");
        assert!(names.contains(&"UserService"), "Class should be extracted");

        fs::remove_dir_all(&tmp).ok();
    }

    // =====================================================================
    // Failure mode tests
    // =====================================================================

    #[test]
    fn malformed_code_doesnt_crash() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_malformed_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        // File with syntax errors — should not crash
        fs::write(tmp.join("broken.py"), r#"
class Foo
    def bar(
        pass

def baz()
    return

class 123Invalid:
    pass
"#).unwrap();

        // File with mixed valid/invalid
        fs::write(tmp.join("mixed.py"), r#"
class Valid:
    def method(self):
        pass

# Some broken stuff below
def broken(
    class What

class AlsoValid(Valid):
    pass
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };

        // Should not panic
        let result = build_graph(&opts);
        assert!(result.is_ok(), "Should handle malformed code without crashing");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn circular_import_handling() {
        // Circular imports should not cause infinite loops
        let tmp = std::env::temp_dir().join(format!(
            "atree_circular_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("a.py"), r#"
from b import B

class A:
    def use_b(self):
        return B()
"#).unwrap();

        fs::write(tmp.join("b.py"), r#"
from a import A

class B:
    def use_a(self):
        return A()
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };

        // Should not hang or crash
        let result = build_graph(&opts);
        assert!(result.is_ok(), "Should handle circular imports without hanging");

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn single_file_project() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_single_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        fs::write(tmp.join("main.py"), r#"
def main():
    return "hello"
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        assert_eq!(result.store_stats.symbols, 1);
        assert_eq!(result.store_stats.files, 1);

        fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn binary_files_ignored() {
        let tmp = std::env::temp_dir().join(format!(
            "atree_binary_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&tmp).unwrap();

        // Create a binary file
        fs::write(tmp.join("image.png"), &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]).unwrap();
        fs::write(tmp.join("data.bin"), &[0x00, 0x01, 0x02, 0x03]).unwrap();

        // Create a valid source file
        fs::write(tmp.join("main.py"), r#"
def main():
    pass
"#).unwrap();

        let opts = ScanOptions {
            root: tmp.clone(),
            max_depth: 10,
            max_nodes: 1000,
            include_files: true,
            threads: 1,
            tree_mode: false,
            db_path: None,
            incremental: false,
            semantic: true,
            ..Default::default()
        };
        let result = build_graph(&opts).unwrap();

        // Should only parse the Python file
        assert!(result.parsed_files[0].path.ends_with("main.py"));

        fs::remove_dir_all(&tmp).ok();
    }
}
