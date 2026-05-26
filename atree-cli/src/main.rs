//! `atree` CLI binary. The actual library lives in `lib.rs`.
//!
//! Output convention: status messages → stderr, data → stdout. This makes
//! the binary pipe-friendly and `--json` mode emit clean JSON to stdout.

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use atree_engine::{
    all_cores, astar, available_memory_bytes, bfs_expanded, build_graph, build_graph_group,
    build_graph_incremental, build_json_report, build_path_report, compute_depths,
    estimated_node_cap_for_half_memory, generate_dot, half_cores, human_size, print_tree,
    GroupConfig, NodeMeta, ScanOptions, SCHEMA_JSON,
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
enum QueryCommand {
    /// Search symbols by name (fuzzy)
    Symbols { name: String },
    /// Show callers of a symbol
    Callers { symbol: String, depth: usize },
    /// Show callees of a symbol
    Callees { symbol: String, depth: usize },
    /// Show impact analysis (upstream + downstream)
    Impact { symbol: String, depth: usize },
    /// 360-degree symbol context (refs, heritage, processes)
    Context { symbol: String },
    /// List detected API routes
    Routes,
    /// Full-text search
    Search { query: String },
    /// Semantic vector search (requires embeddings)
    SemanticSearch { query: String },
    /// Show index statistics
    Stats,
    /// List repos in the group index
    Repos,
    /// Detect uncommitted changes and affected symbols
    DetectChanges,
    /// Rename a symbol across the codebase
    Rename { symbol_name: String, new_name: String, dry_run: bool },
    /// Execute a raw SQL query against the index
    Cypher { query: String },
    /// Check API route response shapes
    ShapeCheck { route: Option<String> },
    /// Show tool-like symbols
    ToolMap,
    /// API route impact analysis
    ApiImpact { route: Option<String>, file: Option<String> },
    /// Run project verification (tests/lint/typecheck)
    Verify { verify_type: String, command: Option<String> },
    /// Sync group contract registry
    GroupSync { name: String },
    /// Explain a symbol: what it does, how it's used, and its role in the codebase
    ExplainSymbol { symbol: String },
    /// Find entry points (main functions, exported handlers, public API)
    FindEntrypoints,
    /// Trace a call path from caller to callee
    TraceCallPath { from: String, to: String },
    /// Show the public API surface of a module or the whole project
    PublicApiSurface { module: Option<String> },
    /// Find tests affected by changes to a symbol
    AffectedTests { symbol: String },
    /// Generate a validation plan for a proposed change
    ValidationPlan { symbol: String },
    /// Detect contract changes (breaking changes to public API)
    ContractChangeDetector { base_ref: Option<String> },
    /// Check architecture boundary violations
    ArchitectureBoundaryCheck,
    /// Detect scope violations (private symbol used externally)
    ScopeViolationDetector,
    /// Map configuration surface (env vars, config files, feature flags)
    ConfigSurfaceMap,
    /// Impact analysis filtered by symbol kind
    ImpactBySymbolKind { target: String, kind: String, direction: String },
    /// Semantic diff summary between current state and a base ref
    SemanticDiffSummary { base_ref: Option<String> },
    /// Scan for side effects (I/O, global state, network calls)
    SideEffectScanner { symbol: String },
    /// Show change coupling (symbols that change together)
    ChangeCoupling { symbol: String },
    /// Detect concurrency surfaces (locks, async, shared state)
    ConcurrencySurfaceDetector { symbol: String },
    /// Find the minimal edit scope for a change
    MinimalEditScope { symbol: String },
    /// Locate code related to an issue/ticket description
    IssueToCodeLocator { issue: String },
    /// Detect documentation drift (docs that don't match code)
    DocsDriftDetector,
    /// Check if a rename is safe
    RenameSafetyCheck { symbol_name: String, new_name: String },
    /// Find dead code candidates
    DeadCodeCandidates,
    /// Find ownership hotspots (symbols with many dependents)
    OwnershipHotspots,
    /// Trace error paths from a symbol
    ErrorPathTrace { symbol: String },
    /// Map resource lifecycle (creation, usage, cleanup)
    ResourceLifecycleMap { resource: String },
    /// Detect dependency cycles
    DependencyCycleDetector,
    /// Find uncovered symbols (no callers/tests)
    FindUncoveredSymbols,
    /// Show resolution quality stats (call/import resolution rates per language)
    ResolutionStats,
    /// Find evidence paths for a query using A* + beam search over the layered graph
    EvidencePath { query: String, max_depth: usize, beam_width: usize, max_evidence: usize },
    /// Show commit history for a file
    FileHistory { path: String, limit: usize },
    /// Show git blame for a file (who last changed each line)
    Blame { path: String },
    /// Show top authors by commit count and lines changed
    TopAuthors { limit: usize },
    /// Show most frequently changed files (hotspots)
    ChangeHotspots { limit: usize },
    /// Show files that frequently change together with a given file
    CoChange { path: String, limit: usize },
    /// Show git history statistics for the indexed repo
    GitStats,
    /// Combined symbol + git intelligence: who owns this symbol, when was it last changed,
    /// how many authors touched it, what else changes with it
    SymbolOwnership { symbol: String },
    /// Risk score for changing a file: combines structural impact (call graph) with
    /// git hotspot frequency and author count. More actionable than either alone.
    ChangeRisk { path: String },
    /// Find experts for a file/symbol: ranks authors by recency + volume of changes
    /// to the specific symbols in the file, not just file-level blame
    FindExperts { path: String },
    /// Integrated co-change: combines static call-graph coupling with git co-change
    /// history. More reliable than either signal alone.
    SmartCoChange { symbol: String, limit: usize },
}

#[derive(Debug)]
struct GroupScanArgs {
    repos: Vec<(String, PathBuf)>,
    db_path: PathBuf,
    threads: usize,
    semantic: bool,
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
    db_path: Option<PathBuf>,
    incremental: bool,
    embeddings: bool,
    graph_phases: bool,
    query: Option<QueryCommand>,
    group: Option<GroupScanArgs>,
    /// When true, start the MCP server instead of scanning/querying.
    mcp_server: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            start: None,
            goal: None,
            max_depth: 10,
            max_nodes: 10000,
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
            db_path: None,
            incremental: false,
            embeddings: false,
            graph_phases: false,
            query: None,
            group: None,
            mcp_server: false,
        }
    }
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let cli_args: Vec<String> = std::env::args().collect();

    fn help() -> &'static str {
        "\nRun 'atree --help' for usage information."
    }

    fn take_value<'a>(cli_args: &'a [String], i: usize) -> &'a str {
        cli_args.get(i + 1).map(|s| s.as_str()).unwrap_or_else(|| {
            eprintln!("Error: flag '{}' requires a value{}", cli_args[i], help());
            std::process::exit(2);
        })
    }

    fn parse_usize(flag: &str, raw: &str) -> usize {
        raw.parse().unwrap_or_else(|_| {
            eprintln!(
                "Error: flag '{}' expected a non-negative integer, got '{}'{}",
                flag, raw, help()
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
                        "Error: --threads expected 'all', 'auto', or a number, got '{}'{}",
                        raw, help()
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
                let raw = take_value(&cli_args, i);
                args.root = std::path::PathBuf::from(raw);
                // Canonicalize to prevent path traversal and ensure consistent behavior.
                if let Ok(canonical) = args.root.canonicalize() {
                    args.root = canonical;
                } else {
                    log::warn!("Cannot canonicalize root path '{}': does it exist?", args.root.display());
                }
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
            "--db" => {
                args.db_path = Some(PathBuf::from(take_value(&cli_args, i)));
                i += 1;
            }
            "--incremental" => args.incremental = true,
            "--embeddings" => args.embeddings = true,
            "--graph-phases" => args.graph_phases = true,
            "--json" => args.json = true,
            "--print-schema" | "--schema" => args.print_schema = true,
            "group" => {
                i += 1;
                if i >= cli_args.len() {
                    eprintln!("Error: 'group' requires a subcommand: scan{}", help());
                    std::process::exit(2);
                }
                match cli_args[i].as_str() {
                    "scan" => {
                        let mut repos: Vec<(String, PathBuf)> = Vec::new();
                        let mut db_path = PathBuf::from(".atree/group.sqlite");
                        let mut semantic = true;
                        let mut threads = ThreadSpec::Auto;
                        i += 1;
                        while i < cli_args.len() {
                            match cli_args[i].as_str() {
                                "--repos" => {
                                    i += 1;
                                    while i < cli_args.len() && !cli_args[i].starts_with("--") {
                                        let parts: Vec<&str> = cli_args[i].splitn(2, ':').collect();
                                        if parts.len() == 2 {
                                            repos.push((parts[0].to_string(), PathBuf::from(parts[1])));
                                        } else {
                                            let path = PathBuf::from(cli_args[i].clone());
                                            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("repo").to_string();
                                            repos.push((name, path));
                                        }
                                        i += 1;
                                    }
                                }
                                "--db" => {
                                    db_path = PathBuf::from(take_value(&cli_args, i));
                                    i += 1;
                                }
                                "--threads" => {
                                    threads = parse_threads(take_value(&cli_args, i));
                                    i += 1;
                                }
                                "--no-semantic" => { semantic = false; }
                                _ => break,
                            }
                            i += 1;
                        }
                        if repos.is_empty() {
                            eprintln!("Error: 'group scan' requires at least one --repos name:path{}", help());
                            std::process::exit(2);
                        }
                        let resolved_threads = resolve_threads(&threads);
                        args.group = Some(GroupScanArgs { repos, db_path, threads: resolved_threads, semantic });
                    }
                    other => {
                        eprintln!("Error: unknown group subcommand '{}'. Valid: scan{}", other, help());
                        std::process::exit(2);
                    }
                }
            }
            "mcp-server" => {
                // Start MCP server for AI agent integration (used by Crush and other MCP hosts).
                // Requires --db <path> to specify which index to serve.
                args.mcp_server = true;
            }
            "query" => {
                // Parse subcommand: atree query <subcommand> [args] --db <path>
                i += 1;
                if i >= cli_args.len() {
                    eprintln!("Error: 'query' requires a subcommand. Run 'atree --help' for full list.{}", help());
                    std::process::exit(2);
                }
                match cli_args[i].as_str() {
                    "symbols" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query symbols' requires a name pattern{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::Symbols { name: cli_args[i].clone() });
                    }
                    "callers" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query callers' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        let symbol = cli_args[i].clone();
                        let depth = cli_args.get(i+1).and_then(|s| s.parse().ok()).unwrap_or(3);
                        args.query = Some(QueryCommand::Callers { symbol, depth });
                    }
                    "callees" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query callees' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        let symbol = cli_args[i].clone();
                        let depth = cli_args.get(i+1).and_then(|s| s.parse().ok()).unwrap_or(3);
                        args.query = Some(QueryCommand::Callees { symbol, depth });
                    }
                    "impact" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query impact' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        let symbol = cli_args[i].clone();
                        let depth = cli_args.get(i+1).and_then(|s| s.parse().ok()).unwrap_or(3);
                        args.query = Some(QueryCommand::Impact { symbol, depth });
                    }
                    "routes" => {
                        args.query = Some(QueryCommand::Routes);
                    }
                    "search" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query search' requires a query string{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::Search { query: cli_args[i].clone() });
                    }
                    "semantic-search" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query semantic-search' requires a query string{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::SemanticSearch { query: cli_args[i].clone() });
                    }
                    "stats" => {
                        args.query = Some(QueryCommand::Stats);
                    }
                    "repos" => {
                        args.query = Some(QueryCommand::Repos);
                    }
                    "context" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query context' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::Context { symbol: cli_args[i].clone() });
                    }
                    "detect-changes" => {
                        args.query = Some(QueryCommand::DetectChanges);
                    }
                    "rename" => {
                        i += 1;
                        if i + 1 >= cli_args.len() {
                            eprintln!("Error: 'query rename' requires <old_name> <new_name>{}", help());
                            std::process::exit(2);
                        }
                        let symbol_name = cli_args[i].clone();
                        i += 1;
                        let new_name = cli_args[i].clone();
                        let dry_run = !cli_args.get(i+1).map(|s| s == "--apply").unwrap_or(false);
                        args.query = Some(QueryCommand::Rename { symbol_name, new_name, dry_run });
                    }
                    "cypher" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query cypher' requires a SQL query{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::Cypher { query: cli_args[i].clone() });
                    }
                    "shape-check" => {
                        let route = cli_args.get(i+1).filter(|s| !s.starts_with("--")).cloned();
                        if route.is_some() { i += 1; }
                        args.query = Some(QueryCommand::ShapeCheck { route });
                    }
                    "tool-map" => {
                        args.query = Some(QueryCommand::ToolMap);
                    }
                    "api-impact" => {
                        let route = cli_args.get(i+1).filter(|s| !s.starts_with("--")).cloned();
                        if route.is_some() { i += 1; }
                        args.query = Some(QueryCommand::ApiImpact { route, file: None });
                    }
                    "verify" => {
                        let verify_type = cli_args.get(i+1).filter(|s| !s.starts_with("--")).cloned().unwrap_or_else(|| "all".to_string());
                        if !verify_type.starts_with("--") { i += 1; }
                        args.query = Some(QueryCommand::Verify { verify_type, command: None });
                    }
                    "group-sync" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query group-sync' requires a group name{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::GroupSync { name: cli_args[i].clone() });
                    }
                    "explain" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query explain' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::ExplainSymbol { symbol: cli_args[i].clone() });
                    }
                    "entrypoints" => {
                        args.query = Some(QueryCommand::FindEntrypoints);
                    }
                    "trace-path" => {
                        i += 1;
                        if i + 1 >= cli_args.len() {
                            eprintln!("Error: 'query trace-path' requires <from> <to>{}", help());
                            std::process::exit(2);
                        }
                        let from = cli_args[i].clone();
                        i += 1;
                        let to = cli_args[i].clone();
                        args.query = Some(QueryCommand::TraceCallPath { from, to });
                    }
                    "public-api" => {
                        let module = cli_args.get(i+1).filter(|s| !s.starts_with("--")).cloned();
                        if module.is_some() { i += 1; }
                        args.query = Some(QueryCommand::PublicApiSurface { module });
                    }
                    "affected-tests" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query affected-tests' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::AffectedTests { symbol: cli_args[i].clone() });
                    }
                    "validation-plan" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query validation-plan' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::ValidationPlan { symbol: cli_args[i].clone() });
                    }
                    "contract-changes" => {
                        let base_ref = cli_args.get(i+1).filter(|s| !s.starts_with("--")).cloned();
                        if base_ref.is_some() { i += 1; }
                        args.query = Some(QueryCommand::ContractChangeDetector { base_ref });
                    }
                    "boundary-check" => {
                        args.query = Some(QueryCommand::ArchitectureBoundaryCheck);
                    }
                    "scope-violations" => {
                        args.query = Some(QueryCommand::ScopeViolationDetector);
                    }
                    "config-map" => {
                        args.query = Some(QueryCommand::ConfigSurfaceMap);
                    }
                    "impact-by-kind" => {
                        i += 1;
                        if i + 1 >= cli_args.len() {
                            eprintln!("Error: 'query impact-by-kind' requires <target> <kind> [direction]{}", help());
                            std::process::exit(2);
                        }
                        let target = cli_args[i].clone();
                        i += 1;
                        let kind = cli_args[i].clone();
                        let direction = cli_args.get(i+1).filter(|s| !s.starts_with("--")).cloned().unwrap_or_else(|| "upstream".to_string());
                        if !direction.starts_with("--") { i += 1; }
                        args.query = Some(QueryCommand::ImpactBySymbolKind { target, kind, direction });
                    }
                    "semantic-diff" => {
                        let base_ref = cli_args.get(i+1).filter(|s| !s.starts_with("--")).cloned();
                        if base_ref.is_some() { i += 1; }
                        args.query = Some(QueryCommand::SemanticDiffSummary { base_ref });
                    }
                    "side-effects" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query side-effects' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::SideEffectScanner { symbol: cli_args[i].clone() });
                    }
                    "change-coupling" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query change-coupling' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::ChangeCoupling { symbol: cli_args[i].clone() });
                    }
                    "concurrency" => {
                        let symbol = cli_args.get(i+1).cloned().unwrap_or_default();
                        if !symbol.is_empty() && !symbol.starts_with("--") { i += 1; }
                        args.query = Some(QueryCommand::ConcurrencySurfaceDetector { symbol });
                    }
                    "edit-scope" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query edit-scope' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::MinimalEditScope { symbol: cli_args[i].clone() });
                    }
                    "issue-locator" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query issue-locator' requires an issue description{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::IssueToCodeLocator { issue: cli_args[i].clone() });
                    }
                    "docs-drift" => {
                        args.query = Some(QueryCommand::DocsDriftDetector);
                    }
                    "rename-safety" => {
                        i += 1;
                        if i + 1 >= cli_args.len() {
                            eprintln!("Error: 'query rename-safety' requires <old_name> <new_name>{}", help());
                            std::process::exit(2);
                        }
                        let symbol_name = cli_args[i].clone();
                        i += 1;
                        let new_name = cli_args[i].clone();
                        args.query = Some(QueryCommand::RenameSafetyCheck { symbol_name, new_name });
                    }
                    "dead-code" => {
                        args.query = Some(QueryCommand::DeadCodeCandidates);
                    }
                    "hotspots" => {
                        args.query = Some(QueryCommand::OwnershipHotspots);
                    }
                    "error-trace" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query error-trace' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::ErrorPathTrace { symbol: cli_args[i].clone() });
                    }
                    "resource-lifecycle" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query resource-lifecycle' requires a resource name{}", help());
                            std::process::exit(2);
                        }
                        args.query = Some(QueryCommand::ResourceLifecycleMap { resource: cli_args[i].clone() });
                    }
                    "dep-cycles" => {
                        args.query = Some(QueryCommand::DependencyCycleDetector);
                    }
                    "uncovered" => {
                        args.query = Some(QueryCommand::FindUncoveredSymbols);
                    }
                    "resolution-stats" => {
                        args.query = Some(QueryCommand::ResolutionStats);
                    }
                    "evidence-path" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query evidence-path' requires a query string{}", help());
                            std::process::exit(2);
                        }
                        let query = cli_args[i].clone();
                        let max_depth = cli_args.get(i+1).filter(|s| !s.starts_with("--")).and_then(|s| s.parse().ok()).unwrap_or(5);
                        if !cli_args.get(i+1).map_or(true, |s| s.starts_with("--")) { i += 1; }
                        let beam_width = cli_args.get(i+1).filter(|s| !s.starts_with("--")).and_then(|s| s.parse().ok()).unwrap_or(5);
                        if !cli_args.get(i+1).map_or(true, |s| s.starts_with("--")) { i += 1; }
                        let max_evidence = cli_args.get(i+1).filter(|s| !s.starts_with("--")).and_then(|s| s.parse().ok()).unwrap_or(10);
                        if !cli_args.get(i+1).map_or(true, |s| s.starts_with("--")) { i += 1; }
                        args.query = Some(QueryCommand::EvidencePath { query, max_depth, beam_width, max_evidence });
                    }
                    "file-history" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query file-history' requires a file path{}", help());
                            std::process::exit(2);
                        }
                        let path = cli_args[i].clone();
                        let limit = cli_args.get(i+1).filter(|s| !s.starts_with("--")).and_then(|s| s.parse().ok()).unwrap_or(20);
                        if limit != 20 { i += 1; }
                        args.query = Some(QueryCommand::FileHistory { path, limit });
                    }
                    "git-blame" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query git-blame' requires a file path{}", help());
                            std::process::exit(2);
                        }
                        let path = cli_args[i].clone();
                        args.query = Some(QueryCommand::Blame { path });
                    }
                    "top-authors" => {
                        let limit = cli_args.get(i+1).filter(|s| !s.starts_with("--")).and_then(|s| s.parse().ok()).unwrap_or(10);
                        if limit != 10 { i += 1; }
                        args.query = Some(QueryCommand::TopAuthors { limit });
                    }
                    "change-hotspots" => {
                        let limit = cli_args.get(i+1).filter(|s| !s.starts_with("--")).and_then(|s| s.parse().ok()).unwrap_or(20);
                        if limit != 20 { i += 1; }
                        args.query = Some(QueryCommand::ChangeHotspots { limit });
                    }
                    "co-change" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query co-change' requires a file path{}", help());
                            std::process::exit(2);
                        }
                        let path = cli_args[i].clone();
                        let limit = cli_args.get(i+1).filter(|s| !s.starts_with("--")).and_then(|s| s.parse().ok()).unwrap_or(10);
                        if limit != 10 { i += 1; }
                        args.query = Some(QueryCommand::CoChange { path, limit });
                    }
                    "git-stats" => {
                        args.query = Some(QueryCommand::GitStats);
                    }
                    "symbol-ownership" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query symbol-ownership' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        let symbol = cli_args[i].clone();
                        args.query = Some(QueryCommand::SymbolOwnership { symbol });
                    }
                    "change-risk" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query change-risk' requires a file path{}", help());
                            std::process::exit(2);
                        }
                        let path = cli_args[i].clone();
                        args.query = Some(QueryCommand::ChangeRisk { path });
                    }
                    "find-experts" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query find-experts' requires a file path or symbol name{}", help());
                            std::process::exit(2);
                        }
                        let path = cli_args[i].clone();
                        args.query = Some(QueryCommand::FindExperts { path });
                    }
                    "smart-co-change" => {
                        i += 1;
                        if i >= cli_args.len() {
                            eprintln!("Error: 'query smart-co-change' requires a symbol name{}", help());
                            std::process::exit(2);
                        }
                        let symbol = cli_args[i].clone();
                        let limit = cli_args.get(i+1).filter(|s| !s.starts_with("--")).and_then(|s| s.parse().ok()).unwrap_or(10);
                        if limit != 10 { i += 1; }
                        args.query = Some(QueryCommand::SmartCoChange { symbol, limit });
                    }
                    other => {
                        eprintln!("Error: unknown query subcommand '{}'. Run 'atree query --help' for full list.", other);
                        std::process::exit(2);
                    }
                }
                i += 1;
                continue;
            }
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
                eprintln!("Error: unknown argument '{}'. Run 'atree --help' for usage information.", other);
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
      --db <PATH>            Path to SQLite index file (default: in-memory)
      --incremental          Only re-index changed files (requires --db)
      query <CMD>            Query an existing index (requires --db):
                             query symbols <name>    Search symbols by name
                             query callers <sym> [depth]  Show callers
                             query callees <sym> [depth]  Show callees
                             query impact <sym> [depth]   Impact analysis
                             query routes            List API routes
                             query search <text>     Full-text search
                             query stats             Index statistics
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

