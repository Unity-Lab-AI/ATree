use crate::lang::get_provider_for_extension;
use crate::semantic::ParsedFile;
use crate::syntax::SyntaxEngine;
use crate::resolver::{SemanticModel, SymbolTable};
use crate::graph::CodeGraph;
pub mod lang;
pub mod syntax;
pub mod semantic;
pub mod resolver;
pub mod graph;
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
    pub root: PathBuf,
    pub max_depth: usize,
    pub max_nodes: usize,
    pub include_files: bool,
    pub threads: usize,
    pub tree_mode: bool,
}

/// Result of a successful [`build_graph`] call.
pub struct ScanResult {
    pub parsed_files: Vec<crate::semantic::ParsedFile>,
    pub semantic_model: SemanticModel,
    pub symbol_table: SymbolTable,
    pub code_graph: CodeGraph,
    pub adj: FxHashMap<String, Vec<String>>,
    pub root_name: String,
    pub meta: FxHashMap<String, NodeMeta>,
    pub stats: Stats,
    /// `true` when the scan stopped early because `max_nodes` was reached.
    pub truncated: bool,
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
pub const SCHEMA_JSON: &str = include_str!("../docs/schema.json");

/// Top-level JSON output schema. Use [`build_json_report`] to construct.
///
/// Keys are sorted (`BTreeMap`) so the output is deterministic and diff-able.
/// Pin [`SCHEMA_VERSION`] to detect format changes; `version` is the binary
/// version (changes more often, doesn't necessarily mean schema changed).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonReport {
    pub semantic: Option<Vec<crate::semantic::ParsedFileOutput>>,
    pub semantic_model: Option<SemanticModel>,
    pub code_graph: Option<CodeGraph>,
    pub symbol_table: Option<SymbolTable>,
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
    ((cores + 1) / 2).max(1)
}

