//! `atree` CLI binary. The actual library lives in `lib.rs`.
//!
//! Output convention: status messages → stderr, data → stdout. This makes
//! the binary pipe-friendly and `--json` mode emit clean JSON to stdout.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use atree::{
    all_cores, astar, available_memory_bytes, bfs_expanded, build_graph,
    build_json_report, build_path_report, compute_depths, estimated_node_cap_for_half_memory,
    generate_dot, half_cores, human_size, print_tree, NodeMeta, ScanOptions, SCHEMA_JSON,
};
use rustc_hash::FxHashSet;

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// ---------------------------------------------------------------------
// Argument parsing
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
enum ThreadSpec {
    /// User did not pass `--threads`; default to half cores.
    Auto,
    /// User passed `--threads all`/`max`/`cores` or `--threads 0`.
    All,
    /// User passed an explicit non-zero count.
    Explicit(usize),
}

#[derive(Debug)]
struct Args {
    root: PathBuf,
    start: Option<String>,
    goal: Option<String>,
    max_depth: usize,
    max_nodes: usize,
    include_files: bool,
    ascii: bool,
    dot: bool,
    no_color: bool,
    threads: ThreadSpec,
    tree: bool,
    no_limit: bool,
    no_mem_cap: bool,
    semantic: bool,
    json: bool,
    print_schema: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            start: None,
            goal: None,
            max_depth: 4,
            max_nodes: 150,
            include_files: false,
            ascii: false,
            dot: false,
            no_color: false,
            threads: ThreadSpec::Auto,
            tree: false,
            no_limit: false,
            no_mem_cap: false,
            semantic: false,
            json: false,
            print_schema: false,
        }
    }
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let cli_args: Vec<String> = std::env::args().collect();

    fn take_value<'a>(cli_args: &'a [String], i: usize) -> &'a str {
        cli_args.get(i + 1).map(|s| s.as_str()).unwrap_or_else(|| {
            eprintln!("Error: flag '{}' requires a value", cli_args[i]);
            std::process::exit(2);
        })
    }

    fn parse_usize(flag: &str, raw: &str) -> usize {
        raw.parse().unwrap_or_else(|_| {
            eprintln!(
                "Error: flag '{}' expected a non-negative integer, got '{}'",
                flag, raw
            );
            std::process::exit(2);
        })
    }

    fn parse_threads(raw: &str) -> ThreadSpec {
        match raw.to_lowercase().as_str() {
            "all" | "max" | "cores" => ThreadSpec::All,
            "auto" | "half" => ThreadSpec::Auto,
            other => match other.parse::<usize>() {
                Ok(0) => ThreadSpec::All, // 0 historically meant "all"
                Ok(n) => ThreadSpec::Explicit(n),
                Err(_) => {
                    eprintln!(
                        "Error: --threads expected 'all', 'auto', or a number, got '{}'",
                        raw
                    );
                    std::process::exit(2);
                }
            },
        }
    }

    let mut i = 1;
    while i < cli_args.len() {
        match cli_args[i].as_str() {
            "--root" | "-r" | "--path" | "--dir" | "--directory" => {
                args.root = PathBuf::from(take_value(&cli_args, i));
                i += 1;
            }
            "--start" | "-s" | "--from" => {
                args.start = Some(take_value(&cli_args, i).to_string());
                i += 1;
            }
            "--goal" | "-g" | "--to" | "--target" | "--dest" => {
                args.goal = Some(take_value(&cli_args, i).to_string());
                i += 1;
            }
            "--max-depth" | "-d" | "-L" | "--depth" | "--maxdepth" | "--level" => {
                args.max_depth = parse_usize("--max-depth", take_value(&cli_args, i));
                i += 1;
            }
            "--max-nodes" | "-n" | "--limit" | "--cap" => {
                args.max_nodes = parse_usize("--max-nodes", take_value(&cli_args, i));
                i += 1;
            }
            "--include-files" | "-f" | "--files" | "--with-files" => {
                args.include_files = true;
            }
            "--ascii" | "-a" | "--plain" => args.ascii = true,
            "--dot" | "--graphviz" | "--graph" => args.dot = true,
            "--no-color" | "-C" | "--monochrome" | "--mono" => args.no_color = true,
            "--threads" | "-j" | "--jobs" | "--workers" => {
                args.threads = parse_threads(take_value(&cli_args, i));
                i += 1;
            }
            "--tree" | "-t" | "--fast" | "--no-stat" | "--map" => args.tree = true,
            "--no-limit" | "--unlimited" | "--no-cap" => args.no_limit = true,
            "--no-mem-cap" | "--hard" => args.no_mem_cap = true,
            "--semantic" => args.semantic = true,
            "--json" => args.json = true,
            "--print-schema" | "--schema" => args.print_schema = true,
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!(
                    "atree {} — UnityAILab (contact@unityailab.com)",
                    env!("CARGO_PKG_VERSION")
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("Error: unknown argument '{}'. Try --help.", other);
                std::process::exit(2);
            }
        }
        i += 1;
    }
    args
}