/// Execute a query command against the code index.
fn execute_query(cmd: &QueryCommand, args: &Args, _scan: Option<&atree_engine::ScanResult>) -> ! {
    use atree_engine::store::GraphStore;
    use atree_engine::search::{self, SearchConfig};

    // Open the graph store.  Priority: --db flag → .atree/index.sqlite in the
    // scanned root → .atree/index.sqlite in cwd.
    let db_path = args.db_path.clone().or_else(|| {
        let candidates = [
            args.root.join(".atree/index.sqlite"),
            PathBuf::from(".atree/index.sqlite"),
        ];
        candidates.iter().find(|p| p.exists()).cloned()
    });
    let store = match &db_path {
        Some(path) => GraphStore::open(path).unwrap_or_else(|e| {
            eprintln!("Error opening index at {}: {}\nRun 'atree --help' for usage information.", path.display(), e);
            std::process::exit(1);
        }),
        None => {
            eprintln!("Error: query commands require a code index.");
            eprintln!("       Run 'atree --semantic --db .atree/index.sqlite --root .' first to build the index.");
            eprintln!("       Or pass --db <PATH> to point to an existing index.");
            std::process::exit(1);
        }
    };

    match cmd {
        QueryCommand::Symbols { name } => {
            let symbols = store.get_symbols_by_name(name).unwrap_or_else(|e| {
                eprintln!("Error querying symbols: {}", e);
                std::process::exit(1);
            });
            if symbols.is_empty() {
                eprintln!("No symbols matching '{}' found.", name);
                std::process::exit(0);
            }
            println!("Found {} symbol(s) matching '{}':", symbols.len(), name);
            for sym in &symbols {
                let file = store.get_file_by_id(sym.file_id).unwrap_or(None);
                let file_path = file.as_ref().map(|f| f.path.as_str()).unwrap_or("?");
                println!("  {}  {}  {}:{}  [{}]",
                    sym.name,
                    sym.kind,
                    file_path,
                    sym.line,
                    if sym.is_exported { "exported" } else { "local" }
                );
            }
        }
        QueryCommand::Callers { symbol, depth } => {
            // Find the symbol first
            let syms = store.get_symbols_by_name(symbol).unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            });
            if syms.is_empty() {
                eprintln!("Symbol '{}' not found. Try 'query symbols {}' first.", symbol, symbol);
                std::process::exit(1);
            }
            for sym in &syms {
                let file = store.get_file_by_id(sym.file_id).unwrap_or(None);
                let fp = file.as_ref().map(|f| f.path.as_str()).unwrap_or("?");
                println!("Callers of {} ({}:{}) [depth={}]:",
                    sym.name, fp, sym.line, depth);
                let callers = store.get_callers(sym.id, *depth).unwrap_or_else(|e| {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                });
                if callers.is_empty() {
                    println!("  (no resolved callers)");
                }
                for (caller_id, caller_name, confidence, file_id) in &callers {
                    let file = store.get_file_by_id(*file_id).unwrap_or(None);
                    let file_path = file.as_ref().map(|f| f.path.as_str()).unwrap_or("?");
                    println!("  {}  {:.2}  {}:{}", caller_name, confidence, file_path, caller_id);
                }
                if syms.len() > 1 {
                    println!("  ---");
                }
            }
        }
        QueryCommand::Callees { symbol, depth } => {
            let syms = store.get_symbols_by_name(symbol).unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            });
            if syms.is_empty() {
                eprintln!("Symbol '{}' not found.", symbol);
                std::process::exit(1);
            }
            for sym in &syms {
                let file = store.get_file_by_id(sym.file_id).unwrap_or(None);
                let fp = file.as_ref().map(|f| f.path.as_str()).unwrap_or("?");
                println!("Callees of {} ({}:{}) [depth={}]:",
                    sym.name, fp, sym.line, depth);
                let callees = store.get_callees(sym.id, *depth).unwrap_or_else(|e| {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                });
                if callees.is_empty() {
                    println!("  (no resolved callees)");
                }
                for (callee_id, callee_name, confidence, file_id) in &callees {
                    let file = store.get_file_by_id(*file_id).unwrap_or(None);
                    let file_path = file.as_ref().map(|f| f.path.as_str()).unwrap_or("?");
                    println!("  {}  {:.2}  {}:{}", callee_name, confidence, file_path, callee_id);
                }
                if syms.len() > 1 {
                    println!("  ---");
                }
            }
        }
        QueryCommand::Impact { symbol, depth } => {
            let syms = store.get_symbols_by_name(symbol).unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            });
            if syms.is_empty() {
                eprintln!("Symbol '{}' not found.", symbol);
                std::process::exit(1);
            }
            for sym in &syms {
                let file = store.get_file_by_id(sym.file_id).unwrap_or(None);
                let fp = file.as_ref().map(|f| f.path.as_str()).unwrap_or("?");
                println!("Impact analysis for {} ({}:{}):", sym.name, fp, sym.line);
                println!("  Upstream (callers, depth={}):", depth);
                let callers = store.get_callers(sym.id, *depth).unwrap();
                if callers.is_empty() {
                    println!("    (none)");
                }
                for (_id, name, conf, _) in &callers {
                    println!("    {}  [{:.2}]", name, conf);
                }
                println!("  Downstream (callees, depth={}):", depth);
                let callees = store.get_callees(sym.id, *depth).unwrap();
                if callees.is_empty() {
                    println!("    (none)");
                }
                for (_id, name, conf, _) in &callees {
                    println!("    {}  [{:.2}]", name, conf);
                }
            }
        }
        QueryCommand::Routes => {
            // Query edges table for route-like patterns (ROUTE edges are persisted
            // during semantic scan by detect_and_persist_routes).
            let conn = store.conn();
            // First try the edges table (populated by semantic scan)
            let mut stmt = match conn.prepare(
                "SELECT s.name, s.file_path, s.line, e.edge_kind, e.confidence,
                        s2.name as handler_name, s2.file_path as handler_file, s2.line as handler_line
                 FROM edges e
                 JOIN symbols s ON s.id = e.src_id
                 LEFT JOIN symbols s2 ON s2.id = e.dst_id
                 WHERE e.edge_kind = 'ROUTE'
                 ORDER BY s.file_path, s.line"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let rows: Vec<_> = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,  // route name
                    row.get::<_, String>(1)?,  // file
                    row.get::<_, i64>(2)?,     // line
                    row.get::<_, String>(3)?,  // edge_kind
                    row.get::<_, f64>(4)?,     // confidence
                    row.get::<_, Option<String>>(5)?, // handler name
                    row.get::<_, Option<String>>(6)?, // handler file
                    row.get::<_, Option<i64>>(7)?,    // handler line
                ))
            }).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
            if rows.is_empty() {
                // Fallback: query symbols table for Route-kind symbols
                let mut stmt2 = match conn.prepare(
                    "SELECT s.name, s.file_path, s.line, 'ROUTE' as kind, 1.0
                     FROM symbols s
                     WHERE s.kind = 'Route'
                     ORDER BY s.file_path, s.line"
                ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
                let rows2: Vec<_> = stmt2.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, f64>(4)?,
                        Option::<String>::None,
                        Option::<String>::None,
                        Option::<i64>::None,
                    ))
                }).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

                if rows2.is_empty() {
                    println!("  (no routes found — run 'atree --semantic --db <path> --root <repo>' to detect routes)");
                } else {
                    println!("  {} route(s) found:", rows2.len());
                    for (name, file, line, kind, conf, _, _, _) in &rows2 {
                        println!("  {}  {}  {}:{}  [{:.2}]", kind, name, file, line, conf);
                    }
                }
            } else {
                println!("  {} route(s) found:", rows.len());
                for (name, file, line, kind, conf, hname, hfile, hline) in &rows {
                    let handler = match (hname, hfile, hline) {
                        (Some(hn), Some(hf), Some(hl)) => format!(" → {} ({}:{})", hn, hf, hl),
                        _ => String::new(),
                    };
                    println!("  {}  {}  {}:{}  [{:.2}]{}", kind, name, file, line, conf, handler);
                }
            }
        }
        QueryCommand::Search { query } => {
            let config = SearchConfig::default();
            let results = search::search(&store, query, &config).unwrap_or_else(|e| {
                eprintln!("Error searching: {}", e);
                std::process::exit(1);
            });
            if results.is_empty() {
                eprintln!("No results for '{}'", query);
                std::process::exit(0);
            }
            println!("Search results for '{}':", query);
            for hit in &results {
                println!("  {}  {}  {}:{}  [{:.3}]",
                    hit.name, hit.kind, hit.file_path, hit.line, hit.score);
            }
        }
        QueryCommand::SemanticSearch { query } => {
            let results = atree_engine::embeddings::semantic_search(&store, query, 10)
                .unwrap_or_else(|e| {
                    eprintln!("Error: {}. Make sure embeddings were generated (--embeddings flag).", e);
                    std::process::exit(1);
                });
            if results.is_empty() {
                eprintln!("No semantic results for '{}'", query);
                std::process::exit(0);
            }
            println!("Semantic search results for '{}':", query);
            for hit in &results {
                println!("  {}  {}  {}:{}  [{:.3}]",
                    hit.name, hit.kind, hit.file_path, hit.line, hit.similarity);
            }
        }
        QueryCommand::Stats => {
            let stats = store.stats().unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            });
            println!("Index statistics:");
            println!("  Files:      {}", stats.files);
            println!("  Symbols:    {}", stats.symbols);
            println!("  Scopes:     {}", stats.scopes);
            println!("  Imports:    {}", stats.imports);
            println!("  Calls:      {}", stats.calls);
            println!("  Edges:      {}", stats.edges);
            println!("  Resolved:   {} calls", stats.resolved_calls);
        }
        QueryCommand::Repos => {
            let repos = store.get_repos().unwrap_or_else(|e| {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            });
            if repos.is_empty() {
                println!("(no repos found — scan with --repo-label or use 'atree group scan')");
            } else {
                let repo_stats = store.get_repo_stats().unwrap();
                println!("Repos in index:");
                for (repo, files, symbols) in &repo_stats {
                    // Count public symbols per repo
                    let conn = store.conn();
                    let pub_count: i64 = conn.query_row(
                        "SELECT COUNT(*) FROM symbols s JOIN files f ON f.id = s.file_id
                         WHERE f.repo_label = ?1 AND s.is_exported = 1",
                        [repo],
                        |r| r.get(0),
                    ).unwrap_or(0);
                    println!("  {}  ({} files, {} symbols, {} public)", repo, files, symbols, pub_count);
                }
                println!("  {} repo(s) total", repos.len());
            }
        }
        // ── GitNexus-compatible tools ──────────────────────────────────
        QueryCommand::Context { symbol } => {
            let syms = store.get_symbols_by_name(symbol).unwrap_or_else(|e| {
                eprintln!("Error: {}", e); std::process::exit(1);
            });
            if syms.is_empty() {
                eprintln!("Symbol '{}' not found.", symbol);
                std::process::exit(1);
            }
            for sym in &syms {
                let file = store.get_file_by_id(sym.file_id).unwrap_or(None);
                let fp = file.as_ref().map(|f| f.path.as_str()).unwrap_or("?");
                println!("Symbol: {}", sym.name);
                println!("Kind: {}", sym.kind);
                println!("File: {}:{}", fp, sym.line);
                println!("Qualified: {}", sym.qualified_name);
                println!();
                let callers = store.get_callers(sym.id, 1).unwrap();
                println!("Callers:");
                if callers.is_empty() { println!("  (none)"); }
                for (_id, name, conf, _) in &callers {
                    println!("  {}  [{:.2}]", name, conf);
                }
                println!();
                let callees = store.get_callees(sym.id, 1).unwrap();
                println!("Callees:");
                if callees.is_empty() { println!("  (none)"); }
                for (_id, name, conf, _) in &callees {
                    println!("  {}  [{:.2}]", name, conf);
                }
                if syms.len() > 1 { println!("---"); }
            }
        }
        QueryCommand::DetectChanges => {
            let files = store.get_all_files().unwrap_or_else(|e| {
                eprintln!("Error: {}", e); std::process::exit(1);
            });
            if files.is_empty() {
                println!("No indexed files. Run 'atree --semantic --db <path> --root <repo>' first.");
                std::process::exit(0);
            }
            let repo_path = std::path::Path::new(&files[0].path).parent().map(|p| p.to_path_buf());
            match repo_path {
                Some(path) => {
                    let changed = atree_engine::detect_git_changes(&path);
                    match changed {
                        Some(c) if !c.is_empty() => {
                            println!("{} changed file(s):", c.len());
                            let conn = store.conn();
                            let mut affected_symbols = 0;
                            for f in &c {
                                println!("  - {}", f);
                                // Find indexed file record and its symbols
                                if let Some(ff) = files.iter().find(|ff| ff.path == *f) {
                                    let mut stmt = match conn.prepare("SELECT name, kind, line FROM symbols WHERE file_id = ?1") {
                                        Ok(s) => s,
                                        Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); }
                                    };
                                    let rows: Vec<_> = stmt.query_map([ff.id], |row| Ok((
                                        row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?,
                                    ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
                                    for row in &rows {
                                        let (n, k, l) = row;
                                        println!("      {} {}:{}", k, n, l);
                                        affected_symbols += 1;
                                    }
                                }
                            }
                            println!("\n  {} affected symbol(s) in {} changed file(s)", affected_symbols, c.len());
                            if affected_symbols > 0 {
                                println!("  Run 'atree query impact <symbol>' to see blast radius.");
                                println!("  Run 'atree query semantic-diff' for detailed diff.");
                            }
                        }
                        _ => println!("No uncommitted changes detected."),
                    }
                }
                None => println!("Could not determine repo path."),
            }
        }
        QueryCommand::Rename { symbol_name, new_name, dry_run } => {
            let syms = store.get_symbols_by_name(symbol_name).unwrap_or_else(|e| {
                eprintln!("Error: {}", e); std::process::exit(1);
            });
            if syms.is_empty() {
                eprintln!("Symbol '{}' not found.", symbol_name);
                std::process::exit(1);
            }
            let sym = &syms[0];
            let file = store.get_file_by_id(sym.file_id).unwrap_or(None);
            let fp = file.as_ref().map(|f| f.path.as_str()).unwrap_or("?");
            let _callers = store.get_callers(sym.id, 5).unwrap();

            // Collect all file positions that need to be renamed
            // (file_id, line, column, old_name) tuples
            let mut edits: Vec<(String, usize, usize, String)> = Vec::new();

            // Definition site
            edits.push((fp.to_string(), sym.line as usize, sym.col as usize, symbol_name.clone()));

            // Call sites — use the calls table for precise positions
            let conn = store.conn();
            let mut stmt = match conn.prepare(
                "SELECT f.path, c.line, c.col FROM calls c
                 JOIN files f ON f.id = c.file_id
                 WHERE c.callee_name = ?1 OR c.resolved_symbol_id = ?2
                 ORDER BY f.path, c.line, c.col"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let call_sites: Vec<_> = stmt.query_map([symbol_name, &sym.id.to_string()], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, i64>(2)?, row.get::<_, i64>(3)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            for (call_file, call_line, call_col) in &call_sites {
                edits.push((call_file.clone(), *call_line as usize, *call_col as usize, symbol_name.clone()));
            }

            // Deduplicate edits by (file, line, col)
            let mut seen = std::collections::HashSet::new();
            edits.retain(|(f, l, c, _)| seen.insert((f.clone(), *l, *c)));

            println!("Rename: {} → {}", symbol_name, new_name);
            println!("Kind: {}  {}:{}", sym.kind, fp, sym.line);
            println!("Dry run: {}", dry_run);
 println!();
            println!("{} location(s) to update:", edits.len());
            for (efile, eline, ecol, _) in &edits {
                println!("  {}:{}:{}", efile, eline, ecol);
            }

            if *dry_run {
                println!("\nDry run — no files modified. Use --apply to execute.");
                println!("  {} files would be modified", edits.len());
            } else {
                // Actually perform the rename
                let mut modified_files = std::collections::HashSet::new();
                let mut errors = Vec::new();

                // Group edits by file
                let mut file_edits: std::collections::HashMap<String, Vec<(usize, usize, String)>> = std::collections::HashMap::new();
                for (f, l, c, old) in &edits {
                    file_edits.entry(f.clone()).or_default().push((*l, *c, old.clone()));
                }

                for (file_path, file_edits_list) in &file_edits {
                    // Read file content
                    let content = match std::fs::read_to_string(file_path) {
                        Ok(c) => c,
                        Err(e) => { errors.push(format!("Cannot read {}: {}", file_path, e)); continue; }
                    };

                    let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();

                    // Sort edits by line DESC, col DESC so we can apply from bottom-right
                    // to avoid position shifts
                    let mut sorted_edits = file_edits_list.clone();
                    sorted_edits.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));

                    for (line, col, old_name) in &sorted_edits {
                        let line_idx = line - 1; // 1-based to 0-based
                        if line_idx >= lines.len() {
                            errors.push(format!("{}:{} — line out of range", file_path, line));
                            continue;
                        }
                        let line_content = &lines[line_idx];
                        // Find the old_name at or after the given column
                        let search_start = if *col > 0 { col - 1 } else { 0 };
                        if let Some(pos) = line_content[search_start..].find(old_name) {
                            let actual_col = search_start + pos;
                            let new_line = format!("{}{}{}",
                                &line_content[..actual_col],
                                new_name,
                                &line_content[actual_col + old_name.len()..]
                            );
                            lines[line_idx] = new_line;
                        } else {
                            // Fallback: replace first occurrence on the line
                            if let Some(pos) = line_content.find(old_name) {
                                let new_line = format!("{}{}{}",
                                    &line_content[..pos],
                                    new_name,
                                    &line_content[pos + old_name.len()..]
                                );
                                lines[line_idx] = new_line;
                            } else {
                                errors.push(format!("{}:{}:{} — '{}' not found on line", file_path, line, col, old_name));
                            }
                        }
                    }

                    // Write back
                    let new_content = lines.join("\n");
                    // Preserve trailing newline if original had one
                    let new_content = if content.ends_with('\n') {
                        format!("{}\n", new_content)
                    } else {
                        new_content
                    };

                    if let Err(e) = std::fs::write(file_path, new_content) {
                        errors.push(format!("Cannot write {}: {}", file_path, e));
                    } else {
                        modified_files.insert(file_path.clone());
                    }
                }

                println!();
                if errors.is_empty() {
                    println!("✅ Successfully renamed '{}' → '{}' in {} file(s).", symbol_name, new_name, modified_files.len());
                    for f in &modified_files {
                        println!("  modified: {}", f);
                    }
                } else {
                    println!("⚠️  Rename completed with {} error(s):", errors.len());
                    for e in &errors {
                        println!("  ERROR: {}", e);
                    }
                    if !modified_files.is_empty() {
                        println!("  {} file(s) were modified:", modified_files.len());
                        for f in &modified_files {
                            println!("    modified: {}", f);
                        }
                    }
                    std::process::exit(1);
                }
            }
        }
        QueryCommand::Cypher { query } => {
            let conn = store.conn();
            // Validate against allowlist to prevent SQL injection.
            let trimmed = query.trim().to_lowercase();
            if !trimmed.starts_with("select") && !trimmed.starts_with("with") {
                log::error!("Error: Only SELECT queries are allowed");
                std::process::exit(1);
            }
            // Block dangerous patterns.
            let blocked = ["sqlite_master", "sqlite_temp_master", "pg_catalog", "information_schema",
                "pragma", ";", "--", "/*", "*/", "insert", "update", "delete", "drop", "alter",
                "create", "attach", "detach", "replace"];
            for pat in &blocked {
                if trimmed.contains(pat) {
                    log::error!("Error: Query contains forbidden pattern: '{}'", pat);
                    std::process::exit(1);
                }
            }
            let mut stmt = match conn.prepare(query) {
                Ok(s) => s,
                Err(e) => { eprintln!("Query error: {}", e); std::process::exit(1); }
            };
            let col_count = stmt.column_count();
            let col_names: Vec<String> = (0..col_count).map(|i| stmt.column_name(i).unwrap_or("?").to_string()).collect();
            println!("| {} |", col_names.join(" | "));
            println!("|{}|", col_names.iter().map(|_| "---").collect::<Vec<_>>().join("|"));
            let rows: Vec<Vec<String>> = match stmt.query_map([], |row| {
                let mut vals = Vec::new();
                for i in 0..col_count { vals.push(format!("{:?}", row.get::<_, rusqlite::types::Value>(i)?)); }
                Ok(vals)
            }) {
                Ok(r) => r.collect::<Result<Vec<_>, _>>().unwrap_or_default(),
                Err(e) => { eprintln!("Error: {}", e); std::process::exit(1); }
            };
            let mut count = 0;
            for row in &rows { println!("| {} |", row.join(" | ")); count += 1; }
            println!("\n{} row(s)", count);
        }
        QueryCommand::ShapeCheck { route } => {
            // Shape check: find what symbols a route handler calls (its response shape proxies)
            // and what callers depend on it (downstream shape consumers).
            let conn = store.conn();
            let target = route.as_deref().unwrap_or("");
            if target.is_empty() {
                // Show all route handlers and their callee counts
                let mut stmt = match conn.prepare(
                    "SELECT s.name, s.file_path, s.line,
                            COUNT(DISTINCT c.id) as callee_count,
                            COUNT(DISTINCT e_dst.id) as caller_count
                     FROM symbols s
                     LEFT JOIN calls c ON c.caller_scope_id = s.id
                     LEFT JOIN edges e_src ON e_src.src_id = s.id AND e_src.edge_kind = 'ROUTE'
                     LEFT JOIN edges e_dst ON e_dst.dst_id = s.id
                     WHERE s.kind IN ('Function', 'Method')
                     AND (s.id IN (SELECT dst_id FROM edges WHERE edge_kind = 'ROUTE')
                          OR s.name LIKE '%handler%' OR s.name LIKE '%Controller%')
                     GROUP BY s.id
                     ORDER BY caller_count DESC, callee_count DESC
                     LIMIT 50"
                ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
                let rows: Vec<_> = stmt.query_map([], |row| Ok((
                    row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?, row.get::<_, i64>(3)?, row.get::<_, i64>(4)?,
                ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
                if rows.is_empty() {
                    println!("  No route handlers found.");
                } else {
                    println!("  {} route handler(s):", rows.len());
                    for (name, file, line, callees, callers) in &rows {
                        println!("  {:<30}  callees: {:>3}  callers: {:>3}  {}:{}", name, callees, callers, file, line);
                    }
                }
            }
        }
        QueryCommand::ToolMap => {
            let conn = store.conn();
            // Solid: query edges table for actual tool/route/handler edges,
            // plus symbols that define tools via known patterns.
            let mut stmt = match conn.prepare(
                "SELECT DISTINCT s.name, s.kind, s.file_path, s.line, e.edge_kind
                 FROM edges e
                 JOIN symbols s ON s.id = e.dst_id
                 WHERE e.edge_kind IN ('HANDLES_ROUTE', 'ROUTE', 'HANDLES_TOOL', 'HANDLES_ENDPOINT')
                 ORDER BY s.file_path, s.line"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let edge_rows: Vec<_> = stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
            // Also find symbols that are tools/handlers/endpoints by their role in the call graph:
            // symbols that are called by route handlers or that call into I/O layers
            let mut stmt2 = match conn.prepare(
                "SELECT DISTINCT s.name, s.kind, s.file_path, s.line, 'CALLS_IO' as role
                 FROM symbols s
                 JOIN calls c ON c.caller_scope_id = s.id
                 WHERE s.kind IN ('Function', 'Method')
                 AND c.callee_name IN (
                     'get', 'post', 'put', 'delete', 'patch', 'head', 'options',
                     'send', 'recv', 'connect', 'listen', 'accept',
                     'read', 'write', 'open', 'close',
                     'execute', 'query', 'invoke', 'call', 'dispatch'
                 )
                 AND s.id NOT IN (
                     SELECT DISTINCT dst_id FROM edges
                     WHERE edge_kind IN ('HANDLES_ROUTE', 'ROUTE', 'HANDLES_TOOL')
                 )
                 ORDER BY s.file_path, s.line LIMIT 50"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let io_rows: Vec<_> = stmt2.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            if edge_rows.is_empty() && io_rows.is_empty() {
                println!("  No tool/handler/endpoint symbols found.");
                println!("  Run 'atree --semantic --db <path> --root <repo>' to populate the graph.");
            } else {
                if !edge_rows.is_empty() {
                    println!("── Route/Tool Handlers (from graph edges) ──");
                    for (name, kind, file, line, edge_kind) in &edge_rows {
                        println!("  {:<12}  {}  {}  {}:{}", edge_kind, kind, name, file, line);
                    }
                }
                if !io_rows.is_empty() {
                    println!("── I/O Dispatch Symbols (from call graph) ──");
                    for (name, kind, file, line, role) in &io_rows {
                        println!("  {:<12}  {}  {}  {}:{}", role, kind, name, file, line);
                    }
                }
                println!();
                println!("  {} edge-based + {} call-graph-based = {} total",
                    edge_rows.len(), io_rows.len(), edge_rows.len() + io_rows.len());
            }
        }
        QueryCommand::ApiImpact { route, file } => {
            let name = route.as_deref().or(file.as_deref()).unwrap_or("");
            if name.is_empty() {
                eprintln!("Error: Need 'route' or 'file' parameter");
                std::process::exit(1);
            }
            let syms = store.get_symbols_by_name(name).unwrap_or_else(|e| {
                eprintln!("Error: {}", e); std::process::exit(1);
            });
            if syms.is_empty() {
                println!("Route handler '{}' not found.", name);
                std::process::exit(0);
            }
            let sym = &syms[0];
            let file_rec = store.get_file_by_id(sym.file_id).unwrap_or(None);
            let fp = file_rec.as_ref().map(|f| f.path.as_str()).unwrap_or("?");

            // Full upstream (callers at multiple depths)
            let direct_callers = store.get_callers(sym.id, 1).unwrap();
            let depth2_callers = store.get_callers(sym.id, 2).unwrap();
            let depth3_callers = store.get_callers(sym.id, 3).unwrap();

            // Downstream: what this API calls internally
            let direct_callees = store.get_callees(sym.id, 1).unwrap();
            let depth2_callees = store.get_callees(sym.id, 2).unwrap();

            // Detect if this is a known route handler
            let conn = store.conn();
            let route_info: Option<String> = conn.query_row(
                "SELECT s2.name FROM edges e
                 JOIN symbols s2 ON s2.id = e.src_id
                 WHERE e.dst_id = ?1 AND e.edge_kind IN ('ROUTE', 'HANDLES_ROUTE')
                 LIMIT 1",
                [sym.id],
                |r| r.get(0),
            ).ok();

            // Risk: weight direct callers more heavily
            let weighted_score = direct_callers.len() * 3 + (depth2_callers.len() - direct_callers.len()) * 2
                + (depth3_callers.len() - depth2_callers.len());
            let risk = match weighted_score {
                0 => "LOW (no consumers)",
                1..=5 => "LOW",
                6..=15 => "MEDIUM",
                16..=40 => "HIGH",
                _ => "CRITICAL",
            };

            println!("═══ API Impact: {} ═══", name);
            println!("  {}  |  {}:{}", sym.kind, fp, sym.line);
            if let Some(ref route) = route_info {
                println!("  Route: {}", route);
            }
            println!("  Risk: {} (weighted score: {})", risk, weighted_score);
            println!();

            // Consumer breakdown by depth
            println!("── Consumers (upstream) ──");
            println!("  Direct callers (depth 1): {}", direct_callers.len());
            for (_id, cname, conf, fid) in &direct_callers {
                let f = store.get_file_by_id(*fid).unwrap_or(None);
                let fpath = f.as_ref().map(|ff| ff.path.as_str()).unwrap_or("?");
                println!("    ← {} [{:.2}]  {}", cname, conf, fpath);
            }
            let indirect_d2 = depth2_callers.len() - direct_callers.len();
            if indirect_d2 > 0 {
                println!("  Indirect callers (depth 2): {}", indirect_d2);
                for (id, cname, conf, fid) in &depth2_callers {
                    if !direct_callers.iter().any(|(dcid, _, _, _)| dcid == id) {
                        let f = store.get_file_by_id(*fid).unwrap_or(None);
                        let fpath = f.as_ref().map(|ff| ff.path.as_str()).unwrap_or("?");
                        println!("    ← {} [{:.2}]  {} (indirect)", cname, conf, fpath);
                    }
                }
            }

            // Internal dependencies (downstream)
            println!();
            println!("── Internal Dependencies (downstream) ──");
            println!("  Direct callees: {}", direct_callees.len());
            for (_id, cname, conf, fid) in &direct_callees {
                let f = store.get_file_by_id(*fid).unwrap_or(None);
                let fpath = f.as_ref().map(|ff| ff.path.as_str()).unwrap_or("?");
                println!("    → {} [{:.2}]  {}", cname, conf, fpath);
            }
            let indirect_c2 = depth2_callees.len() - direct_callees.len();
            if indirect_c2 > 0 {
                println!("  Transitive dependencies (depth 2): {}", indirect_c2);
            }

            // Blast radius summary
            let total_unique_callers = depth3_callers.len();
            let total_unique_callees = depth2_callees.len();
            println!();
            println!("── Blast Radius Summary ──");
            println!("  Upstream reach:  {} symbols (3-hop)", total_unique_callers);
            println!("  Downstream reach: {} symbols (2-hop)", total_unique_callees);
            println!("  Total affected:  {} symbols", total_unique_callers + total_unique_callees + 1);
        }
        QueryCommand::Verify { verify_type, command } => {
            // Strict allowlist: only predefined cargo subcommands are permitted.
            // Custom --command is rejected to prevent shell injection.
            let cmd_str = match verify_type.as_str() {
                "test" => "cargo test",
                "lint" => "cargo clippy",
                "typecheck" => "cargo check",
                _ => {
                    log::error!("Unknown verify type: '{}'. Allowed: test, lint, typecheck", verify_type);
                    std::process::exit(1);
                }
            };
            if command.is_some() {
                log::error!("Custom --command is not allowed for security. Use --type test|lint|typecheck");
                std::process::exit(1);
            }
            let output = std::process::Command::new("sh").arg("-c").arg(cmd_str).output()
                .unwrap_or_else(|e| { log::error!("Failed to run '{}': {}", cmd_str, e); std::process::exit(1); });
            println!("Verify ({}): {}", verify_type, if output.status.success() { "PASSED" } else { "FAILED" });
            print!("{}", String::from_utf8_lossy(&output.stdout));
            eprint!("{}", String::from_utf8_lossy(&output.stderr));
        }
        QueryCommand::GroupSync { name } => {
            // Rebuild cross-repo contract links: for each repo's exported symbols,
            // find matching imports in other repos within the group.
            println!("Group sync for '{}' — rebuilding cross-repo contract links...", name);
            let conn = store.conn();
            let repos = store.get_repos().unwrap();
            if repos.is_empty() {
                println!("  No repos found. Run 'atree group scan --repos name:path ...' first.");
                std::process::exit(0);
            }
            if repos.len() < 2 {
                println!("  Only 1 repo '{}'. Cross-repo linking requires 2+ repos.", repos[0]);
                std::process::exit(0);
            }

            // Get all exported symbols across all repos
            let mut stmt = match conn.prepare(
                "SELECT e.exported_name, s.file_path, s.name, s.kind, f.repo_label
                 FROM exports e
                 JOIN symbols s ON s.id = e.symbol_id
                 JOIN files f ON f.id = s.file_id
                 ORDER BY f.repo_label, e.exported_name"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let exports: Vec<(String, String, String, String, String)> = stmt.query_map([], |row| Ok((
                row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // Get all imports across all repos
            let mut stmt2 = match conn.prepare(
                "SELECT i.imported_name, i.source, i.local_name, i.resolved_file_id, f.repo_label
                 FROM imports i
                 JOIN files f ON f.id = i.file_id
                 ORDER BY f.repo_label, i.imported_name"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let imports: Vec<(String, String, String, Option<i64>, String)> = stmt2.query_map([], |row| Ok((
                row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // Match imports to exports across repos
            let mut cross_links = 0;
            let mut unresolved_imports = 0;
            for (imp_name, _source, _local, resolved, imp_repo) in &imports {
                let matching_exports: Vec<&(String, String, String, String, String)> = exports.iter()
                    .filter(|(exp_name, _, _, _, exp_repo)| exp_name == imp_name && exp_repo != imp_repo)
                    .collect();
                if matching_exports.is_empty() {
                    if resolved.is_none() {
                        unresolved_imports += 1;
                    }
                } else {
                    for (_exp_name, exp_file, exp_sym, exp_kind, exp_repo) in matching_exports {
                        // Create a cross-repo edge
                        let src_file_id = store.get_file(exp_file).unwrap_or(None).map(|f| f.id).unwrap_or(0);
                        if src_file_id > 0 {
                            let _ = store.insert_edge(&atree_engine::store::EdgeRecord {
                                id: 0,
                                src_id: src_file_id,
                                dst_id: src_file_id, // self-referencing for now; ideally we'd have both IDs
                                edge_kind: "CROSS_REPO_DEP".to_string(),
                                confidence: 0.8,
                                file_id: Some(src_file_id),
                                line: 0,
                            });
                        }
                        println!("  {}:{} → {}:{} ({})", imp_repo, imp_name, exp_repo, exp_sym, exp_kind);
                        cross_links += 1;
                    }
                }
            }

            println!();
            println!("  {} repos, {} exports, {} imports", repos.len(), exports.len(), imports.len());
            println!("  {} cross-repo links found", cross_links);
            if unresolved_imports > 0 {
                println!("  {} unresolved imports (no matching export in group)", unresolved_imports);
            }
        }
        // ── Advanced tools (25 new) ────────────────────────────────────
        QueryCommand::ExplainSymbol { symbol } => {
            let syms = store.get_symbols_by_name(symbol).unwrap_or_else(|e| {
                eprintln!("Error: {}", e); std::process::exit(1);
            });
            if syms.is_empty() { eprintln!("Symbol '{}' not found.", symbol); std::process::exit(1); }
            let sym = &syms[0];
            let file = store.get_file_by_id(sym.file_id).unwrap_or(None);
            let fp = file.as_ref().map(|f| f.path.as_str()).unwrap_or("?");
            let callers = store.get_callers(sym.id, 3).unwrap();
            let callees = store.get_callees(sym.id, 3).unwrap();
            let heritage = store.get_heritage_by_child(sym.id).unwrap();
            println!("═══ {} ═══", sym.name);
            println!("Kind: {}  |  File: {}:{}  |  Qualified: {}", sym.kind, fp, sym.line, sym.qualified_name);
            if sym.is_exported { println!("Visibility: exported"); }
            println!();
            println!("Role: {} '{}' in {}", sym.kind, sym.name, fp);
            if !callers.is_empty() {
                println!("Called by ({}):", callers.len());
                for (_id, n, conf, _) in &callers { println!("  ← {} [{:.2}]", n, conf); }
            }
            if !callees.is_empty() {
                println!("Calls ({}):", callees.len());
                for (_id, n, conf, _) in &callees { println!("  → {} [{:.2}]", n, conf); }
            }
            if !heritage.is_empty() {
                println!("Inheritance:");
                for h in &heritage { println!("  {} {} [{:.2}]", h.heritage_kind, h.parent_name, h.confidence); }
            }
            let total_refs = callers.len() + callees.len();
            println!();
            println!("Summary: {} has {} outgoing and {} incoming references (total: {}).",
                sym.name, callees.len(), callers.len(), total_refs);
        }
        QueryCommand::FindEntrypoints => {
            let conn = store.conn();
            let mut stmt = match conn.prepare(
                "SELECT s.name, s.kind, s.file_path, s.line FROM symbols s
                 WHERE s.is_exported = 1
                 AND s.kind IN ('Function', 'Method', 'Class')
                 ORDER BY s.file_path, s.line LIMIT 100"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let rows = stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
            println!("Entry points (exported symbols):");
            let mut count = 0;
            for row in &rows { let (n, k, f, l) = row; println!("  {}  {}  {}:{}", n, k, f, l); count += 1; }
            println!("  {} entry point(s)", count);
        }
        QueryCommand::TraceCallPath { from, to } => {
            let from_syms = store.get_symbols_by_name(from).unwrap();
            let to_syms = store.get_symbols_by_name(to).unwrap();
            if from_syms.is_empty() { eprintln!("'{}' not found.", from); std::process::exit(1); }
            if to_syms.is_empty() { eprintln!("'{}' not found.", to); std::process::exit(1); }
            let to_id = to_syms[0].id;
            let to_name = &to_syms[0].name;

            // BFS through the call graph to find the shortest path.
            // Each queue entry: (symbol_id, path_vec_of_(name, confidence))
            let mut visited = std::collections::HashSet::new();
            let mut queue: Vec<(i64, Vec<(String, f64)>)> = Vec::new();
            queue.push((from_syms[0].id, vec![(from.clone(), 1.0)]));
            visited.insert(from_syms[0].id);

            let mut found_path: Option<Vec<(String, f64)>> = None;
            let mut all_paths: Vec<Vec<(String, f64)>> = Vec::new();
            let max_depth = 6;

            while let Some((cur_id, path)) = queue.pop() {
                if path.len() > max_depth { continue; }
                let callees = store.get_callees(cur_id, 1).unwrap();
                for (callee_id, callee_name, conf, _) in &callees {
                    let mut new_path = path.clone();
                    new_path.push((callee_name.clone(), *conf));
                    if *callee_id == to_id || callee_name == to_name {
                        if found_path.is_none() {
                            found_path = Some(new_path.clone());
                        }
                        all_paths.push(new_path);
                        if all_paths.len() >= 5 { break; }
                    } else if visited.insert(*callee_id) && path.len() < max_depth {
                        queue.push((*callee_id, new_path));
                    }
                }
                if all_paths.len() >= 5 { break; }
            }

            println!("Call path: {} → {}", from, to);
            match found_path {
                None => {
                    println!("  No call path found within {} hops.", max_depth);
                    println!("  Try 'query impact {}' to see the full neighborhood.", from);
                }
                Some(path) => {
                    if all_paths.len() > 1 {
                        println!("  {} path(s) found (showing shortest):", all_paths.len());
                    }
                    for (i, (name, conf)) in path.iter().enumerate() {
                        if i == 0 {
                            println!("  {}", name);
                        } else {
                            println!("  → {} [{:.2}]", name, conf);
                        }
                    }
                    let total_conf: f64 = path.iter().skip(1).map(|(_, c)| c).product();
                    println!("  Path confidence: {:.4}", total_conf);
                }
            }
        }
        QueryCommand::PublicApiSurface { module } => {
            let conn = store.conn();
            let sql = match module {
                Some(m) => format!(
                    "SELECT s.name, s.kind, s.file_path, s.line FROM symbols s
                     WHERE s.is_exported = 1 AND s.file_path LIKE '%{}%'
                     ORDER BY s.file_path, s.line", m),
                None => "SELECT s.name, s.kind, s.file_path, s.line FROM symbols s
                         WHERE s.is_exported = 1 ORDER BY s.file_path, s.line LIMIT 200".to_string(),
            };
            let mut stmt = match conn.prepare(&sql) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let rows = stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
            let mut count = 0;
            for row in &rows { let (n, k, f, l) = row; println!("  {}  {}  {}:{}", n, k, f, l); count += 1; }
            println!("  {} public symbol(s)", count);
        }
        QueryCommand::AffectedTests { symbol } => {
            let syms = store.get_symbols_by_name(symbol).unwrap();
            if syms.is_empty() { eprintln!("'{}' not found.", symbol); std::process::exit(1); }
            let sym = &syms[0];
            let conn = store.conn();

            // Strategy 1: Walk the call graph and find symbols that are tests by name
            let _direct_callers = store.get_callers(sym.id, 1).unwrap();
            let deep_callers = store.get_callers(sym.id, 5).unwrap();

            // Strategy 2: SQL query — find all test-like symbols that transitively call this symbol
            // via the calls table, regardless of file path
            let mut stmt = match conn.prepare(
                "SELECT DISTINCT s.name, s.file_path, s.line, 'CALLS_DIRECTLY' as how
                 FROM calls c
                 JOIN symbols s ON s.id = c.caller_scope_id
                 WHERE c.resolved_symbol_id = ?1
                 AND c.resolved_symbol_id IS NOT NULL
                 AND (
                     s.name LIKE '%test%' OR s.name LIKE '%Test%' OR s.name LIKE '%Spec%'
                     OR s.name LIKE '%_test%' OR s.name LIKE '%_spec%'
                     OR s.name LIKE 'test_%' OR s.name LIKE 'it_%'
                     OR s.name LIKE '%_test_%' OR s.name LIKE '%describe%'
                 )
                 ORDER BY s.file_path, s.line"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let sql_tests: Vec<_> = stmt.query_map([sym.id], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?, row.get::<_, String>(3)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // Strategy 3: Find test files (path-based) that contain callers
            let mut test_by_path: Vec<(String, String, f64, String)> = Vec::new();
            let mut test_by_name: Vec<(String, String, f64, String)> = Vec::new();
            let mut already_seen = std::collections::HashSet::new();

            for (_id, name, conf, fid) in &deep_callers {
                let f = store.get_file_by_id(*fid).unwrap_or(None);
                if let Some(ref ff) = f {
                    let is_test_path = ff.path.contains("test") || ff.path.contains("spec")
                        || ff.path.contains("__tests__") || ff.path.contains("/test/")
                        || ff.path.contains("/tests/") || ff.path.contains("/spec/")
                        || ff.path.ends_with("_test.rs") || ff.path.ends_with("_test.py")
                        || ff.path.ends_with("_test.ts") || ff.path.ends_with("_test.js")
                        || ff.path.ends_with(".test.ts") || ff.path.ends_with(".test.js")
                        || ff.path.ends_with(".spec.ts") || ff.path.ends_with(".spec.js")
                        || ff.path.ends_with("_test.go") || ff.path.ends_with("_test.java");
                    if is_test_path && already_seen.insert(name.clone()) {
                        test_by_path.push((name.clone(), ff.path.clone(), *conf, "test file path".to_string()));
                    }
                }
                // Also check by symbol name pattern
                let is_test_name = name.contains("test") || name.contains("Test")
                    || name.contains("Spec") || name.contains("should_") || name.starts_with("it_")
                    || name.starts_with("test_") || name.ends_with("_test") || name.ends_with("_spec");
                if is_test_name && already_seen.insert(name.clone()) {
                    let fpath = f.as_ref().map(|ff| ff.path.as_str()).unwrap_or("?");
                    test_by_name.push((name.clone(), fpath.to_string(), *conf, "test symbol name".to_string()));
                }
            }

            // Strategy 4: Find test runner invocations — symbols that call test frameworks
            let mut test_runners: Vec<String> = Vec::new();
            for (caller_id, caller_name, _, _) in &deep_callers {
                let callees = store.get_callees(*caller_id, 1).unwrap();
                for (_, callee_name, _, _) in &callees {
                    if callee_name.contains("test") || callee_name.contains("expect") || callee_name.contains("assert")
                        || callee_name.contains("describe") || callee_name.contains("it(") || callee_name.contains("jest")
                        || callee_name.contains("mocha") || callee_name.contains("pytest") || callee_name.contains("runner")
                    {
                        test_runners.push(caller_name.clone());
                        break;
                    }
                }
            }

            println!("Tests affected by changes to '{}':", symbol);
            println!();

            let mut total_found = 0;

            if !sql_tests.is_empty() {
                println!("── Test Symbols (resolved via call graph) ──");
                for (name, file, line, how) in &sql_tests {
                    println!("  ✦ {}  {}:{}  [via {}]", name, file, line, how);
                    total_found += 1;
                }
            }

            if !test_by_path.is_empty() {
                println!("── Test Files (path-based) ──");
                for (name, path, conf, how) in &test_by_path {
                    println!("  ✦ {}  {}  [{:.2}]  [via {}]", name, path, conf, how);
                    total_found += 1;
                }
            }

            let name_only: Vec<_> = test_by_name.iter()
                .filter(|(n, _, _, _)| !sql_tests.iter().any(|(sn, _, _, _)| sn == n) && !test_by_path.iter().any(|(pn, _, _, _)| pn == n))
                .collect();
            if !name_only.is_empty() {
                println!("── Test-like Symbols (name-based) ──");
                for (name, path, conf, how) in &name_only {
                    println!("  ? {}  {}  [{:.2}]  [via {}]", name, path, conf, how);
                    total_found += 1;
                }
            }

            if !test_runners.is_empty() {
                println!("── Test Runners (call test frameworks) ──");
                for name in &test_runners {
                    println!("  ▶ {} (test framework caller)", name);
                    total_found += 1;
                }
            }

            if total_found == 0 {
                println!("  No tests found that depend on '{}'.", symbol);
                println!("  This could mean:");
                println!("    - The symbol is not directly tested");
                println!("    - Tests are not in the indexed codebase");
                println!("    - Call resolution is incomplete (re-index with --semantic)");
            } else {
                println!();
                println!("  {} test symbol(s) affected", total_found);
                println!("  Run: atree query verify test --db <path>");
            }
        }
        QueryCommand::ValidationPlan { symbol } => {
            let syms = store.get_symbols_by_name(symbol).unwrap();
            if syms.is_empty() { eprintln!("'{}' not found.", symbol); std::process::exit(1); }
            let sym = &syms[0];
            let file = store.get_file_by_id(sym.file_id).unwrap_or(None);
            let fp = file.as_ref().map(|f| f.path.as_str()).unwrap_or("?");
            let callers = store.get_callers(sym.id, 3).unwrap();
            let callees = store.get_callees(sym.id, 3).unwrap();
            let heritage = store.get_heritage_by_child(sym.id).unwrap();

            // Risk assessment
            let total_risk_score = callers.len() * 3 + callees.len() + heritage.len() * 2;
            let risk = match total_risk_score {
                0 => "LOW (isolated symbol)",
                1..=5 => "LOW",
                6..=15 => "MEDIUM",
                16..=40 => "HIGH",
                _ => "CRITICAL",
            };

            // Find affected tests
            let test_callers: Vec<&(i64, String, f64, i64)> = callers.iter()
                .filter(|(_, name, _, file_id)| {
                    let f = store.get_file_by_id(*file_id).unwrap_or(None);
                    f.as_ref().map(|ff| ff.path.contains("test") || ff.path.contains("spec")).unwrap_or(false)
                        || name.contains("test") || name.contains("Test") || name.contains("Spec")
                })
                .collect();

            // Find affected files (unique file IDs from callers + callees + self)
            let mut affected_files = std::collections::HashSet::new();
            affected_files.insert(sym.file_id);
            for (_, _, _, fid) in &callers { affected_files.insert(*fid); }
            for (_, _, _, fid) in &callees { affected_files.insert(*fid); }

            println!("═══ Validation Plan: {} ═══", symbol);
            println!("  {}  |  {}:{}  |  Risk: {}", sym.kind, fp, sym.line, risk);
            println!("  Callers: {}  |  Callees: {}  |  Heritage: {}", callers.len(), callees.len(), heritage.len());
            println!("  Affected files: {}", affected_files.len());
            println!();

            // Phase 1: Pre-change analysis
            println!("── Phase 1: Pre-change analysis ──");
            if !callers.is_empty() {
                println!("  Callers to review ({}):", callers.len());
                for (_id, n, conf, fid) in &callers {
                    let f = store.get_file_by_id(*fid).unwrap_or(None);
                    let fpath = f.as_ref().map(|ff| ff.path.as_str()).unwrap_or("?");
                    println!("    ← {} [{:.2}] {}", n, conf, fpath);
                }
            } else {
                println!("  No callers — change is safe from consumer side.");
            }
            if !callees.is_empty() {
                println!("  Dependencies to verify ({}):", callees.len());
                for (_id, n, conf, _) in &callees {
                    println!("    → {} [{:.2}]", n, conf);
                }
            }
            if !heritage.is_empty() {
                println!("  Inheritance chain:");
                for h in &heritage {
                    println!("    ↑ {} {} [{:.2}]", h.heritage_kind, h.parent_name, h.confidence);
                }
            }
            println!();

            // Phase 2: Test impact
            println!("── Phase 2: Test impact ──");
            if test_callers.is_empty() {
                println!("  No directly-associated test symbols found.");
                println!("  Run: atree query affected-tests {}  (path-based search)", symbol);
            } else {
                println!("  {} test symbol(s) to run:", test_callers.len());
                for (_, n, _, fid) in &test_callers {
                    let f = store.get_file_by_id(*fid).unwrap_or(None);
                    let fpath = f.as_ref().map(|ff| ff.path.as_str()).unwrap_or("?");
                    println!("    ✦ {} ({})", n, fpath);
                }
            }
            println!();

            // Phase 3: Execution plan
            println!("── Phase 3: Execution plan ──");
            println!("  1. Review {} caller(s) for API compatibility", callers.len());
            println!("  2. Verify {} callee behavior unchanged", callees.len());
            if !test_callers.is_empty() {
                println!("  3. Run {} associated test symbol(s)", test_callers.len());
            }
            println!("  {}. Run full test suite:  atree query verify test --db <path>", if test_callers.is_empty() { 3 } else { 4 });
            println!("  {}. Check for regressions: atree query detect-changes --db <path>", if test_callers.is_empty() { 4 } else { 5 });
        }
        QueryCommand::ContractChangeDetector { base_ref } => {
            let conn = store.conn();
            let files = store.get_all_files().unwrap();
            if files.is_empty() {
                println!("No indexed files. Run 'atree --semantic --db <path> --root <repo>' first.");
                std::process::exit(0);
            }

            // Get current public API surface
            let mut stmt = match conn.prepare(
                "SELECT s.name, s.kind, s.qualified_name, s.file_path, s.line
                 FROM symbols s WHERE s.is_exported = 1
                 ORDER BY s.qualified_name"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let current_public: Vec<(String, String, String, String, i64)> = stmt.query_map([], |row| Ok((
                row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            if let Some(base) = base_ref {
                // Compare current public API against git base ref
                let repo_path = std::path::Path::new(&files[0].path).parent().map(|p| p.to_path_buf());
                if let Some(path) = repo_path {
                    // Get changed files between base and HEAD
                    let output = std::process::Command::new("git")
                        .args(["diff", "--name-only", base, "HEAD"])
                        .current_dir(&path)
                        .output();
                    match output {
                        Ok(out) if out.status.success() => {
                            let changed: Vec<String> = String::from_utf8_lossy(&out.stdout)
                                .lines().map(|s| s.to_string()).filter(|s| !s.is_empty()).collect();
                            // Find which public symbols are in changed files
                            let mut changed_public: Vec<&(String, String, String, String, i64)> = Vec::new();
                            let mut unchanged_public: Vec<&(String, String, String, String, i64)> = Vec::new();
                            for sym in &current_public {
                                if changed.iter().any(|cf| sym.3.contains(cf)) {
                                    changed_public.push(sym);
                                } else {
                                    unchanged_public.push(sym);
                                }
                            }
                            // Detect new public symbols (exist now but file is new/modified)
                            let mut new_symbols: Vec<&(String, String, String, String, i64)> = Vec::new();
                            for f in &changed {
                                let diff_out = std::process::Command::new("git")
                                    .args(["diff", base, "HEAD", "--", f])
                                    .current_dir(&path)
                                    .output();
                                if let Ok(do_) = diff_out {
                                    let diff_text = String::from_utf8_lossy(&do_.stdout);
                                    for sym in &current_public {
                                        if sym.3.contains(f) {
                                            // Check if this symbol's line was added
                                            for line in diff_text.lines() {
                                                if line.starts_with('+') && !line.starts_with("+++") {
                                                    if line.contains(&sym.0) || line.contains(&format!("fn {}", sym.0)) || line.contains(&format!("class {}", sym.0)) || line.contains(&format!("def {}", sym.0)) {
                                                        if !new_symbols.contains(&sym) {
                                                            new_symbols.push(sym);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            println!("Contract change detection (vs {}):", base);
                            println!("  {} public symbols total", current_public.len());
                            println!("  {} public symbols in changed files", changed_public.len());
                            println!("  {} potentially new public symbols", new_symbols.len());
                            println!();

                            if !changed_public.is_empty() {
                                println!("── Modified public API ──");
                                for (name, kind, qname, file, line) in &changed_public {
                                    let is_new = new_symbols.iter().any(|s| s.2 == *qname);
                                    let tag = if is_new { " [NEW]" } else { " [MODIFIED]" };
                                    println!("  {}  {}  {}:{}{}", kind, name, file, line, tag);
                                }
                            } else {
                                println!("  No public API changes detected.");
                            }

                            if !new_symbols.is_empty() {
                                println!();
                                println!("── New public symbols (potential breaking additions) ──");
                                for (name, kind, _qname, file, line) in &new_symbols {
                                    println!("  + {}  {}  {}:{}", kind, name, file, line);
                                }
                            }
                        }
                        _ => {
                            eprintln!("Error: git diff failed. Is '{}' a valid ref?", base);
                            std::process::exit(1);
                        }
                    }
                }
            } else {
                // No base ref — show current public API surface with change detection via git status
                let repo_path = std::path::Path::new(&files[0].path).parent().map(|p| p.to_path_buf());
                let changed = repo_path.and_then(|p| atree_engine::detect_git_changes(&p));
                let mut changed_public = Vec::new();
                let mut stable_public = Vec::new();
                for sym in &current_public {
                    match &changed {
                        Some(c) if c.iter().any(|cf| sym.3.contains(cf)) => changed_public.push(sym),
                        _ => stable_public.push(sym),
                    }
                }
                println!("Contract surface (current index):");
                println!("  {} public symbols total", current_public.len());
                if !changed_public.is_empty() {
                    println!("  {} public symbols in uncommitted changed files:", changed_public.len());
                    for (name, kind, _qname, file, line) in &changed_public {
                        println!("  ⚠ {}  {}  {}:{}", kind, name, file, line);
                    }
                }
                if !stable_public.is_empty() {
                    println!("  {} stable public symbols:", stable_public.len());
                    for (name, kind, _qname, file, line) in &stable_public {
                        println!("    {}  {}  {}:{}", kind, name, file, line);
                    }
                }
                println!();
                println!("  Use --base <ref> to compare against a git ref (e.g., --base main).");
            }
        }
        QueryCommand::ArchitectureBoundaryCheck => {
            println!("Architecture boundary check:");
            let conn = store.conn();

            // Strategy 1: Cross-repo boundary violations (using repo_label)
            let mut stmt = match conn.prepare(
                "SELECT s1.name, s1.file_path, f1.repo_label, s2.name, s2.file_path, f2.repo_label
                 FROM calls c
                 JOIN symbols s1 ON s1.id = c.caller_scope_id
                 JOIN files f1 ON f1.id = s1.file_id
                 JOIN symbols s2 ON s2.id = c.resolved_symbol_id
                 JOIN files f2 ON f2.id = s2.file_id
                 WHERE c.resolved_symbol_id IS NOT NULL
                 AND f1.repo_label IS NOT NULL AND f2.repo_label IS NOT NULL
                 AND f1.repo_label != f2.repo_label
                 ORDER BY f1.repo_label, f2.repo_label
                 LIMIT 50"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let cross_repo: Vec<_> = stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, String>(3)?,
                row.get::<_, String>(4)?, row.get::<_, String>(5)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // Strategy 2: Cross-module boundary violations within a repo
            // Detect calls between different top-level directories (module boundaries)
            let mut stmt2 = match conn.prepare(
                "SELECT s1.name, s1.file_path, s2.name, s2.file_path
                 FROM calls c
                 JOIN symbols s1 ON s1.id = c.caller_scope_id
                 JOIN symbols s2 ON s2.id = c.resolved_symbol_id
                 JOIN files f1 ON f1.id = s1.file_id
                 JOIN files f2 ON f2.id = s2.file_id
                 WHERE c.resolved_symbol_id IS NOT NULL
                 AND f1.file_path IS NOT NULL AND f2.file_path IS NOT NULL
                 AND f1.file_path != f2.file_path
                 -- Different top-level directory (module boundary)
                 AND SUBSTR(f1.file_path, 1, INSTR(f1.file_path || '/', '/') - 1) !=
                     SUBSTR(f2.file_path, 1, INSTR(f2.file_path || '/', '/') - 1)
                 ORDER BY s1.file_path, s2.file_path
                 LIMIT 100"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let cross_module: Vec<_> = stmt2.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, String>(3)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();




            // Strategy 3: Import-based boundary violations (imports from non-adjacent modules)
            let mut stmt3 = match conn.prepare(
                "SELECT s1.name, s1.file_path, i.source, i.imported_name
                 FROM imports i
                 JOIN symbols s1 ON s1.file_id = i.file_id
                 JOIN files f1 ON f1.id = i.file_id
                 LEFT JOIN files f2 ON f2.id = i.resolved_file_id
                 WHERE i.resolved_file_id IS NOT NULL
                 AND f1.file_path IS NOT NULL AND f2.file_path IS NOT NULL
                 AND SUBSTR(f1.file_path, 1, INSTR(f1.file_path || '/', '/') - 1) !=
                     SUBSTR(f2.file_path, 1, INSTR(f2.file_path || '/', '/') - 1)
                 ORDER BY f1.file_path
                 LIMIT 50"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let cross_import: Vec<_> = stmt3.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, String>(3)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            let total = cross_repo.len() + cross_module.len() + cross_import.len();

            if total == 0 {
                println!("  No architecture boundary violations detected.");
                println!("  (This could mean clean boundaries OR incomplete call resolution.)");
                println!("  Re-index with --semantic for best results.");
            } else {
                if !cross_repo.is_empty() {
                    println!("── Cross-Repo Boundary Violations ({}) ──", cross_repo.len());
                    for (from, fp1, repo1, to, fp2, repo2) in &cross_repo {
                        println!("  {} ({}) [{}] → {} ({}) [{}]", from, fp1, repo1, to, fp2, repo2);
                    }
                    println!();
                }
                if !cross_module.is_empty() {
                    println!("── Cross-Module Boundary Violations ({}) ──", cross_module.len());
                    // Deduplicate
                    let mut seen = std::collections::HashSet::new();
                    for (from, fp1, to, fp2) in &cross_module {
                        let key = (from.clone(), to.clone());
                        if seen.insert(key) {
                            println!("  {} ({}) → {} ({})", from, fp1, to, fp2);
                        }
                    }
                    println!();
                }
                if !cross_import.is_empty() {
                    println!("── Cross-Module Import Violations ({}) ──", cross_import.len());
                    let mut seen = std::collections::HashSet::new();
                    for (from, fp1, source, imported) in &cross_import {
                        let key = (from.clone(), imported.clone());
                        if seen.insert(key) {
                            println!("  {} ({}) imports {} from {}", from, fp1, imported, source);
                        }
                    }
                }
                println!();
                println!("  {} total boundary violations", total);
            }
        }
        QueryCommand::ScopeViolationDetector => {
            println!("Scope violation detection:");
            let conn = store.conn();

            // Strategy 1: Non-exported symbols called from outside their file (existing logic, expanded)
            let mut stmt = match conn.prepare(
                "SELECT s.name, s.file_path, s.line, s.kind FROM symbols s
                 WHERE s.is_exported = 0
                 AND s.kind IN ('Function', 'Method', 'Class')
                 ORDER BY s.file_path, s.line"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let rows: Vec<_> = stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?, row.get::<_, String>(3)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
            let mut violations: Vec<(String, String, i64, String, String, String, String)> = Vec::new();
            // (name, file, line, kind, caller_name, caller_file, violation_type)

            for (name, file, line, kind) in &rows {
                let syms = store.get_symbols_by_name(name).unwrap();
                for sym in &syms {
                    let callers = store.get_callers(sym.id, 1).unwrap();
                    for (_id, caller_name, _, caller_file_id) in &callers {
                        let caller_file = store.get_file_by_id(*caller_file_id).unwrap_or(None);
                        if let Some(ref cf) = caller_file {
                            if cf.path != *file {
                                violations.push((
                                    name.clone(), file.clone(), *line, kind.clone(),
                                    caller_name.clone(), cf.path.clone(),
                                    "non-exported cross-file".to_string(),
                                ));
                                break;
                            }
                        }
                    }
                }
            }

            // Strategy 2: Naming convention violations — symbols that follow private naming
            // conventions but are used externally
            // Python: _prefix (protected), __prefix (name-mangled/private)
            // Rust: no pub keyword (handled by is_exported)
            // Java/Kotlin/C#/TS: private keyword (handled by is_exported)
            // But we can catch symbols with private naming that ARE exported
            let mut stmt2 = match conn.prepare(
                "SELECT s.name, s.file_path, s.line, s.kind FROM symbols s
                 WHERE s.is_exported = 1
                 AND (
                     -- Python: exported but starts with _ (protected by convention)
                     (s.name LIKE '_%' AND s.name NOT LIKE '__%')
                     OR
                     -- Any language: exported but name contains internal/private/impl
                     s.name LIKE '%_internal%' OR s.name LIKE '%_private%' OR s.name LIKE '%_impl%'
                     OR s.name LIKE '%Internal%' OR s.name LIKE '%Private%'
                 )
                 AND s.kind IN ('Function', 'Method', 'Class')
                 ORDER BY s.file_path, s.line LIMIT 50"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let naming_violations: Vec<_> = stmt2.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?, row.get::<_, String>(3)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();




            for (name, file, line, kind) in &naming_violations {
                violations.push((
                    name.clone(), file.clone(), *line, kind.clone(),
                    "".to_string(), "".to_string(),
                    "exported-but-private-naming".to_string(),
                ));
            }

            // Strategy 3: Cross-module private access — symbols in private modules (_prefix)
            // that are imported by public modules
            let mut stmt3 = match conn.prepare(
                "SELECT s.name, s.file_path, s.line, i.source
                 FROM imports i
                 JOIN symbols s ON s.file_id = i.file_id
                 JOIN files f ON f.id = i.file_id
                 WHERE i.source LIKE '%/_%'
                 AND i.source NOT LIKE '%/__pycache__%'
                 AND s.is_exported = 1
                 ORDER BY s.file_path, s.line LIMIT 25"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let private_imports: Vec<_> = stmt3.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?, row.get::<_, String>(3)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();




            for (name, file, line, source) in &private_imports {
                violations.push((
                    name.clone(), file.clone(), *line, "import".to_string(),
                    source.clone(), "".to_string(),
                    "imports-from-private-module".to_string(),
                ));
            }

            if violations.is_empty() {
                println!("  No scope violations detected.");
            } else {
                // Deduplicate
                let mut seen = std::collections::HashSet::new();
                let mut unique: Vec<_> = Vec::new();
                for v in &violations {
                    let key = (v.0.clone(), v.1.clone(), v.2, v.6.clone());
                    if seen.insert(key) {
                        unique.push(v);
                    }
                }

                println!("  {} scope violation(s) detected:", unique.len());
                println!();

                // Group by violation type
                let cross_file: Vec<_> = unique.iter().filter(|v| v.6 == "non-exported cross-file").collect();
                let naming: Vec<_> = unique.iter().filter(|v| v.6 == "exported-but-private-naming").collect();
                let private_mod: Vec<_> = unique.iter().filter(|v| v.6 == "imports-from-private-module").collect();

                if !cross_file.is_empty() {
                    println!("── Non-exported symbols used externally ({}) ──", cross_file.len());
                    for (name, file, line, kind, caller, caller_file, _) in &cross_file {
                        println!("  {}  {}  {}:{}  (called from {} in {})", kind, name, file, line, caller, caller_file);
                    }
                }
                if !naming.is_empty() {
                    println!("── Exported symbols with private naming ({}) ──", naming.len());
                    for (name, file, line, kind, _, _, _) in &naming {
                        println!("  {}  {}  {}:{}  (exported but name suggests private)", kind, name, file, line);
                    }
                }
                if !private_mod.is_empty() {
                    println!("── Imports from private modules ({}) ──", private_mod.len());
                    for (name, file, line, _, source, _, _) in &private_mod {
                        println!("  {}  {}:{}  (imports from private module: {})", name, file, line, source);
                    }
                }
            }
        }
        QueryCommand::ConfigSurfaceMap => {
            println!("Configuration surface map:");
            let conn = store.conn();

            // Strategy 1: Keyword-based config symbols (expanded)
            let mut stmt = match conn.prepare(
                "SELECT s.name, s.kind, s.file_path, s.line, 'keyword' as detection_method
                 FROM symbols s
                 WHERE (s.name LIKE '%config%' OR s.name LIKE '%env%' OR s.name LIKE '%setting%'
                        OR s.name LIKE '%feature%' OR s.name LIKE '%flag%' OR s.name LIKE '%var%'
                        OR s.name LIKE '%option%' OR s.name LIKE '%param%' OR s.name LIKE '%pref%'
                        OR s.name LIKE '%secret%' OR s.name LIKE '%token%' OR s.name LIKE '%key%'
                        OR s.name LIKE '%credential%' OR s.name LIKE '%password%' OR s.name LIKE '%api_key%')
                 AND s.kind IN ('Const', 'Variable', 'Static', 'Function', 'Struct', 'Class')
                 ORDER BY s.file_path, s.line LIMIT 100"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let keyword_rows: Vec<_> = stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
            // Strategy 2: Detect environment variable access patterns via calls table
            let mut stmt2 = match conn.prepare(
                "SELECT DISTINCT s.name, s.kind, s.file_path, s.line, 'env_access' as detection_method
                 FROM calls c
                 JOIN symbols s ON s.id = c.caller_scope_id
                 WHERE c.callee_name IN (
                     'env', 'var', 'set_var', 'remove_var', 'vars',
                     'env_var', 'get_env', 'set_env', 'read_env',
                     'std::env::var', 'std::env::set_var', 'std::env::vars',
                     'dotenv', 'dotenv_var', 'from_env', 'env_logger',
                     'config', 'load_config', 'read_config', 'parse_config',
                     'from_str', 'from_env', 'try_from_env'
                 )
                 ORDER BY s.file_path, s.line LIMIT 50"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let env_rows: Vec<_> = stmt2.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // Strategy 3: Detect config file readers
            let mut stmt3 = match conn.prepare(
                "SELECT DISTINCT s.name, s.kind, s.file_path, s.line, 'config_reader' as detection_method
                 FROM calls c
                 JOIN symbols s ON s.id = c.caller_scope_id
                 WHERE c.callee_name IN (
                     'read_to_string', 'read', 'open', 'File::open',
                     'from_reader', 'from_path', 'load', 'parse',
                     'toml', 'serde_json', 'serde_yaml', 'serde_toml',
                     'ConfigBuilder', 'Config', 'Environment',
                     'configparser', 'ConfigParser', 'load_dotenv'
                 )
                 AND s.name NOT LIKE '%test%' AND s.name NOT LIKE '%Test%'
                 ORDER BY s.file_path, s.line LIMIT 50"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let reader_rows: Vec<_> = stmt3.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // Deduplicate
            let mut seen = std::collections::HashSet::new();
            let mut all_results: Vec<(String, String, String, i64, String)> = Vec::new();
            for row in keyword_rows.iter().chain(&env_rows).chain(&reader_rows) {
                if seen.insert(row.0.clone()) {
                    all_results.push(row.clone());
                }
            }

            if all_results.is_empty() {
                println!("  No configuration surface symbols found.");
            } else {
                println!("  {} config surface symbols:", all_results.len());
                println!();
                all_results.sort_by(|a, b| a.2.cmp(&b.2).then(a.3.cmp(&b.3)));
                for (name, kind, file, line, method) in &all_results {
                    println!("  {:<16}  {:<10}  {}  {}:{}", method, kind, name, file, line);
                }
            }
        }
        QueryCommand::ImpactBySymbolKind { target, kind, direction } => {
            let syms = store.get_symbols_by_name(&target).unwrap();
            if syms.is_empty() { eprintln!("'{}' not found.", target); std::process::exit(1); }
            let sym = &syms[0];
            println!("Impact for '{}' (kind filter: {}, direction: {})", target, kind, direction);
            if direction == "upstream" || direction == "both" {
                let callers = store.get_callers(sym.id, 3).unwrap();
                println!("  Upstream (callers):");
                for (_id, n, conf, _) in &callers {
                    let caller_syms = store.get_symbols_by_name(n).unwrap();
                    let matches = caller_syms.iter().any(|s| s.kind.to_lowercase().contains(&kind.to_lowercase()));
                    if matches || kind == "*" {
                        println!("    {} [{:.2}]", n, conf);
                    }
                }
            }
            if direction == "downstream" || direction == "both" {
                let callees = store.get_callees(sym.id, 3).unwrap();
                println!("  Downstream (callees):");
                for (_id, n, conf, _) in &callees {
                    let callee_syms = store.get_symbols_by_name(n).unwrap();
                    let matches = callee_syms.iter().any(|s| s.kind.to_lowercase().contains(&kind.to_lowercase()));
                    if matches || kind == "*" {
                        println!("    {} [{:.2}]", n, conf);
                    }
                }
            }
        }
        QueryCommand::SemanticDiffSummary { base_ref } => {
            // Unlike detect_changes (which lists changed files + their symbols),
            // semantic_diff_summary shows the TRANSITIVE impact: for each changed
            // symbol, what callers are affected (blast radius).
            println!("Semantic diff summary{}:",
                base_ref.as_ref().map(|b| format!(" (vs {})", b)).unwrap_or_default());
            let files = store.get_all_files().unwrap();
            if files.is_empty() { println!("  No indexed files."); std::process::exit(0); }
            let repo_path = std::path::Path::new(&files[0].path).parent().map(|p| p.to_path_buf());
            if let Some(path) = repo_path {
                let changed = if let Some(base) = base_ref {
                    // Get files changed between base and HEAD
                    let output = std::process::Command::new("git")
                        .args(["diff", "--name-only", base, "HEAD"])
                        .current_dir(&path).output();
                    match output {
                        Ok(out) if out.status.success() => {
                            let c: std::collections::HashSet<String> = String::from_utf8_lossy(&out.stdout)
                                .lines().filter(|s| !s.is_empty()).map(|s| s.to_string()).collect();
                            if c.is_empty() { None } else { Some(c) }
                        }
                        _ => { eprintln!("Error: git diff failed for ref '{}'", base); std::process::exit(1); }
                    }
                } else {
                    atree_engine::detect_git_changes(&path)
                };

                match changed {
                    Some(c) if !c.is_empty() => {
                        let conn = store.conn();
                        let mut total_affected_callers = 0;
                        let mut high_impact_symbols: Vec<(String, String, i64, usize, Vec<String>)> = Vec::new();

                        println!("  {} changed file(s):", c.len());
                        for f in &c {
                            println!("  ── {} ──", f);
                            let file_records: Vec<_> = files.iter().filter(|ff| ff.path == *f).collect();
                            for fs in &file_records {
                                let mut stmt = match conn.prepare("SELECT id, name, kind, line FROM symbols WHERE file_id = ?1") { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
                                let syms: Vec<_> = stmt.query_map([fs.id], |row| Ok((
                                    row.get::<_, i64>(0)?, row.get::<_, String>(1)?,
                                    row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
                                ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

                                for (sym_id, name, kind, line) in &syms {
                                    let callers = store.get_callers(*sym_id, 3).unwrap();
                                    let caller_names: Vec<String> = callers.iter().map(|(_, n, _, _)| n.clone()).collect();
                                    let impact = callers.len();
                                    total_affected_callers += impact;
                                    if impact > 0 {
                                        high_impact_symbols.push((name.clone(), kind.clone(), *line, impact, caller_names.clone()));
                                    }
                                    let impact_indicator = match impact {
                                        0 => "  ○".to_string(),
                                        1..=2 => format!("  ◐ ({})", impact),
                                        _ => format!("  ● ({})", impact),
                                    };
                                    println!("    {} {} {}:{}  {}", impact_indicator, kind, name, line,
                                        if impact > 0 { format!("→ {} callers", impact) } else { String::new() });
                                }
                            }
                        }

                        if !high_impact_symbols.is_empty() {
                            println!();
                            println!("── High-impact changes (sorted by blast radius) ──");
                            high_impact_symbols.sort_by(|a, b| b.3.cmp(&a.3));
                            for (name, kind, line, impact, callers) in &high_impact_symbols {
                                println!("  {}  {}:{}  ({} callers impacted)", name, kind, line, impact);
                                for c_name in callers.iter().take(5) {
                                    println!("    ← {}", c_name);
                                }
                                if callers.len() > 5 {
                                    println!("    ... and {} more", callers.len() - 5);
                                }
                            }
                        }

                        println!();
                        println!("  Total: {} changed files, {} symbols with downstream impact",
                            c.len(), high_impact_symbols.len());
                        println!("  {} total caller relationships affected", total_affected_callers);
                    }
                    _ => println!("  No uncommitted changes."),
                }
            }
        }
        QueryCommand::SideEffectScanner { symbol } => {
            let syms = store.get_symbols_by_name(symbol).unwrap();
            if syms.is_empty() { eprintln!("'{}' not found.", symbol); std::process::exit(1); }
            let sym = &syms[0];
            let direct_callees = store.get_callees(sym.id, 1).unwrap();
            let deep_callees = store.get_callees(sym.id, 3).unwrap();

            // Comprehensive I/O side effect categories
            let io_effects = [
                // Console / logging
                "print", "println", "eprint", "eprintln", "console", "log", "warn", "error", "debug", "trace", "info",
                // File I/O
                "read", "write", "open", "close", "create", "remove", "delete", "rename", "copy", "seek", "flush",
                "read_to", "write_all", "read_file", "write_file", "read_dir", "metadata", "canonicalize",
                // Network
                "send", "recv", "connect", "listen", "accept", "bind", "request", "fetch", "http", "https",
                "tcp", "udp", "socket", "tls", "websocket", "grpc",
                // Database
                "query", "execute", "insert", "update", "drop", "commit", "rollback", "transaction",
                "database", "db", "pool", "connection", "migrate",
                // Process / thread
                "spawn", "thread", "fork", "exec", "process", "command", "kill", "wait", "join",
                "channel", "mpsc", "broadcast",
                // Memory / sync
                "lock", "mutex", "rwlock", "atomic", "arc", "rc", "cell", "refcell", "lazy", "once",
                // Time / env
                "sleep", "timeout", "interval", "timer", "now", "elapsed",
                "env", "var", "set_var", "args", "stdin", "stdout", "stderr",
                // FFI / unsafe
                "unsafe", "extern", "ffi", "c_str", "from_raw", "into_raw",
                // Serialization (can fail = side effect)
                "serialize", "deserialize", "encode", "decode", "to_json", "from_json", "to_string", "parse",
            ];

            let mut direct_effects: Vec<(String, String, f64, String)> = Vec::new();
            let mut transitive_effects: Vec<(String, String, f64, String, String)> = Vec::new();

            // Check direct callees
            for (_id, name, conf, _) in &direct_callees {
                let lower = name.to_lowercase();
                for effect in &io_effects {
                    if lower.contains(effect) {
                        direct_effects.push((name.clone(), effect.to_string(), *conf, "direct".to_string()));
                        break;
                    }
                }
            }

            // 2-hop: check what the callees call
            for (id, name, conf, _) in &deep_callees {
                if direct_callees.iter().any(|(did, _, _, _)| did == id) { continue; }
                let lower = name.to_lowercase();
                for effect in &io_effects {
                    if lower.contains(effect) {
                        // Find which direct callee leads to this
                        let via = direct_callees.iter()
                            .filter(|(dcid, _, _, _)| {
                                let dc_callees = store.get_callees(*dcid, 1).unwrap();
                                dc_callees.iter().any(|(ccid, _, _, _)| ccid == id)
                            })
                            .map(|(_, dcname, _, _)| dcname.clone())
                            .next()
                            .unwrap_or_else(|| "?".to_string());
                        transitive_effects.push((name.clone(), effect.to_string(), *conf, via, "transitive".to_string()));
                        break;
                    }
                }
            }

            println!("Side effect scan for '{}':", symbol);
            println!();

            if direct_effects.is_empty() && transitive_effects.is_empty() {
                println!("  ✅ No I/O side effects detected in 3-hop callee chain.");
                println!("     This symbol appears to be a pure computation.");
            } else {
                if !direct_effects.is_empty() {
                    println!("── Direct Side Effects (depth 1) ──");
                    for (name, effect, conf, _) in &direct_effects {
                        println!("  ⚠ {}  [{:.2}]  (I/O: {})", name, conf, effect);
                    }
                }
                if !transitive_effects.is_empty() {
                    println!("── Transitive Side Effects (depth 2-3) ──");
                    for (name, effect, conf, via, _) in &transitive_effects {
                        println!("  ⚠ {}  [{:.2}]  (I: {}, via {})", name, conf, effect, via);
                    }
                }
                println!();
                println!("  {} direct + {} transitive = {} total side-effect callees",
                    direct_effects.len(), transitive_effects.len(),
                    direct_effects.len() + transitive_effects.len());

                // Categorize
                let mut categories = std::collections::HashSet::new();
                for (_, effect, _, _) in &direct_effects { categories.insert(effect.clone()); }
                for (_, effect, _, _, _) in &transitive_effects { categories.insert(effect.clone()); }
                let mut cats: Vec<_> = categories.into_iter().collect();
                cats.sort();
                println!("  Categories: {}", cats.join(", "));
            }
        }
        QueryCommand::ChangeCoupling { symbol } => {
            println!("Change coupling for '{}':", symbol);
            let syms = store.get_symbols_by_name(symbol).unwrap();
            if syms.is_empty() { eprintln!("'{}' not found.", symbol); std::process::exit(1); }
            let sym = &syms[0];
            let files = store.get_all_files().unwrap();
            if files.is_empty() { println!("  No indexed files."); std::process::exit(0); }

            // Strategy 1: Static coupling — symbols that share callers (existing logic, expanded)
            let callers = store.get_callers(sym.id, 3).unwrap();
            let mut static_coupled = std::collections::HashSet::new();
            for (_id, caller_name, _, _caller_file_id) in &callers {
                let caller_syms = store.get_symbols_by_name(caller_name).unwrap();
                for cs in &caller_syms {
                    let cs_callees = store.get_callees(cs.id, 2).unwrap();
                    for (_id2, callee_name, _, _) in &cs_callees {
                        if callee_name != symbol { static_coupled.insert(callee_name.clone()); }
                    }
                }
            }

            // Strategy 2: Git history co-change analysis
            // Find files that frequently change together with this symbol's file
            let repo_path = std::path::Path::new(&files[0].path).parent().map(|p| p.to_path_buf());
            let mut git_coupled: Vec<(String, usize)> = Vec::new();

            if let Some(ref path) = repo_path {
                // Get the file path for this symbol
                let sym_file = files.iter().find(|f| f.id == sym.file_id);
                if let Some(sf) = sym_file {
                    // Use git log to find commits that touched this file
                    let log_output = std::process::Command::new("git")
                        .args(["log", "--format=%H", "--follow", "--", &sf.path])
                        .current_dir(path)
                        .output();

                    if let Ok(output) = log_output {
                        if output.status.success() {
                            let commits: Vec<String> = String::from_utf8_lossy(&output.stdout)
                                .lines().map(|s| s.to_string()).filter(|s| !s.is_empty()).collect();

                            // For each commit, find other files that were changed
                            let mut cochange_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
                            // Limit to last 50 commits for performance
                            for commit in commits.iter().take(50) {
                                let show_output = std::process::Command::new("git")
                                    .args(["show", "--name-only", "--format=", commit])
                                    .current_dir(path)
                                    .output();
                                if let Ok(so) = show_output {
                                    if so.status.success() {
                                        let changed_files: Vec<String> = String::from_utf8_lossy(&so.stdout)
                                            .lines().map(|s| s.to_string()).filter(|s| !s.is_empty() && s != &sf.path)
                                            .collect();
                                        for cf in &changed_files {
                                            *cochange_counts.entry(cf.clone()).or_insert(0) += 1;
                                        }
                                    }
                                }
                            }

                            // Convert to sorted list
                            let mut cochange_vec: Vec<(String, usize)> = cochange_counts.into_iter().collect();
                            cochange_vec.sort_by(|a, b| b.1.cmp(&a.1));
                            git_coupled = cochange_vec.into_iter().take(20).collect();
                        }
                    }

                    // Strategy 3: Find symbols in co-changed files
                    let mut git_symbol_coupled = std::collections::HashSet::new();
                    for (coupled_file, _) in &git_coupled {
                        let coupled_file_records: Vec<_> = files.iter().filter(|f| f.path == *coupled_file).collect();
                        for cfr in &coupled_file_records {
                            let conn = store.conn();
                            let mut stmt = match conn.prepare("SELECT name FROM symbols WHERE file_id = ?1 LIMIT 10") { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
                            let rows: Vec<_> = stmt.query_map([cfr.id], |row| Ok(row.get::<_, String>(0)?))
                                .unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
                            for row in &rows {
                                if row != symbol { git_symbol_coupled.insert(row.clone()); }
                            }
                        }
                    }

                    println!("  Change coupling for '{}':", symbol);
                    println!("  File: {}:{}", sym.file_id, sym.line);
                    println!();

                    if !static_coupled.is_empty() {
                        println!("── Static Coupling (shared callers, depth 3) ──");
                        println!("  {} symbols that share callers with '{}':", static_coupled.len(), symbol);
                        for name in &static_coupled { println!("    - {}", name); }
                        println!();
                    }

                    if !git_coupled.is_empty() {
                        println!("── Git History Co-Change (last 50 commits) ──");
                        println!("  Files that frequently change with {}:", sf.path);
                        for (file, count) in &git_coupled {
                            println!("    {}  ({} co-commits)", file, count);
                        }
                        println!();

                        if !git_symbol_coupled.is_empty() {
                            println!("── Symbols in Co-Changed Files ──");
                            for name in &git_symbol_coupled {
                                if !static_coupled.contains(name) {
                                    println!("    - {} (via git history)", name);
                                }
                            }
                        }
                    } else if static_coupled.is_empty() {
                        println!("  No change coupling detected.");
                        println!("  (No shared callers found and no git history available.)");
                    }
                }
            } else {
                // No git repo — fall back to static coupling only
                if static_coupled.is_empty() {
                    println!("  No change coupling detected (no git repo available).");
                } else {
                    println!("  {} statically coupled symbols:", static_coupled.len());
                    for name in &static_coupled { println!("    - {}", name); }
                }
            }
        }
        QueryCommand::ConcurrencySurfaceDetector { symbol } => {
            println!("Concurrency surface detection:");
            let conn = store.conn();

            // Strategy 1: Keyword-based detection (expanded)
            let mut stmt = match conn.prepare(
                "SELECT s.name, s.kind, s.file_path, s.line, 'keyword' as detection_method
                 FROM symbols s
                 WHERE (s.name LIKE '%lock%' OR s.name LIKE '%mutex%' OR s.name LIKE '%atomic%'
                        OR s.name LIKE '%thread%' OR s.name LIKE '%spawn%' OR s.name LIKE '%async%'
                        OR s.name LIKE '%await%' OR s.name LIKE '%channel%' OR s.name LIKE '%arc%'
                        OR s.name LIKE '%sync%' OR s.name LIKE '%concurrent%' OR s.name LIKE '%parallel%'
                        OR s.name LIKE '%rwlock%' OR s.name LIKE '%semaphore%' OR s.name LIKE '%barrier%'
                        OR s.name LIKE '%condvar%' OR s.name LIKE '%lazy_static%' OR s.name LIKE '%once_cell%'
                        OR s.name LIKE '%tokio%' OR s.name LIKE '%smol%' OR s.name LIKE '%executor%'
                        OR s.name LIKE '%worker%' OR s.name LIKE '%pool%' OR s.name LIKE '%queue%')
                 ORDER BY s.file_path, s.line LIMIT 100"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let keyword_rows: Vec<_> = stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // Strategy 2: Detect async functions
            let mut stmt2 = match conn.prepare(
                "SELECT DISTINCT s.name, s.kind, s.file_path, s.line, 'async_call' as detection_method
                 FROM calls c
                 JOIN symbols s ON s.id = c.caller_scope_id
                 WHERE c.callee_name LIKE '%await%'
                    OR c.callee_name LIKE '%spawn%'
                    OR c.callee_name LIKE '%join%'
                    OR c.callee_name IN ('block_on', 'spawn_blocking', 'yield_now', 'sleep', 'timeout')
                 ORDER BY s.file_path, s.line LIMIT 50"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let async_rows: Vec<_> = stmt2.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // Strategy 3: Detect shared-state patterns
            let mut stmt3 = match conn.prepare(
                "SELECT DISTINCT s.name, s.kind, s.file_path, s.line, 'shared_state' as detection_method
                 FROM calls c
                 JOIN symbols s ON s.id = c.caller_scope_id
                 WHERE c.callee_name IN ('Arc', 'Rc', 'Mutex', 'RwLock', 'Cell', 'RefCell',
                     'lock', 'read', 'write', 'try_lock', 'get_mut')
                 ORDER BY s.file_path, s.line LIMIT 50"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let shared_rows: Vec<_> = stmt3.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // Deduplicate
            let mut seen = std::collections::HashSet::new();
            let mut all_results: Vec<(String, String, String, i64, String)> = Vec::new();
            for row in keyword_rows.iter().chain(&async_rows).chain(&shared_rows) {
                let key = (row.0.clone(), row.1.clone(), row.2.clone(), row.3);
                if seen.insert(key.clone()) {
                    all_results.push(row.clone());
                }
            }

            println!("Concurrency surface detection for '{}':", symbol);
            println!("  {} concurrency-related callees found:", all_results.len());

            if !all_results.is_empty() {
                println!("  {:<30} {:<12} {:<8} {}", "Symbol", "Kind", "Method", "File");
                for (name, kind, file, line, method) in &all_results {
                    println!("  {:<30} {:<12} {:<8} {}:{}", name, kind, method, file, line);
                }
            } else {
                println!("  No concurrency surface detected (may need --semantic re-index).");
            }
        }
        QueryCommand::MinimalEditScope { symbol } => {
            let syms = store.get_symbols_by_name(symbol).unwrap();
            if syms.is_empty() { eprintln!("'{}' not found.", symbol); std::process::exit(1); }
            let sym = &syms[0];
            let callers = store.get_callers(sym.id, 1).unwrap();
            let callees = store.get_callees(sym.id, 1).unwrap();
            println!("Minimal edit scope for '{}':", symbol);
            println!("  Definition: {}:{}", sym.file_id, sym.line);
            println!("  Direct callers ({}):", callers.len());
            for (_id, n, _, _) in &callers { println!("    - {}", n); }
            println!("  Direct callees ({}):", callees.len());
            for (_id, n, _, _) in &callees { println!("    - {}", n); }
            println!();
            println!("  Total files to review: {}", callers.len() + callees.len() + 1);
        }
        QueryCommand::IssueToCodeLocator { issue } => {
            println!("Locating code related to: {}", issue);
            // Search for symbols matching keywords from the issue
            let config = atree_engine::search::SearchConfig { limit: 20, ..Default::default() };
            let results = atree_engine::search::search(&store, &issue, &config).unwrap();
            if results.is_empty() {
                println!("  No matching symbols found. Try different keywords.");
            } else {
                println!("  {} matching symbol(s):", results.len());
                for hit in &results {
                    println!("  {}  {}  {}:{}  [{:.3}]", hit.name, hit.kind, hit.file_path, hit.line, hit.score);
                }
            }
        }
        QueryCommand::DocsDriftDetector => {
            println!("Documentation drift detection:");
            let conn = store.conn();
            let files = store.get_all_files().unwrap();
            if files.is_empty() {
                println!("  No indexed files.");
                std::process::exit(0);
            }

            // Get all exported symbols
            let mut stmt = match conn.prepare(
                "SELECT s.name, s.kind, s.qualified_name, s.file_path, s.line, s.id
                 FROM symbols s WHERE s.is_exported = 1
                 ORDER BY s.file_path, s.line"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let exported: Vec<(String, String, String, String, i64, i64)> = stmt.query_map([], |row| Ok((
                row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // For each exported symbol, check if its file has uncommitted changes
            // and whether the symbol's signature has changed (heuristic: check if
            // the symbol line is in a changed hunk)
            let repo_path = std::path::Path::new(&files[0].path).parent().map(|p| p.to_path_buf());
            let changed_files: Option<std::collections::HashSet<String>> = repo_path.and_then(|p| atree_engine::detect_git_changes(&p));

            let mut potentially_drifted: Vec<(String, String, String, i64, String)> = Vec::new();
            let mut up_to_date: Vec<(String, String, String, i64)> = Vec::new();

            for (name, kind, _qname, file, line, sym_id) in &exported {
                // Check if this symbol has callees whose count changed (signature drift)
                let callee_count = store.get_callees(*sym_id, 1).unwrap_or_default().len();
                let caller_count = store.get_callers(*sym_id, 1).unwrap_or_default().len();

                // Heuristic: if the file has uncommitted changes, docs may be stale
                let file_changed = changed_files.as_ref()
                    .map(|cf| cf.iter().any(|c| file.contains(c)))
                    .unwrap_or(false);

                // Check if symbol name suggests it should have docs but might not
                // (public functions with complex signatures are higher risk)
                let complexity_score = callee_count + caller_count;
                let needs_docs = *kind == "Function" || *kind == "Method" || *kind == "Class";

                if file_changed && needs_docs {
                    let reason = if complexity_score > 5 {
                        format!("file changed, high complexity ({} callees, {} callers)", callee_count, caller_count)
                    } else {
                        "file changed".to_string()
                    };
                    potentially_drifted.push((name.clone(), kind.clone(), file.clone(), *line, reason));
                } else {
                    up_to_date.push((name.clone(), kind.clone(), file.clone(), *line));
                }
            }

            // Also check for exported symbols with generic names that often lack docs
            let generic_names = ["new", "init", "create", "get", "set", "run", "start", "stop", "handle", "process", "update", "delete", "add", "remove", "find", "list", "load", "save"];
            let mut undocumented_candidates: Vec<&(String, String, String, i64)> = Vec::new();
            for sym in &up_to_date {
                if generic_names.iter().any(|g| sym.0.to_lowercase() == *g) {
                    undocumented_candidates.push(sym);
                }
            }

            println!("  {} exported symbols analyzed", exported.len());
            println!();

            if !potentially_drifted.is_empty() {
                println!("── Potentially drifted docs (file has uncommitted changes) ──");
                for (name, kind, file, line, reason) in &potentially_drifted {
                    println!("  ⚠ {}  {}  {}:{}  ({})", kind, name, file, line, reason);
                }
                println!();
            }

            if !undocumented_candidates.is_empty() {
                println!("── Generic-named public symbols (often under-documented) ──");
                for (name, kind, file, line) in &undocumented_candidates {
                    println!("  ? {}  {}  {}:{}", kind, name, file, line);
                }
                println!();
            }

            println!("── Summary ──");
            println!("  {} symbols with potentially drifted docs", potentially_drifted.len());
            println!("  {} generic-named symbols to review", undocumented_candidates.len());
            println!("  {} symbols appear up-to-date", up_to_date.len() - undocumented_candidates.len());
        }
        QueryCommand::RenameSafetyCheck { symbol_name, new_name } => {
            let syms = store.get_symbols_by_name(&symbol_name).unwrap();
            if syms.is_empty() { eprintln!("'{}' not found.", symbol_name); std::process::exit(1); }
            let sym = &syms[0];
            let callers = store.get_callers(sym.id, 5).unwrap();
            let callees = store.get_callees(sym.id, 5).unwrap();
            let total_refs = callers.len() + callees.len();
            let risk = match total_refs {
                0 => "SAFE (no references)",
                1..=5 => "LOW",
                6..=20 => "MEDIUM",
                _ => "HIGH",
            };
            println!("Rename safety: {} → {}", symbol_name, new_name);
            println!("  Risk: {}", risk);
            println!("  References: {} callers, {} callees (total: {})", callers.len(), callees.len(), total_refs);
            if total_refs > 0 {
                println!("  Affected symbols:");
                for (_id, n, _, _) in &callers { println!("    caller: {}", n); }
                for (_id, n, _, _) in &callees { println!("    callee: {}", n); }
            }
            // Check for name collisions
            let existing = store.get_symbols_by_name(new_name).unwrap();
            if !existing.is_empty() {
                println!("  ⚠ WARNING: '{}' already exists!", new_name);
                for e in &existing { println!("    {}  {}:{}", e.kind, e.file_id, e.line); }
            }
        }
        QueryCommand::DeadCodeCandidates => {
            println!("Dead code candidates (no incoming calls):");
            let conn = store.conn();
            let mut stmt = match conn.prepare(
                "SELECT s.name, s.kind, f.path, s.line FROM symbols s
                 JOIN files f ON f.id = s.file_id
                 WHERE s.kind IN ('Function', 'Method')
                 AND s.is_exported = 0
                 AND s.id NOT IN (SELECT DISTINCT resolved_symbol_id FROM calls WHERE resolved_symbol_id IS NOT NULL)
                 ORDER BY f.path, s.line LIMIT 50"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let rows = stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
            let mut count = 0;
            for row in &rows { let (n, k, f, l) = row; println!("  {}  {}  {}:{}", n, k, f, l); count += 1; }
            if count == 0 { println!("  No dead code candidates found."); }
            else { println!("  {} candidate(s) — verify before removing!", count); }
        }
        QueryCommand::OwnershipHotspots => {
            println!("Ownership hotspots (symbols with most dependents):");
            let conn = store.conn();
            let mut stmt = match conn.prepare(
                "SELECT s.name, s.kind, s.file_path, s.line, COUNT(c.id) as ref_count
                 FROM symbols s
                 LEFT JOIN calls c ON c.resolved_symbol_id = s.id
                 GROUP BY s.id
                 HAVING ref_count > 0
                 ORDER BY ref_count DESC
                 LIMIT 20"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let rows = stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?, row.get::<_, i64>(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
            for row in &rows {
                let (n, k, f, l, c) = row; println!("  {:<30} {:<12} {:<8} {}:{}", n, k, c, f, l);
            }
        }
        QueryCommand::ErrorPathTrace { symbol } => {
            println!("Error path trace for '{}':", symbol);
            let syms = store.get_symbols_by_name(symbol).unwrap();
            if syms.is_empty() { eprintln!("'{}' not found.", symbol); std::process::exit(1); }
            let sym = &syms[0];

            // Comprehensive error-related patterns
            let error_creation = ["error", "err", "panic", "throw", "exception", "fail", "abort", "bail", "fatal", "critical"];
            let error_handling = ["catch", "except", "handle", "recover", "rescue", "trap", "on_error", "on_panic", "guard"];
            let error_propagation = ["unwrap", "expect", "propagate", "bail", "raise", "rethrow", "try", "question_mark", "ok_or", "map_err", "or_else", "and_then"];
            let error_types = ["result", "option", "none", "some", "ok", "err", "error", "failure", "try", "either"];

            let mut visited = std::collections::HashSet::new();
            // Queue: (id, name, depth, path_vec)
            let mut queue: Vec<(i64, String, usize, Vec<(String, String)>)> = Vec::new();
            queue.push((sym.id, sym.name.clone(), 0, vec![(sym.name.clone(), "start".to_string())]));

            let mut error_paths: Vec<(Vec<(String, String)>, String, usize)> = Vec::new(); // (path, category, depth)
            let max_depth = 5;

            while let Some((cur_id, _cur_name, depth, path)) = queue.pop() {
                if depth > max_depth || !visited.insert(cur_id) { continue; }
                let callees = store.get_callees(cur_id, 1).unwrap();
                for (callee_id, callee_name, _conf, _) in &callees {
                    let lower = callee_name.to_lowercase();

                    let mut category: Option<&str> = None;
                    for kw in &error_creation {
                        if lower.contains(kw) { category = Some("error_creation"); break; }
                    }
                    if category.is_none() {
                        for kw in &error_handling {
                            if lower.contains(kw) { category = Some("error_handling"); break; }
                        }
                    }
                    if category.is_none() {
                        for kw in &error_propagation {
                            if lower.contains(kw) { category = Some("error_propagation"); break; }
                        }
                    }
                    if category.is_none() {
                        for kw in &error_types {
                            if lower.contains(kw) { category = Some("error_type"); break; }
                        }
                    }

                    let mut new_path = path.clone();
                    new_path.push((callee_name.clone(), category.unwrap_or("transit").to_string()));

                    if let Some(cat) = category {
                        error_paths.push((new_path.clone(), cat.to_string(), depth + 1));
                    }

                    if category.is_none() && depth < max_depth - 1 {
                        queue.push((*callee_id, callee_name.clone(), depth + 1, new_path));
                    }
                }
            }

            if error_paths.is_empty() {
                println!("  No error paths found within {} hops of '{}'.", max_depth, symbol);
                println!("  This symbol may not interact with error handling.");
            } else {
                // Deduplicate by last symbol
                let mut seen = std::collections::HashSet::new();
                let mut unique_paths: Vec<_> = error_paths.into_iter()
                    .filter(|(path, _, _)| {
                        path.last().map(|(n, _)| seen.insert(n.clone())).unwrap_or(false)
                    })
                .collect();
                unique_paths.sort_by_key(|(_, _, d)| *d);

                println!("  {} error-reaching path(s) found:", unique_paths.len());
                println!();

                // Group by category
                let creations: Vec<_> = unique_paths.iter().filter(|(_, c, _)| c == "error_creation").collect();
                let propagations: Vec<_> = unique_paths.iter().filter(|(_, c, _)| c == "error_propagation").collect();
                let handlings: Vec<_> = unique_paths.iter().filter(|(_, c, _)| c == "error_handling").collect();
                let types: Vec<_> = unique_paths.iter().filter(|(_, c, _)| c == "error_type").collect();

                if !propagations.is_empty() {
                    println!("── Error Propagation (try/?/unwrap/expect) ──");
                    for (path, _, d) in &propagations {
                        let chain: Vec<String> = path.iter().map(|(n, _)| n.clone()).collect();
                        println!("  {} (depth {})", chain.join(" → "), d);
                    }
                }
                if !creations.is_empty() {
                    println!("── Error Creation (panic/throw/return Err) ──");
                    for (path, _, d) in &creations {
                        let chain: Vec<String> = path.iter().map(|(n, _)| n.clone()).collect();
                        println!("  {} (depth {})", chain.join(" → "), d);
                    }
                }
                if !handlings.is_empty() {
                    println!("── Error Handling (catch/except/handle) ──");
                    for (path, _, d) in &handlings {
                        let chain: Vec<String> = path.iter().map(|(n, _)| n.clone()).collect();
                        println!("  {} (depth {})", chain.join(" → "), d);
                    }
                }
                if !types.is_empty() {
                    println!("── Error Type Usage (Result/Option/match) ──");
                    for (path, _, d) in &types {
                        let chain: Vec<String> = path.iter().map(|(n, _)| n.clone()).collect();
                        println!("  {} (depth {})", chain.join(" → "), d);
                    }
                }

                // Summary
                let has_propagation = !propagations.is_empty();
                let has_handling = !handlings.is_empty();
                let has_creation = !creations.is_empty();
                println!();
                match (has_propagation, has_handling, has_creation) {
                    (true, true, _) => println!("  ✅ Errors are propagated AND handled"),
                    (true, false, true) => println!("  ⚠️  Errors are created and propagated but NOT handled here"),
                    (true, false, false) => println!("  ⚠️  Errors are propagated but not handled in this path"),
                    (false, false, false) => println!("  ℹ️  Only error type usage detected (no propagation/creation/handling)"),
                    _ => println!("  ℹ️  Partial error handling detected"),
                }
            }
        }
        QueryCommand::ResourceLifecycleMap { resource } => {
            let syms = store.get_symbols_by_name(&resource).unwrap();
            if syms.is_empty() { eprintln!("'{}' not found.", resource); std::process::exit(1); }
            let sym = &syms[0];
            let file = store.get_file_by_id(sym.file_id).unwrap_or(None);
            let fp = file.as_ref().map(|f| f.path.as_str()).unwrap_or("?");

            let direct_callees = store.get_callees(sym.id, 1).unwrap();
            let deep_callees = store.get_callees(sym.id, 3).unwrap();

            // Lifecycle phase categories
            let creation = ["new", "init", "create", "open", "acquire", "connect", "build", "start", "setup", "begin", "alloc", "allocate"];
            let usage = ["get", "read", "write", "update", "set", "put", "push", "append", "insert", "send", "recv", "fetch", "load", "process", "execute", "run", "call", "invoke", "lock", "hold", "use", "access", "query", "find", "search", "filter", "map", "iter"];
            let cleanup = ["close", "drop", "release", "unlock", "free", "destroy", "cleanup", "clear", "reset", "flush", "shutdown", "stop", "end", "finish", "dealloc", "dispose", "remove", "delete"];

            let mut creation_methods: Vec<(String, String, f64)> = Vec::new();
            let mut usage_methods: Vec<(String, String, f64)> = Vec::new();
            let mut cleanup_methods: Vec<(String, String, f64)> = Vec::new();
            let mut transitive_creation: Vec<(String, String, f64, String)> = Vec::new();
            let mut transitive_cleanup: Vec<(String, String, f64, String)> = Vec::new();

            // Categorize direct callees
            for (_id, name, conf, _) in &direct_callees {
                let lower = name.to_lowercase();
                let mut categorized = false;
                for kw in &creation {
                    if lower.contains(kw) { creation_methods.push((name.clone(), kw.to_string(), *conf)); categorized = true; break; }
                }
                if !categorized {
                    for kw in &cleanup {
                        if lower.contains(kw) { cleanup_methods.push((name.clone(), kw.to_string(), *conf)); categorized = true; break; }
                    }
                }
                if !categorized {
                    for kw in &usage {
                        if lower.contains(kw) { usage_methods.push((name.clone(), kw.to_string(), *conf)); break; }
                    }
                }
            }

            // 2-hop: find lifecycle methods through callees of callees
            for (id, name, conf, _) in &deep_callees {
                if direct_callees.iter().any(|(did, _, _, _)| did == id) { continue; }
                let lower = name.to_lowercase();
                // Find which direct callee leads to this
                let via = direct_callees.iter()
                    .filter(|(dcid, _, _, _)| {
                        store.get_callees(*dcid, 1).unwrap_or_default().iter().any(|(ccid, _, _, _)| ccid == id)
                    })
                    .map(|(_, dcname, _, _)| dcname.clone())
                    .next();
                if let Some(via_name) = via {
                    for kw in &creation {
                        if lower.contains(kw) && !transitive_creation.iter().any(|(n, _, _, _)| n == name) {
                            transitive_creation.push((name.clone(), kw.to_string(), *conf, via_name.clone()));
                            break;
                        }
                    }
                    for kw in &cleanup {
                        if lower.contains(kw) && !transitive_cleanup.iter().any(|(n, _, _, _)| n == name) {
                            transitive_cleanup.push((name.clone(), kw.to_string(), *conf, via_name.clone()));
                            break;
                        }
                    }
                }
            }

            println!("Resource lifecycle for '{}':", resource);
            println!("  {}  |  {}:{}", sym.kind, fp, sym.line);
            println!();

            // Lifecycle completeness check
            let has_creation = !creation_methods.is_empty() || !transitive_creation.is_empty();
            let has_cleanup = !cleanup_methods.is_empty() || !transitive_cleanup.is_empty();
            let lifecycle_status = match (has_creation, has_cleanup) {
                (true, true) => "✅ Complete lifecycle detected",
                (true, false) => "⚠️  Missing cleanup — potential resource leak",
                (false, true) => "⚠️  Missing creation — resource may be passed in",
                (false, false) => "⚠️  No lifecycle methods detected",
            };
            println!("  Status: {}", lifecycle_status);
            println!();

            if !creation_methods.is_empty() {
                println!("── Creation (direct) ──");
                for (name, phase, conf) in &creation_methods {
                    println!("  + {} [{:.2}]  ({})", name, conf, phase);
                }
            }
            if !transitive_creation.is_empty() {
                println!("── Creation (transitive, 2-hop) ──");
                for (name, phase, conf, via) in &transitive_creation {
                    println!("  + {} [{:.2}]  ({}, via {})", name, conf, phase, via);
                }
            }
            if !usage_methods.is_empty() {
                println!("── Usage ──");
                for (name, phase, conf) in &usage_methods {
                    println!("  ◆ {} [{:.2}]  ({})", name, conf, phase);
                }
            }
            if !transitive_cleanup.is_empty() {
                println!("── Cleanup (transitive, 2-hop) ──");
                for (name, phase, conf, via) in &transitive_cleanup {
                    println!("  - {} [{:.2}]  ({}, via {})", name, conf, phase, via);
                }
            }
            if !cleanup_methods.is_empty() {
                println!("── Cleanup (direct) ──");
                for (name, phase, conf) in &cleanup_methods {
                    println!("  - {} [{:.2}]  ({})", name, conf, phase);
                }
            }

            if !has_cleanup {
                println!();
                println!("  ⚠️  WARNING: No cleanup methods detected for '{}'. Potential resource leak.", resource);
            }
        }
        QueryCommand::DependencyCycleDetector => {
            println!("Dependency cycle detection:");
            let conn = store.conn();
            // Find cycles in the call graph (2-hop cycles)
            let mut stmt = match conn.prepare(
                "SELECT DISTINCT s1.name, s2.name
                 FROM calls c1
                 JOIN calls c2 ON c2.resolved_symbol_id = c1.caller_scope_id AND c2.caller_scope_id = c1.resolved_symbol_id
                 JOIN symbols s1 ON s1.id = c1.caller_scope_id
                 JOIN symbols s2 ON s2.id = c1.resolved_symbol_id
                 LIMIT 20"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let rows: Vec<_> = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).unwrap_or_else(|e| { log::error!("Query failed: {}", e); std::process::exit(1); }).collect();            let mut found = false;
            for row in rows { let (a, b) = match row { Ok(r) => r, Err(e) => { log::warn!("Row parse error: {}", e); continue; } }; println!("  {} ↔ {} (cycle)", a, b); found = true; }
            if !found { println!("  No 2-hop cycles detected."); }
        }
        QueryCommand::ResolutionStats => {
            let conn = store.conn();

            // ── Overall call resolution ──
            let total_calls: i64 = conn.query_row(
                "SELECT COUNT(*) FROM calls", [], |r| r.get(0)
            ).unwrap_or(0);
            let resolved_calls: i64 = conn.query_row(
                "SELECT COUNT(*) FROM calls WHERE resolved_symbol_id IS NOT NULL", [], |r| r.get(0)
            ).unwrap_or(0);
            let _unresolved_calls = total_calls - resolved_calls;
            let call_rate = if total_calls > 0 {
                (resolved_calls as f64 / total_calls as f64) * 100.0
            } else { 0.0 };

            // ── Overall import resolution ──
            let total_imports: i64 = conn.query_row(
                "SELECT COUNT(*) FROM imports", [], |r| r.get(0)
            ).unwrap_or(0);
            let resolved_imports: i64 = conn.query_row(
                "SELECT COUNT(*) FROM imports WHERE resolved_file_id IS NOT NULL", [], |r| r.get(0)
            ).unwrap_or(0);
            let _unresolved_imports = total_imports - resolved_imports;
            let import_rate = if total_imports > 0 {
                (resolved_imports as f64 / total_imports as f64) * 100.0
            } else { 0.0 };

            // ── Call resolution per language ──
            let mut stmt = match conn.prepare(
                "SELECT f.language,
                        COUNT(*) as total,
                        SUM(CASE WHEN c.resolved_symbol_id IS NOT NULL THEN 1 ELSE 0 END) as resolved
                 FROM calls c
                 JOIN files f ON f.id = c.file_id
                 GROUP BY f.language
                 ORDER BY total DESC"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let call_by_lang: Vec<(String, i64, i64)> = stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
            // ── Import resolution per language ──
            let mut stmt2 = match conn.prepare(
                "SELECT f.language,
                        COUNT(*) as total,
                        SUM(CASE WHEN i.resolved_file_id IS NOT NULL THEN 1 ELSE 0 END) as resolved
                 FROM imports i
                 JOIN files f ON f.id = i.file_id
                 GROUP BY f.language
                 ORDER BY total DESC"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let import_by_lang: Vec<(String, i64, i64)> = stmt2.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // ── Top unresolved call patterns (callee_name) ──
            let mut stmt3 = match conn.prepare(
                "SELECT c.callee_name, COUNT(*) as count
                 FROM calls c
                 WHERE c.resolved_symbol_id IS NULL
                 GROUP BY c.callee_name
                 ORDER BY count DESC
                 LIMIT 20"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let unresolved_call_patterns: Vec<(String, i64)> = stmt3.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, i64>(1)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // ── Top unresolved import patterns (source) ──
            let mut stmt4 = match conn.prepare(
                "SELECT i.source, COUNT(*) as count
                 FROM imports i
                 WHERE i.resolved_file_id IS NULL
                 GROUP BY i.source
                 ORDER BY count DESC
                 LIMIT 20"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let unresolved_import_patterns: Vec<(String, i64)> = stmt4.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, i64>(1)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // ── Confidence distribution for resolved calls ──
            let mut stmt5 = match conn.prepare(
                "SELECT
                    SUM(CASE WHEN confidence >= 0.9 THEN 1 ELSE 0 END) as high,
                    SUM(CASE WHEN confidence >= 0.5 AND confidence < 0.9 THEN 1 ELSE 0 END) as medium,
                    SUM(CASE WHEN confidence < 0.5 THEN 1 ELSE 0 END) as low,
                    AVG(confidence) as avg_conf
                 FROM calls
                 WHERE resolved_symbol_id IS NOT NULL"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let (high_conf, med_conf, low_conf, avg_conf): (i64, i64, i64, f64) = stmt5.query_row([], |row| Ok((
                row.get::<_, i64>(0).unwrap_or(0),
                row.get::<_, i64>(1).unwrap_or(0),
                row.get::<_, i64>(2).unwrap_or(0),
                row.get::<_, f64>(3).unwrap_or(0.0),
            ))).unwrap_or((0, 0, 0, 0.0));

            // ── Symbols with most unresolved outgoing calls ──
            let mut stmt6 = match conn.prepare(
                "SELECT s.name, s.kind, f.language,
                        COUNT(*) as total_out,
                        SUM(CASE WHEN c.resolved_symbol_id IS NULL THEN 1 ELSE 0 END) as unresolved_out
                 FROM calls c
                 JOIN symbols s ON s.id = c.caller_scope_id
                 JOIN files f ON f.id = s.file_id
                 GROUP BY c.caller_scope_id
                 HAVING unresolved_out > 0
                 ORDER BY unresolved_out DESC
                 LIMIT 15"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let worst_symbols: Vec<(String, String, String, i64, i64)> = stmt6.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?, row.get::<_, i64>(4)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();

            // ═══════════════════════════════════════════════════════════════
            // Output
            // ═══════════════════════════════════════════════════════════════
            println!("═══ Resolution Quality Report ═══");
            println!();

            // Summary
            println!("── Overall ──");
            println!("  Calls:    {}/{} resolved ({:.1}%)", resolved_calls, total_calls, call_rate);
            println!("  Imports:  {}/{} resolved ({:.1}%)", resolved_imports, total_imports, import_rate);
            println!();

            // Confidence distribution
            if resolved_calls > 0 {
                println!("── Call Confidence Distribution ──");
                println!("  High (≥0.9):   {}  |  Medium (0.5-0.9): {}  |  Low (<0.5): {}", high_conf, med_conf, low_conf);
                println!("  Average confidence: {:.3}", avg_conf);
                println!();
            }

            // Per-language call resolution
            if !call_by_lang.is_empty() {
                println!("── Call Resolution by Language ──");
                println!("  {:<16} {:>8} {:>8} {:>8}", "Language", "Total", "Resolved", "Rate");
                for (lang, total, resolved) in &call_by_lang {
                    let rate = if *total > 0 { (*resolved as f64 / *total as f64) * 100.0 } else { 0.0 };
                    let indicator = if rate >= 80.0 { "✅" } else if rate >= 50.0 { "⚠️" } else { "❌" };
                    println!("  {:<16} {:>8} {:>8} {:>7.1}% {}", lang, total, resolved, rate, indicator);
                }
                println!();
            }

            // Per-language import resolution
            if !import_by_lang.is_empty() {
                println!("── Import Resolution by Language ──");
                println!("  {:<16} {:>8} {:>8} {:>8}", "Language", "Total", "Resolved", "Rate");
                for (lang, total, resolved) in &import_by_lang {
                    let rate = if *total > 0 { (*resolved as f64 / *total as f64) * 100.0 } else { 0.0 };
                    let indicator = if rate >= 80.0 { "✅" } else if rate >= 50.0 { "⚠️" } else { "❌" };
                    println!("  {:<16} {:>8} {:>8} {:>7.1}% {}", lang, total, resolved, rate, indicator);
                }
                println!();
            }

            // Top unresolved call patterns
            if !unresolved_call_patterns.is_empty() {
                println!("── Top Unresolved Call Patterns (callee_name) ──");
                for (name, count) in &unresolved_call_patterns {
                    println!("  {:<40}  {} occurrences", name, count);
                }
                println!();
            }

            // Top unresolved import patterns
            if !unresolved_import_patterns.is_empty() {
                println!("── Top Unresolved Import Patterns (source) ──");
                for (source, count) in &unresolved_import_patterns {
                    println!("  {:<40}  {} occurrences", source, count);
                }
                println!();
            }

            // Symbols with worst resolution
            if !worst_symbols.is_empty() {
                println!("── Symbols with Most Unresolved Outgoing Calls ──");
                println!("  {:<30} {:<10} {:<12} {:>6} {:>6}", "Symbol", "Kind", "Language", "Total", "Unres.");
                for (name, kind, lang, total, unresolved) in &worst_symbols {
                    println!("  {:<30} {:<10} {:<12} {:>6} {:>6}", name, kind, lang, total, unresolved);
                }
                println!();
            }

            // Recommendations
            println!("── Recommendations ──");
            if call_rate < 50.0 {
                println!("  ❌ Call resolution is low ({:.1}%). Focus on improving scope resolution.", call_rate);
                println!("     Check the top unresolved patterns above — these are the biggest gaps.");
            } else if call_rate < 80.0 {
                println!("  ⚠️  Call resolution is moderate ({:.1}%). Room for improvement.", call_rate);
                println!("     Look at languages with the lowest rates above.");
            } else {
                println!("  ✅ Call resolution is strong ({:.1}%).", call_rate);
            }
            if import_rate < 50.0 {
                println!("  ❌ Import resolution is low ({:.1}%). Check import resolver for affected languages.", import_rate);
            } else if import_rate < 80.0 {
                println!("  ⚠️  Import resolution is moderate ({:.1}%).", import_rate);
            } else {
                println!("  ✅ Import resolution is strong ({:.1}%).", import_rate);
            }
            if avg_conf < 0.5 && resolved_calls > 0 {
                println!("  ⚠️  Average call confidence is low ({:.3}). Many resolutions may be incorrect.", avg_conf);
            }
        }
        QueryCommand::FindUncoveredSymbols => {
            // Unlike dead_code_candidates (which only checks for no callers),
            // this also checks whether symbols are referenced from test files.
            // A symbol is "covered" if it has callers OR is referenced by a test.
            println!("Uncovered symbols (no callers, not exported, not tested):");
            let conn = store.conn();
            let mut stmt = match conn.prepare(
                "SELECT s.name, s.kind, s.file_path, s.line FROM symbols s
                 WHERE s.is_exported = 0
                 AND s.kind IN ('Function', 'Method', 'Class')
                 AND s.id NOT IN (SELECT DISTINCT resolved_symbol_id FROM calls WHERE resolved_symbol_id IS NOT NULL)
                 AND s.id NOT IN (
                     -- Symbols referenced from test files
                     SELECT DISTINCT c2.resolved_symbol_id
                     FROM calls c2
                     JOIN files f ON f.id = c2.file_id
                     WHERE c2.resolved_symbol_id IS NOT NULL
                     AND (f.path LIKE '%test%' OR f.path LIKE '%spec%' OR f.path LIKE '%__tests__%')
                 )
                 ORDER BY s.file_path, s.line LIMIT 50"
            ) { Ok(s) => s, Err(e) => { log::error!("DB prepare failed: {}", e); std::process::exit(1); } };
            let rows = stmt.query_map([], |row| Ok((
                row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                row.get::<_, String>(2)?, row.get::<_, i64>(3)?,
            ))).unwrap().collect::<Result<Vec<_>, _>>().unwrap_or_default();
            let mut count = 0;
            for row in &rows { let (n, k, f, l) = row; println!("  {}  {}  {}:{}", n, k, f, l); count += 1; }
            if count == 0 { println!("  All non-exported symbols have callers."); }
            else { println!("  {} uncovered symbol(s)", count); }
        }
        QueryCommand::EvidencePath { query, max_depth, beam_width, max_evidence } => {
            // Build in-memory KnowledgeGraph from the store.
            let graph = atree_engine::graph::KnowledgeGraph::from_store(&store).unwrap_or_else(|e| {
                eprintln!("Error building graph from store: {}", e);
                std::process::exit(1);
            });

            let config = atree_engine::evidence::EvidenceConfig {
                max_seeds: 10,
                beam_width: *beam_width,
                max_depth: *max_depth,
                max_evidence: *max_evidence,
                ..Default::default()
            };

            let paths = atree_engine::evidence::find_evidence_paths(&store, &graph, &query, &config);

            if paths.is_empty() {
                eprintln!("No evidence paths found for '{}'", query);
                std::process::exit(0);
            }

            println!("Evidence paths for '{}' ({} paths found):", query, paths.len());
            println!();
            for (i, path) in paths.iter().enumerate() {
                println!("── Path {}  confidence={:.3}  cost={:.2} ──", i + 1, path.confidence, path.cost);
                println!("  {}", path.explanation);
                for (j, step) in path.steps.iter().enumerate() {
                    let via = match &step.via {
                        atree_engine::evidence::EvidenceVia::TextMatch => "text",
                        atree_engine::evidence::EvidenceVia::CallChain => "calls",
                        atree_engine::evidence::EvidenceVia::Inheritance => "inherit",
                        atree_engine::evidence::EvidenceVia::ImportChain => "import",
                        atree_engine::evidence::EvidenceVia::DataFlow => "data",
                        atree_engine::evidence::EvidenceVia::Containment => "contains",
                        atree_engine::evidence::EvidenceVia::Semantic => "semantic",
                    };
                    println!("  {}. {}  {}:{}  [{:.3}] via {}",
                        j + 1, step.label, step.file_path, step.line, step.relevance, via);
                }
                println!();
            }
        }
        QueryCommand::FileHistory { path, limit } => {
            let history = atree_engine::git_history::get_file_history(&store.conn(), path, *limit)
                .unwrap_or_else(|e| {
                    eprintln!("Error querying file history: {}", e);
                    std::process::exit(1);
                });
            if history.is_empty() {
                println!("No commit history found for '{}' (run git history extraction during indexing)", path);
                std::process::exit(0);
            }
            println!("Commit history for '{}' ({} commits):", path, history.len());
            for (i, c) in history.iter().enumerate() {
                let ts = chrono::DateTime::from_timestamp(c.timestamp, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| c.timestamp.to_string());
                println!("  {}. {}  {}  <{}>  {}",
                    i + 1, &c.hash[..8], ts, c.author_name, c.message);
            }
        }
        QueryCommand::Blame { path } => {
            // Extract blame on-demand for this specific file
            let blame = atree_engine::git_history::extract_blame_for_file(&args.root, path)
                .unwrap_or_else(|e| {
                    eprintln!("Error extracting blame: {}", e);
                    std::process::exit(1);
                });
            if blame.is_empty() {
                println!("No blame data for '{}' (file may not be tracked by git)", path);
                std::process::exit(0);
            }
            println!("Git blame for '{}' ({} lines):", path, blame.len());
            // Group consecutive lines by author for compact output
            let mut current_author = String::new();
            let mut current_hash = String::new();
            let mut current_ts: i64 = 0;
            let mut start_line = 0usize;
            for (i, line) in blame.iter().enumerate() {
                if line.author_name != current_author || line.commit_hash != current_hash {
                    if i > 0 {
                        let ts_str = chrono::DateTime::from_timestamp(current_ts, 0)
                            .map(|dt| dt.format("%Y-%m-%d").to_string())
                            .unwrap_or_default();
                        if start_line + 1 == i {
                            println!("  {:>5}  {}  {}  {}", start_line, &current_hash[..8], ts_str, current_author);
                        } else {
                            println!("  {:>5} - {:>5}  {}  {}  {}", start_line, i - 1, &current_hash[..8], ts_str, current_author);
                        }
                    }
                    current_author = line.author_name.clone();
                    current_hash = line.commit_hash.clone();
                    current_ts = line.timestamp;
                    start_line = line.line_number;
                }
            }
            // Print last group
            if !current_author.is_empty() {
                let ts_str = chrono::DateTime::from_timestamp(current_ts, 0)
                    .map(|dt| dt.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                let last_line = match blame.last() { Some(b) => b.line_number, None => { log::error!("No blame data available"); std::process::exit(1); } };                if start_line == last_line {
                    println!("  {:>5}  {}  {}  {}", start_line, &current_hash[..8], ts_str, current_author);
                } else {
                    println!("  {:>5} - {:>5}  {}  {}  {}", start_line, last_line, &current_hash[..8], ts_str, current_author);
                }
            }
        }
        QueryCommand::TopAuthors { limit } => {
            let authors = atree_engine::git_history::get_top_authors(&store.conn(), *limit)
                .unwrap_or_else(|e| {
                    eprintln!("Error querying authors: {}", e);
                    std::process::exit(1);
                });
            if authors.is_empty() {
                println!("No author data (run git history extraction during indexing)");
                std::process::exit(0);
            }
            println!("Top {} authors:", authors.len());
            for (i, a) in authors.iter().enumerate() {
                let first = chrono::DateTime::from_timestamp(a.first_commit, 0)
                    .map(|dt| dt.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                let last = chrono::DateTime::from_timestamp(a.last_commit, 0)
                    .map(|dt| dt.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                println!("  {:>3}. {:<30}  {:>5} commits  +{}/-{} lines  {} - {}",
                    i + 1, a.name, a.commit_count, a.lines_added, a.lines_removed, first, last);
            }
        }
        QueryCommand::ChangeHotspots { limit } => {
            let hotspots = atree_engine::git_history::get_change_frequency(&store.conn(), *limit)
                .unwrap_or_else(|e| {
                    eprintln!("Error querying change frequency: {}", e);
                    std::process::exit(1);
                });
            if hotspots.is_empty() {
                println!("No change frequency data (run git history extraction during indexing)");
                std::process::exit(0);
            }
            println!("Top {} most frequently changed files:", hotspots.len());
            for (i, (path, commits, changes)) in hotspots.iter().enumerate() {
                println!("  {:>3}. {:<60}  {:>4} commits  {:>6} lines changed",
                    i + 1, path, commits, changes);
            }
        }
        QueryCommand::CoChange { path, limit } => {
            let cochanges = atree_engine::git_history::get_cochange_frequency(&store.conn(), path, *limit)
                .unwrap_or_else(|e| {
                    eprintln!("Error querying co-change frequency: {}", e);
                    std::process::exit(1);
                });
            if cochanges.is_empty() {
                println!("No co-change data for '{}' (run git history extraction during indexing)", path);
                std::process::exit(0);
            }
            println!("Files that co-change with '{}' ({} files):", path, cochanges.len());
            for (i, (other_path, count)) in cochanges.iter().enumerate() {
                println!("  {:>3}. {:<60}  {} co-commits", i + 1, other_path, count);
            }
        }
        QueryCommand::GitStats => {
            let commit_count: i64 = store.conn().query_row("SELECT COUNT(*) FROM commits", [], |r| r.get(0)).unwrap_or(0);
            let author_count: i64 = store.conn().query_row("SELECT COUNT(*) FROM authors", [], |r| r.get(0)).unwrap_or(0);
            let file_commit_count: i64 = store.conn().query_row("SELECT COUNT(*) FROM file_commits", [], |r| r.get(0)).unwrap_or(0);
            let blame_count: i64 = store.conn().query_row("SELECT COUNT(*) FROM blame_lines", [], |r| r.get(0)).unwrap_or(0);
            println!("Git history statistics:");
            println!("  Commits:          {}", commit_count);
            println!("  Authors:          {}", author_count);
            println!("  File-commit links:{}", file_commit_count);
            println!("  Blame lines:      {}", blame_count);
            if commit_count == 0 {
                println!("\n  No git history extracted. Run indexing with git history enabled.");
            }
        }
        QueryCommand::SymbolOwnership { symbol } => {
            let ownership = atree_engine::git_history::get_symbol_ownership(&store.conn(), symbol)
                .unwrap_or_else(|e| {
                    eprintln!("Error querying symbol ownership: {}", e);
                    std::process::exit(1);
                });
            if ownership.is_empty() {
                println!("No ownership data for '{}' (not found or no git history)", symbol);
                std::process::exit(0);
            }
            println!("Ownership for '{}':", symbol);
            for (_sym, author, last_ts, num_authors, first_hash) in &ownership {
                let ts_str = chrono::DateTime::from_timestamp(*last_ts, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                println!("  last modified: {} by {}  ({} authors, first commit: {})",
                    ts_str, author, num_authors, &first_hash[..8]);
            }
        }
        QueryCommand::ChangeRisk { path } => {
            let risk = atree_engine::git_history::get_change_risk(&store.conn(), path)
                .unwrap_or_else(|e| {
                    eprintln!("Error querying change risk: {}", e);
                    std::process::exit(1);
                });
            if risk.is_empty() {
                println!("No change risk data for '{}' (no git history)", path);
                std::process::exit(0);
            }
            for (file, commits, authors, days_since, score) in &risk {
                let risk_level = if *score > 10.0 { "HIGH" } else if *score > 3.0 { "MEDIUM" } else { "LOW" };
                println!("Change risk for '{}': {} (score: {:.1})", file, risk_level, score);
                println!("  {} commits by {} authors, last changed {} days ago", commits, authors, days_since);
            }
        }
        QueryCommand::FindExperts { path } => {
            let experts = atree_engine::git_history::find_experts(&store.conn(), path, 5)
                .unwrap_or_else(|e| {
                    eprintln!("Error finding experts: {}", e);
                    std::process::exit(1);
                });
            if experts.is_empty() {
                println!("No expert data for '{}' (no git history)", path);
                std::process::exit(0);
            }
            println!("Experts for '{}' (best reviewers):", path);
            for (i, (name, commits, lines, last_active)) in experts.iter().enumerate() {
                let last_str = chrono::DateTime::from_timestamp(*last_active, 0)
                    .map(|dt| dt.format("%Y-%m-%d").to_string())
                    .unwrap_or_default();
                println!("  {:>3}. {:<30}  {:>4} commits  {:>6} lines  last: {}",
                    i + 1, name, commits, lines, last_str);
            }
        }
        QueryCommand::SmartCoChange { symbol, limit } => {
            let cochanges = atree_engine::git_history::get_smart_cochange(&store.conn(), symbol, *limit)
                .unwrap_or_else(|e| {
                    eprintln!("Error querying smart co-change: {}", e);
                    std::process::exit(1);
                });
            if cochanges.is_empty() {
                println!("No co-change data for '{}'", symbol);
                std::process::exit(0);
            }
            println!("Smart co-change for '{}':", symbol);
            for (file, count, signal) in &cochanges {
                println!("  {:<60}  {:>4} co-commits  [{}]", file, count, signal);
            }
        }
    }
    std::process::exit(0);
}

fn main() {
    let args = parse_args();

    // Handle MCP server mode — start the MCP server instead of scanning/querying.
    if args.mcp_server {
        #[cfg(feature = "mcp")]
        {
            let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
            rt.block_on(atree_engine::mcp::run_mcp_server(
                Some("atree".to_string()),
                args.db_path,
            )).expect("MCP server failed");
            return;
        }
        #[cfg(not(feature = "mcp"))]
        {
            eprintln!("Error: mcp-server requires the 'mcp' feature. Rebuild with: cargo build --features mcp -p atree\nRun 'atree --help' for usage information.");
            std::process::exit(2);
        }
    }

    // Handle group scan (cross-repo) before single-repo scan.
    if let Some(ref group_args) = args.group {
        let config = GroupConfig {
            repos: group_args.repos.clone(),
            db_path: group_args.db_path.clone(),
            threads: group_args.threads,
            semantic: group_args.semantic,
            graph_phases: args.graph_phases,
            ..Default::default()
        };
        eprintln!("[INFO] Scanning {} repos into group index at {}...",
            config.repos.len(), config.db_path.display());
        let start = Instant::now();
        match build_graph_group(&config) {
            Ok(results) => {
                let total_files: usize = results.iter().map(|r| r.parsed_files.len()).sum();
                let total_symbols: usize = results.iter().map(|r| {
                    r.parsed_files.iter().map(|f| f.symbols.len()).sum::<usize>()
                }).sum();
                eprintln!("[OK] Group scan complete in {:.2?}: {} files, {} symbols across {} repos",
                    start.elapsed(), total_files, total_symbols, results.len());
            }
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

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

    // Handle query subcommands — no scan needed, just query the existing DB.
    if let Some(ref cmd) = args.query {
        execute_query(cmd, &args, None);
    }

    let start_time = Instant::now();
    let threads = resolve_threads(&args.threads);
    let (max_depth, max_nodes, mem_capped) = resolve_caps(&args);

    // Auto-persist semantic index to .atree/index.sqlite when --semantic is
    // used but --db is not explicitly provided.  Path is rooted at --root so
    // that scanning different repos never collides on a single DB file.
    let db_path = args.db_path.clone().or_else(|| {
        if args.semantic {
            let p = args.root.join(".atree/index.sqlite");
            if !args.json { eprintln!("[INFO] Auto-persisting index to {}", p.display()); }
            Some(p)
        } else {
            None
        }
    });
    let opts = ScanOptions {
        root: args.root.clone(),
        max_depth,
        max_nodes,
        include_files: args.include_files || args.semantic,
        threads,
        tree_mode: args.tree,
        semantic: args.semantic,
        db_path,
        incremental: args.incremental,
        embeddings: args.embeddings,
        repo_label: None,
        graph_phases: args.graph_phases,
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

    let scan = if opts.incremental {
        match build_graph_incremental(&opts) {
            Ok((s, inc)) => {
                if !args.json {
                    eprintln!(
                        "[INC] Incremental scan: {} added, {} updated, {} unchanged, {} removed",
                        inc.files_added, inc.files_updated, inc.files_unchanged, inc.files_removed
                    );
                }
                s
            }
            Err(e) => {
                eprintln!("Error during incremental scan: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        match build_graph(&opts) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error scanning directory: {}", e);
                std::process::exit(1);
            }
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
    scan: &atree_engine::ScanResult,
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

fn print_summary_box(args: &Args, threads: usize, elapsed: std::time::Duration, stats: &atree_engine::Stats, semantic_files: usize, semantic_defs: usize, semantic_calls: usize, semantic_resolved: usize, semantic_unresolved: usize, semantic_nodes: usize, semantic_edges: usize) {
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