/// Available system memory in bytes (Linux-only via `/proc/meminfo`).
/// Returns `None` on platforms where this can't be determined.
pub fn available_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let content = std::fs::read_to_string("/proc/meminfo").ok()?;
        for line in content.lines() {
            if let Some(rest) = line.strip_prefix("MemAvailable:") {
                let parts: Vec<&str> = rest.trim().split_whitespace().collect();
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
    pub parsed_files: Vec<crate::semantic::ParsedFile>,
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

fn try_steal(stealers: &[Stealer<Job>]) -> Option<Job> {
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
    let prev = node_count.fetch_add(1, Ordering::Relaxed);
    if prev >= max_nodes {
        node_count.fetch_sub(1, Ordering::Relaxed);
        false
    } else {
        true
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
    syntax: &mut SyntaxEngine,
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
                if let Some(provider) = get_provider_for_extension(ext) {
                    if let Ok(content) = fs::read_to_string(&entry_path) {
                        let file_hash = {
                            use std::hash::{Hash, Hasher};
                            let mut h = std::collections::hash_map::DefaultHasher::new();
                            content.hash(&mut h);
                            h.finish()
                        };
                        let file_id = file_hash; // use hash as unique ID
                        let captures = syntax.extract_captures(provider, &content);
                        let parsed = ParsedFile::from_captures(file_id, &child_rel, provider.id(), file_hash, captures);
                        local.parsed_files.push(parsed);
                    }
                }
            }
        }

        if is_dir && !is_symlink {
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
                    let mut syntax = SyntaxEngine::new();
                    let mut local = LocalAccum::default();
                    let hint = (max_nodes.min(1 << 18) / n).max(16);
                    local.adj.reserve(hint);
                    local.meta.reserve(hint);

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

                        process_dir(&job.0, &job.1, job.2, opts_ref, root_name_ref, &mut local, node_count_ref, &my_queue, pending_ref, &mut syntax);
                        pending_ref.fetch_sub(1, Ordering::Release);
                    }
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

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

    let mut parsed_files = Vec::new();
    for local in &locals {
        parsed_files.extend(local.parsed_files.clone());
    }

    // Build the semantic model (in-memory hot-path index with resolution)
    let semantic_model = SemanticModel::build_from_parsed(parsed_files.clone());

    // Build flat symbol table from the resolved model
    let symbol_table = SymbolTable::from_model(&semantic_model);

    // Build the code graph from the resolved model
    let code_graph = CodeGraph::from_model(&semantic_model);

    Ok(ScanResult {
        parsed_files,
        semantic_model,
        symbol_table,
        code_graph,
        adj,
        root_name,
        meta,
        stats,
        truncated,
    })
}

// =====================================================================
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
        semantic_model: if options.semantic { Some(scan.semantic_model.clone()) } else { None },
        code_graph: if options.semantic { Some(scan.code_graph.clone()) } else { None },
        symbol_table: if options.semantic { Some(scan.symbol_table.clone()) } else { None },
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
            semantic: false,
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
            semantic: false,
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
            semantic: false,
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
            semantic: false,
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
            semantic: false,
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
            semantic: true,
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

        // ---- Verify semantic model ----
        let model = &result.semantic_model;
        assert!(model.total_files >= 4, "SemanticModel: expected >=4 files, got {}", model.total_files);
        assert!(model.total_symbols > 0, "SemanticModel: no symbols indexed");
        assert!(model.total_calls > 0, "SemanticModel: no calls found");

        // Verify same-file resolution in semantic model
        let sm_rust = result.semantic_model.files.values().find(|f| f.path.ends_with("main.rs")).expect("main.rs not in semantic model");
        let create_service_call = sm_rust.calls.iter().find(|c| c.callee_name == "create_service");
        if let Some(call) = create_service_call {
            assert_eq!(call.confidence, crate::semantic::Confidence::ExactLocal,
                "create_service() call should be ExactLocal, got {:?}", call.confidence);
        }

        // Verify TypeScript heritage resolution in semantic model
        let sm_ts = result.semantic_model.files.values().find(|f| f.path.ends_with("service.ts")).expect("service.ts not in semantic model");
        assert!(!sm_ts.heritage.is_empty(), "TS: no heritage found in semantic model");
        let extends = sm_ts.heritage.iter().find(|h| h.target_name == "BaseService");
        assert!(extends.is_some(), "TS: extends BaseService heritage not found");

        // ---- Verify code graph ----
        let graph = &result.code_graph;
        assert!(!graph.nodes.is_empty(), "CodeGraph: no nodes");
        assert!(!graph.edges.is_empty(), "CodeGraph: no edges");

        // Verify file nodes exist
        let file_nodes: Vec<_> = graph.nodes.iter().filter(|n| n.node_type == "file").collect();
        assert!(file_nodes.len() >= 4, "CodeGraph: expected >=4 file nodes, got {}", file_nodes.len());

        // Verify symbol nodes exist
        let sym_nodes: Vec<_> = graph.nodes.iter().filter(|n| n.node_type == "symbol").collect();
        assert!(sym_nodes.len() > 0, "CodeGraph: no symbol nodes");

        // Verify defines edges exist
        let defines_edges: Vec<_> = graph.edges.iter().filter(|e| e.edge_type == "defines").collect();
        assert!(!defines_edges.is_empty(), "CodeGraph: no defines edges");

        // Verify resolution edges exist (calls → symbols)
        let resolves_edges: Vec<_> = graph.edges.iter().filter(|e| e.edge_type == "resolves_to").collect();
        assert!(!resolves_edges.is_empty(), "CodeGraph: no resolves_to edges");

        // Verify confidence scores on edges
        let high_conf: Vec<_> = resolves_edges.iter().filter(|e| e.confidence >= 0.9).collect();
        assert!(!high_conf.is_empty(), "CodeGraph: no high-confidence resolution edges");

        // ---- Verify JSON roundtrip ----
        let depths = compute_depths(&result.adj, &result.root_name);
        let report = build_json_report(&result, &opts, &depths, None, 1.0);
        let json = serde_json::to_string(&report).expect("serialize");
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("deserialize");

        // Verify semantic array in JSON
        let semantic = parsed["semantic"].as_array().expect("semantic array");
        assert!(semantic.len() >= 4, "JSON: expected >=4 semantic entries");

        // Verify code_graph in JSON
        let cg = parsed["code_graph"].as_object().expect("code_graph object");
        assert!(cg["nodes"].as_array().unwrap().len() > 0, "JSON: code_graph has no nodes");
        assert!(cg["edges"].as_array().unwrap().len() > 0, "JSON: code_graph has no edges");

        // Verify semantic_model in JSON
        let sm = parsed["semantic_model"].as_object().expect("semantic_model object");
        assert!(sm["total_symbols"].as_u64().unwrap() > 0, "JSON: semantic_model has no symbols");

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
            semantic: true,
        };
        let result = build_graph(&opts).unwrap();

        // The main.rs file should have calls to helper and process (check parsed_files for extraction)
        let main_file = result.parsed_files.iter().find(|f| f.path.ends_with("main.rs")).expect("main.rs not found");
        let main_call_names: Vec<&str> = main_file.calls.iter().map(|c| c.callee_name.as_str()).collect();
        assert!(main_call_names.contains(&"helper"), "main.rs should call helper, got {:?}", main_call_names);
        assert!(main_call_names.contains(&"process"), "main.rs should call process, got {:?}", main_call_names);

        // Check resolution in the semantic model's copy of the file
        let sm_main = result.semantic_model.files.values().find(|f| f.path.ends_with("main.rs")).expect("main.rs not in semantic model");
        let resolved_count = sm_main.calls.iter()
            .filter(|c| c.confidence != crate::semantic::Confidence::Unresolved)
            .count();
        assert!(resolved_count > 0, "Expected some resolved calls in semantic model, got 0 out of {}", sm_main.calls.len());

        // Verify the semantic model has both files
        assert!(result.semantic_model.total_files >= 2);

        // Verify the symbol table has both helper and process
        assert!(result.symbol_table.resolve("helper").is_some());
        assert!(result.symbol_table.resolve("process").is_some());

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
            semantic: true,
        };
        let result = build_graph(&opts).unwrap();

        let file = result.parsed_files.iter().find(|f| f.path.ends_with("test.py")).expect("test.py not found");

        // Verify symbols were extracted
        let names: Vec<&str> = file.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"MyClass"), "MyClass not found in {:?}", names);
        assert!(names.contains(&"my_func"), "my_func not found in {:?}", names);

        // Check resolution in the semantic model's copy
        let sm_file = result.semantic_model.files.values().find(|f| f.path.ends_with("test.py")).expect("test.py not in semantic model");
        let my_func_call = sm_file.calls.iter().find(|c| c.callee_name == "my_func");
        assert!(my_func_call.is_some(), "my_func() call not found in semantic model");
        assert_eq!(my_func_call.unwrap().confidence, crate::semantic::Confidence::ExactLocal,
            "my_func() should be ExactLocal, got {:?}", my_func_call.unwrap().confidence);

        // Verify stats
        assert!(result.semantic_model.total_resolved > 0, "Should have resolved calls");
        assert!(result.semantic_model.total_symbols >= 2, "Should have at least 2 symbols");

        fs::remove_dir_all(&tmp).ok();
    }
}