fn print_help() {
    println!(
        r#"atree — Parallel filesystem analysis & A* pathfinder
v{} — UnityAILab — contact@unityailab.com

Shows your folders/files as an ASCII/Unicode tree + finds the optimal
navigation path using A* (with efficiency stats vs blind search).
Outputs human-readable trees, Graphviz DOT, or JSON.
The full JSON Schema (Draft 7) is bundled — run `atree --print-schema`.

Usage: atree [OPTIONS]"#,
        env!("CARGO_PKG_VERSION")
    );
    println!(
        r#"

Options:
  -r, --root <PATH>          Root directory to scan (default: .)
  -s, --start <NODE>         Starting node relative path (e.g. "src")
  -g, --goal <NODE>          Goal node relative path (e.g. "src/main.rs")
  -d, --max-depth <N>        Max folder depth (default: 4)
  -n, --max-nodes <N>        Max nodes in map (default: 150)
  -f, --include-files        Include files as leaf nodes (default: folders only)
  -a, --ascii                Use pure ASCII tree characters (no Unicode)
      --dot                  Also generate Graphviz DOT file (optional)
      --no-color             Disable colored output (blue folders, green executables)
  -j, --threads <N|all>      Worker threads. 'all' = every core, default = half cores.
  -t, --tree                 Tree-mapping mode: skip per-file stat for max scan speed
                             (no size column, no exec coloring, no [rwx] badge)
      --no-limit             Remove --max-depth and --max-nodes caps (scan everything)
      --no-mem-cap           With --no-limit, disable the ~half-RAM safety cap
      --semantic             Enable code intelligence: extract symbols via tree-sitter
      --json                 Emit a JSON report on stdout (status still goes to stderr)
      --print-schema         Print the bundled JSON Schema (Draft 7) on stdout and exit
  -h, --help                 Show this help
  -V, --version              Print version and exit

Aliases (familiar names from find/tree/du also work):
  --root              --path, --dir, --directory
  --start             --from
  --goal              --to, --target, --dest
  --max-depth         -L, --depth, --maxdepth, --level
  --max-nodes         --limit, --cap
  --include-files     --files, --with-files
  --ascii             --plain
  --dot               --graphviz, --graph
  --no-color          -C, --monochrome, --mono
  --threads           --jobs, --workers
  --tree              --fast, --no-stat, --map
  --no-limit          --unlimited, --no-cap
  --no-mem-cap        --hard

Examples:
  atree --root /home/user --max-depth 3
  atree -r . -s src -g Cargo.toml -f
  atree --path /usr -L 5 --files --fast --no-limit
  atree --dir ~/code --jobs all --tree --json > report.json
  atree --root ~/project --semantic --json > code_intel.json

The main output is a clean tree view with the A* path highlighted.
Status messages go to stderr, data goes to stdout — the binary is pipeable.

UnityAILab is a sovereign, independent team. Not affiliated with Unity
Technologies or the Unity game engine. See NOTICE for details.
"#
    );
}

// ---------------------------------------------------------------------
// Resource resolution
// ---------------------------------------------------------------------

fn resolve_threads(spec: &ThreadSpec) -> usize {
    match spec {
        ThreadSpec::Auto => half_cores(),
        ThreadSpec::All => all_cores(),
        ThreadSpec::Explicit(n) => (*n).max(1),
    }
}

