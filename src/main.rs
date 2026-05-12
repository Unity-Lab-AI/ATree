use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;
use rustc_hash::{FxHashSet, FxHashMap};

use atree::{
    astar, build_graph, compute_depths, human_size, ScanOptions, PathReport,
};

#[derive(Debug)]
enum ThreadSpec {
    All,
    Half,
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
    semantic: bool,
    no_mem_cap: bool,
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
            max_nodes: 1000,
            include_files: false,
            ascii: false,
            dot: false,
            no_color: false,
            threads: ThreadSpec::Half,
            tree: false,
            no_limit: false,
            semantic: false,
            no_mem_cap: false,
            json: false,
            print_schema: false,
        }
    }
}

fn parse_args() -> Args {
    let mut args = Args::default();
    let cli_args: Vec<String> = std::env::args().collect();

    fn take_value(args: &[String], i: &mut usize) -> String {
        *i += 1;
        args.get(*i).cloned().unwrap_or_else(|| ".".to_string())
    }

    let mut i = 1;
    while i < cli_args.len() {
        match cli_args[i].as_str() {
            "--root" | "-r" => args.root = PathBuf::from(take_value(&cli_args, &mut i)),
            "--start" | "-s" => args.start = Some(take_value(&cli_args, &mut i)),
            "--goal" | "-g" => args.goal = Some(take_value(&cli_args, &mut i)),
            "--files" | "-f" => args.include_files = true,
            "--ascii" => args.ascii = true,
            "--dot" => args.dot = true,
            "--no-color" => args.no_color = true,
            "--tree" | "-t" => args.tree = true,
            "--no-limit" => args.no_limit = true,
            "--semantic" => args.semantic = true,
            "--no-mem-cap" => args.no_mem_cap = true,
            "--json" => args.json = true,
            "--print-schema" => args.print_schema = true,
            _ => {}
        }
        i += 1;
    }

    if args.no_limit {
        args.max_nodes = usize::MAX;
        args.max_depth = usize::MAX;
    }

    args
}

fn main() {
    let args = parse_args();

    if args.print_schema {
        println!("{}", atree::SCHEMA_JSON);
        return;
    }

    let n = match args.threads {
        ThreadSpec::All => atree::all_cores(),
        ThreadSpec::Half => atree::half_cores(),
        ThreadSpec::Explicit(n) => n,
    };

    let mut opts = ScanOptions {
        root: args.root.clone(),
        max_depth: args.max_depth,
        max_nodes: args.max_nodes,
        include_files: args.include_files,
        tree_mode: args.tree,
        semantic: args.semantic,
        threads: n,
    };

    let start_time = Instant::now();
    let scan = match build_graph(&opts) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };
    let elapsed_ms = start_time.elapsed().as_secs_f64() * 1000.0;

    let root_name = scan.root_name.clone();
    let start_node = args.start.unwrap_or_else(|| root_name.clone());

    let goal_node = args.goal.or_else(|| {
        scan.meta
            .iter()
            .filter(|(_, m)| !m.is_dir)
            .map(|(path, _)| path.clone())
            .next()
    });

    if args.semantic {
        let mut symbols = atree::resolver::SymbolTable::new();
        for file in &scan.parsed_files {
            symbols.index_file(file);
        }
    }

    let depths = compute_depths(&scan.adj, &root_name);
    
    let path_report = if let Some(ref goal) = goal_node {
        if let Some((nodes, astar_expanded)) = astar(&scan.adj, &start_node, goal, &depths) {
            let bfs_expanded = depths.get(goal).cloned().unwrap_or(0) as usize;
            let efficiency_pct = (1.0 - (astar_expanded as f64 / (scan.adj.len() as f64))) * 100.0;
            Some(PathReport {
                start: start_node.clone(),
                goal: goal.clone(),
                hops: nodes.len().saturating_sub(1),
                nodes,
                astar_expanded,
                bfs_expanded,
                efficiency_pct,
            })
        } else {
            None
        }
    } else {
        None
    };

    if args.json {
        let report = atree::build_json_report(
            &scan,
            &opts,
            &depths,
            path_report,
            elapsed_ms,
        );
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        let mut out = io::stdout();
        let path_set: FxHashSet<String> = path_report.as_ref().map(|pr| pr.nodes.iter().cloned().collect()).unwrap_or_else(FxHashSet::default);
        let _ = atree::print_tree(
            &mut out,
            &scan.adj,
            &scan.meta,
            &root_name,
            &depths,
            args.ascii,
            args.no_color,
            &path_set,
        );
        
        println!("\nSUMMARY");
        println!("Scan time      : {:.2}ms", elapsed_ms);
        println!("Total nodes    : {}", scan.stats.total_nodes);
        if args.semantic {
            let total_defs: usize = scan.parsed_files.iter().map(|f| f.defs.len()).sum();
            let total_calls: usize = scan.parsed_files.iter().map(|f| f.calls.len()).sum();
            println!("Semantic       : {} symbols, {} calls across {} files", total_defs, total_calls, scan.parsed_files.len());
        }
    }
}