/// Apply `--no-limit` and the soft memory cap. Returns the resolved
/// `(max_depth, max_nodes, mem_capped: bool)`.
fn resolve_caps(args: &Args) -> (usize, usize, bool) {
    if !args.no_limit {
        return (args.max_depth, args.max_nodes, false);
    }
    let max_depth = usize::MAX;
    if args.no_mem_cap {
        return (max_depth, usize::MAX, false);
    }
    match estimated_node_cap_for_half_memory() {
        Some(cap) => (max_depth, cap, true),
        None => (max_depth, usize::MAX, false), // can't query → trust user
    }
}

// ---------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------

fn main() {
    let args = parse_args();

    // Handled before any scan work so the schema can be retrieved with no
    // filesystem access at all.
    if args.print_schema {
        let stdout = io::stdout();
        let mut h = stdout.lock();
        let _ = h.write_all(SCHEMA_JSON.as_bytes());
        if !SCHEMA_JSON.ends_with('\n') {
            let _ = writeln!(h);
        }
        return;
    }

    let start_time = Instant::now();
    let threads = resolve_threads(&args.threads);
    let (max_depth, max_nodes, mem_capped) = resolve_caps(&args);

    let opts = ScanOptions {
        root: args.root.clone(),
        max_depth,
        max_nodes,
        include_files: args.include_files,
        threads,
        tree_mode: args.tree,
        semantic: args.semantic,
    };

    if !args.json {
        eprintln!(
            "\x1b[1;36m>>> Building file system logical map (Rust - fancy colored tree edition)...\x1b[0m"
        );
        let mode_tags = match (args.tree, args.no_limit) {
            (true, true) => " [tree mode, no limits]",
            (true, false) => " [tree mode]",
            (false, true) => " [no limits]",
            (false, false) => "",
        };
        let semantic_tag = if args.semantic { " [semantic]" } else { "" };
        eprintln!("[INFO] Parallel scan with {} thread(s){}{}", threads, mode_tags, semantic_tag);
        if mem_capped {
            let mem_str = available_memory_bytes()
                .map(human_size)
                .unwrap_or_else(|| "unknown".into());
            eprintln!(
                "[INFO] Memory soft-cap: ~{} nodes (~half of {} available). Pass --no-mem-cap to disable.",
                max_nodes, mem_str
            );
        }
    }

    let scan = match build_graph(&opts) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error scanning directory: {}", e);
            std::process::exit(1);
        }
    };

    let elapsed = start_time.elapsed();
    let stats = &scan.stats;

    if !args.json {
        eprintln!(
            "[OK] Map built in {:.2?}: {} nodes ({} folders, {} files, {} symlinks, {} exec, {} hidden) — total size {}",
            elapsed,
            stats.total_nodes,
            stats.folders,
            stats.files,
            stats.symlinks,
            stats.executables,
            stats.hidden,
            human_size(stats.total_size_bytes),
        );
        if scan.truncated {
            eprintln!(
                "[WARN] Scan truncated at {} nodes (cap reached).",
                stats.total_nodes
            );
        }
        if args.semantic {
            let total_defs: usize = scan.parsed_files.iter().map(|f| f.symbols.len()).sum();
            let total_calls: usize = scan.parsed_files.iter().map(|f| f.calls.len()).sum();
            let total_resolved = scan.store_stats.resolved_calls as usize;
            let total_unresolved = total_calls - total_resolved;
            let total_edges = scan.store_stats.edges as usize;
            eprintln!(
                "[SEMANTIC] Extracted {} symbols, {} calls ({} resolved, {} unresolved) across {} files",
                total_defs, total_calls, total_resolved, total_unresolved, scan.parsed_files.len()
            );
            eprintln!(
                "[SEMANTIC] Store: {} symbols, {} calls, {} edges",
                scan.store_stats.symbols, scan.store_stats.calls, total_edges
            );
        }
    }

    // Resolve start/goal, run A*, and assemble the path report.
    let (mut start, goal, path_result) = compute_path(&args, &scan);

    let depths = compute_depths(&scan.adj, &scan.root_name);

    // ---------- JSON branch ----------
    if args.json {
        let path_report = path_result.as_ref().map(|(path, ax_exp, bfs_exp)| {
            build_path_report(&start, &goal, path, *ax_exp, *bfs_exp)
        });
        let report = build_json_report(
            &scan,
            &opts,
            &depths,
            path_report,
            elapsed.as_secs_f64() * 1000.0,
        );
        let stdout = io::stdout();
        let mut h = stdout.lock();
        if let Err(e) = serde_json::to_writer(&mut h, &report) {
            eprintln!("Error writing JSON: {}", e);
            std::process::exit(1);
        }
        let _ = writeln!(h);
        return;
    }

    // ---------- Human-readable branch ----------
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let path_set: FxHashSet<String> = match &path_result {
        Some((path, _, _)) => path.iter().cloned().collect(),
        None => [start.clone()].into_iter().collect(),
    };

    if let Some((path, ax_exp, bfs_exp)) = &path_result {
        let savings = if *bfs_exp > 0 {
            100.0 * (1.0 - *ax_exp as f64 / *bfs_exp as f64)
        } else {
            0.0
        };
        eprintln!(
            "[OK] A* found path with {} hops. Expanded {} nodes.",
            path.len() - 1,
            ax_exp
        );
        eprintln!(
            "   Blind BFS would expand {} nodes → A* is {:.1}% more efficient!",
            bfs_exp, savings
        );

        let _ = writeln!(out, "\n>>> Optimal Navigation Path (A*):");
        for (i, node) in path.iter().enumerate() {
            let m = scan.meta.get(node).cloned().unwrap_or_else(|| NodeMeta {
                is_dir: true,
                is_symlink: false,
                is_hidden: false,
                is_exec: false,
                mode: 0,
                size: 0,
                name: node.clone(),
            });
            let icon = if m.is_dir {
                "[DIR]"
            } else if m.is_symlink {
                "[LNK]"
            } else {
                "[FILE]"
            };
            let _ = writeln!(out, "   {}. {} {}  ({})", i + 1, icon, m.name, node);
        }
        let _ = writeln!(
            out,
            "\n   Total hops: {}  |  Shortest logical path through your folder structure.",
            path.len() - 1
        );
    } else if start == goal {
        eprintln!("[INFO] Start and goal are identical. Path length = 0.");
        let _ = writeln!(out, "\n>>> Optimal Navigation Path (A*):");
        if let Some(sm) = scan.meta.get(&start) {
            let icon = if sm.is_dir {
                "[DIR]"
            } else if sm.is_symlink {
                "[LNK]"
            } else {
                "[FILE]"
            };
            let _ = writeln!(out, "   1. {} {}", icon, sm.name);
        }
        let _ = &mut start;
    }

    let _ = print_tree(
        &mut out,
        &scan.adj,
        &scan.meta,
        &scan.root_name,
        &depths,
        args.ascii,
        args.no_color,
        &path_set,
    );

    if args.dot {
        let dot_file = "atree_map.dot";
        if let Err(e) = generate_dot(
            &scan.adj,
            &scan.meta,
            &depths,
            path_result.as_ref().map(|(p, _, _)| p.as_slice()).unwrap_or(&[]),
            &scan.root_name,
            dot_file,
        ) {
            eprintln!("[WARN] Could not write DOT file: {}", e);
        } else {
            eprintln!("[OK] Graphviz DOT file written: {}", dot_file);
            eprintln!("     Render it with:  dot -Tpng {} -o atree_map.png", dot_file);
        }
    }

    let (sem_files, sem_defs, sem_calls, sem_resolved, sem_unresolved, sem_nodes, sem_edges) = if args.semantic {
        let files = scan.parsed_files.len();
        let defs: usize = scan.parsed_files.iter().map(|f| f.symbols.len()).sum();
        let calls: usize = scan.parsed_files.iter().map(|f| f.calls.len()).sum();
        let resolved = scan.store_stats.resolved_calls as usize;
        let unresolved = calls - resolved;
        let nodes = scan.store_stats.symbols as usize;
        let edges = scan.store_stats.edges as usize;
        (files, defs, calls, resolved, unresolved, nodes, edges)
    } else {
        (0, 0, 0, 0, 0, 0, 0)
    };

    print_summary_box(&args, threads, elapsed, stats, sem_files, sem_defs, sem_calls, sem_resolved, sem_unresolved, sem_nodes, sem_edges);
}

/// Resolve start/goal (with auto-pick fallback) and run A* + BFS comparison.
/// Returns `(start, goal, Option<(path, astar_expanded, bfs_expanded)>)`.
fn compute_path(
    args: &Args,
    scan: &atree::ScanResult,
) -> (String, String, Option<(Vec<String>, usize, usize)>) {
    let mut start = args.start.clone().unwrap_or_else(|| scan.root_name.clone());
    if !scan.adj.contains_key(&start) {
        if !args.json {
            eprintln!(
                "[WARN] Start '{}' not found, using root '{}'",
                start, scan.root_name
            );
        }
        start = scan.root_name.clone();
    }

    let auto_pick_leaf = || -> Option<String> {
        let mut leaves: Vec<&String> = scan
            .adj
            .iter()
            .filter(|(n, neis)| neis.len() <= 1 && n.as_str() != start.as_str())
            .map(|(n, _)| n)
            .collect();
        if leaves.is_empty() {
            return None;
        }
        leaves.sort();
        Some(leaves[leaves.len() / 2].clone())
    };

    let goal = match args.goal.clone() {
        Some(g) if scan.adj.contains_key(&g) => g,
        Some(g) => match auto_pick_leaf() {
            Some(pick) => {
                if !args.json {
                    eprintln!("[INFO] Goal '{}' not found. Auto-picked: {}", g, pick);
                }
                pick
            }
            None => start.clone(),
        },
        None => match auto_pick_leaf() {
            Some(pick) => {
                if !args.json {
                    eprintln!("[INFO] No goal specified. Auto-picked interesting leaf: {}", pick);
                }
                pick
            }
            None => start.clone(),
        },
    };

    if start == goal {
        return (start, goal, None);
    }

    let depths = compute_depths(&scan.adj, &scan.root_name);
    if !args.json {
        eprintln!("[SEARCH] Running A* from '{}' to '{}' ...", start, goal);
    }
    let astar_result = astar(&scan.adj, &start, &goal, &depths);
    match astar_result {
        Some((path, ax_exp)) => {
            let bfs_exp = bfs_expanded(&scan.adj, &start, &goal);
            (start, goal, Some((path, ax_exp, bfs_exp)))
        }
        None => {
            eprintln!("[ERROR] No path found (graph may be disconnected)");
            std::process::exit(1);
        }
    }
}

fn print_summary_box(args: &Args, threads: usize, elapsed: std::time::Duration, stats: &atree::Stats, semantic_files: usize, semantic_defs: usize, semantic_calls: usize, semantic_resolved: usize, semantic_unresolved: usize, semantic_nodes: usize, semantic_edges: usize) {
    let stderr = io::stderr();
    let mut out = stderr.lock();
    let box_top = if args.ascii { "+--[ SUMMARY ]--" } else { "┌──[ SUMMARY ]──" };
    let box_bot = if args.ascii { "`--[ A* READY ]--" } else { "└──[ A* READY ]──" };
    let color_title = if args.no_color { "" } else { "\x1b[1;36m" };
    let color_reset = if args.no_color { "" } else { "\x1b[0m" };

    let _ = writeln!(out, "\n{}{}", color_title, box_top);
    let _ = writeln!(out, "│  Scan time      : {:.2?}", elapsed);
    let _ = writeln!(
        out,
        "│  Total nodes    : {} ({} folders + {} files + {} symlinks)",
        stats.total_nodes, stats.folders, stats.files, stats.symlinks
    );
    let _ = writeln!(
        out,
        "│  Executables    : {}  |  Hidden: {}",
        stats.executables, stats.hidden
    );
    let _ = writeln!(out, "│  Total size     : {}", human_size(stats.total_size_bytes));
    let _ = writeln!(out, "│  Threads        : {}", threads);
    if args.semantic {
        let _ = writeln!(out, "│  Semantic       : {} defs, {} calls ({} resolved, {} unresolved) in {} files", semantic_defs, semantic_calls, semantic_resolved, semantic_unresolved, semantic_files);
        let _ = writeln!(out, "│  Code graph     : {} nodes, {} edges", semantic_nodes, semantic_edges);
    }
    let _ = writeln!(out, "│");
    let _ = writeln!(
        out,
        "│  Color legend   : {}📁 Blue=Folder  🔗 Cyan=Symlink  📄 Green=Exec  White=File{} (dim if hidden)",
        if args.no_color { "" } else { "\x1b[1;34m" },
        color_reset
    );
    let _ = writeln!(out, "│  Permission     : [rwx]  r=read(green)  w=write(yellow)  x=exec(magenta)");
    let _ = writeln!(out, "│  * marker       : Nodes on the optimal A* navigation path");
    let _ = writeln!(out, "{}{}", color_title, box_bot);
    let _ = writeln!(out, "{}", color_reset);
}
