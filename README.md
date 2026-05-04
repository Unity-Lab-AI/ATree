# file_system_a_star

**Version 0.6 ALPHA** — by [UnityAILab](#team) — released 2026-05-04

A production-grade parallel filesystem analysis and A\*-pathfinding library, written in Rust. Walks a directory tree using lock-free work-stealing concurrency, builds a graph, finds optimal navigation paths between any two nodes, and emits human-readable trees, Graphviz DOT, or a deterministic JSON document for inter-process integration with consumers in any language.

## Description

`file_system_a_star` performs high-throughput parallel directory enumeration and computes shortest paths across the resulting graph using the A\* algorithm with an admissible depth-difference heuristic. It is engineered for production environments: memory-aware resource defaults, sanitized output (no terminal-injection through hostile filenames), zero `unsafe` code, deterministic JSON keyed for diff-friendliness, and a stable JSON Schema (v1) for downstream tooling.

### Use cases

- **AI-agent infrastructure** — rapid project-context indexing for code-aware LLM applications, retrieval-augmented generation pipelines, and knowledge-base construction
- **Build and deployment tooling** — dependency-relationship mapping, incremental-build optimization, and deployment artifact auditing
- **Filesystem auditing and forensics** — structural analysis across millions of files, security investigations, change detection
- **Visualization** — terminal-rendered trees for development, Graphviz DOT for documentation, JSON for programmatic consumers
- **Inter-process integration** — emit structured filesystem snapshots from one process and consume them in any language with a JSON parser (Node.js, Python, Go, etc.)

## What it does

- **Scans** a directory tree using a lock-free work-stealing parallel walker (`crossbeam-deque`).
- **Builds a graph**: nodes are folders/files, edges are parent↔child relationships.
- **Pathfinds** between two nodes using A\* with a depth-difference heuristic. Reports A\*'s expansion count vs blind BFS as an efficiency comparison.
- **Renders** the result as a colored Unicode/ASCII tree, a Graphviz DOT file, or a JSON document conforming to [`docs/schema.json`](docs/schema.json).

## Why use it

| Tool | What you can replace it with |
|---|---|
| `tree -L N` | `file_system_a_star -L N --tree` (faster on large trees) |
| `find PATH -maxdepth N -type f` | `file_system_a_star --path PATH --maxdepth N --files --tree` |
| Custom tree-walker scripts | embed it as a Rust library, or call the CLI with `--json` |

It's faster than `tree` on the same workload, and the JSON output is intended for AI-agent / knowledge-base / build-tooling pipelines that need structured filesystem maps.

## Quick start

```bash
cargo build --release
./target/release/file_system_a_star --help
```

### Common invocations

```bash
# tree-style overview (level 3, current dir)
./target/release/file_system_a_star -L 3

# scan everything fast (skip per-file stat, no caps)
./target/release/file_system_a_star --path /home/me --tree --no-limit --files

# A* path between two nodes, JSON to stdout, status to stderr
./target/release/file_system_a_star --dir . --from src --to Cargo.toml --files --json > report.json

# pipe-friendly: only data on stdout
./target/release/file_system_a_star --dir . --no-color > tree.txt
```

Run with `--help` to see the full flag list including aliases (`--path`/`--dir`/`--directory`, `--from`/`--to`, `-L`/`--maxdepth`/`--depth`, `--jobs`/`--workers`, `--fast`/`--no-stat`, etc.).

## Library use (Rust)

```rust
use file_system_a_star::{build_graph, astar, compute_depths, ScanOptions, half_cores};
use std::path::PathBuf;

let opts = ScanOptions {
    root: PathBuf::from("."),
    max_depth: 10,
    max_nodes: 100_000,
    include_files: true,
    threads: half_cores(),
    tree_mode: true,
};
let scan = build_graph(&opts).unwrap();
let depths = compute_depths(&scan.adj, &scan.root_name);
if let Some((path, expanded)) = astar(&scan.adj, &scan.root_name, "src/main.rs", &depths) {
    println!("Found path of {} hops, A* expanded {} nodes", path.len() - 1, expanded);
}
```

## JSON integration

`--json` emits a single JSON document on stdout (status messages still go to stderr). The full machine-readable JSON Schema (Draft 7) is in [`docs/schema.json`](docs/schema.json) — validate any output with `ajv` (Node) or `jsonschema` (Python).

Schema overview:

```jsonc
{
  "schema_version": 1,         // pin this in your consumer
  "version": "0.1.0",          // binary version (changes more often)
  "root": "/abs/path/to/scanned/dir",
  "root_name": "dir",
  "elapsed_ms": 12.34,
  "threads": 6,
  "options": {
    "max_depth": 4,            // null when --no-limit
    "max_nodes": 150,          // null when --no-limit (or unbounded)
    "include_files": true,
    "tree_mode": false
  },
  "stats": {
    "total_nodes": 1234, "folders": 100, "files": 1100, "symlinks": 30,
    "executables": 50, "hidden": 5, "total_size_bytes": 12345678
  },
  "truncated": false,
  "depths": { "node_key": 0, "...": 1 },
  "nodes": {
    "src/main.rs": {
      "is_dir": false, "is_symlink": false, "is_hidden": false,
      "is_exec": false, "mode": 33188, "size": 12345, "name": "main.rs"
    }
  },
  "edges": { "src": ["src/main.rs"], "src/main.rs": ["src"] },
  "path": {
    "start": "src", "goal": "src/main.rs", "hops": 1,
    "nodes": ["src", "src/main.rs"],
    "astar_expanded": 1, "bfs_expanded": 2, "efficiency_pct": 50.0
  }
}
```

Keys are sorted (`BTreeMap`), so the output is deterministic and diff-able.

### Calling from other languages

```js
// Node.js
const { execFileSync } = require('child_process');
const report = JSON.parse(execFileSync(
  'file_system_a_star',
  ['--root', '/project', '--tree', '--no-limit', '--json']
));
```

```python
# Python
import json, subprocess
report = json.loads(subprocess.check_output(
    ['file_system_a_star', '--root', project_path, '--tree', '--no-limit', '--json']
))
```

## Performance

Numbers from a 50,000-node scan of `/usr` on warm cache, 12-core machine:

| Threads | Time |
|---|---|
| 1 | 118 ms |
| 4 | 37 ms |
| 12 | **28 ms** |

`--tree` mode (skipping per-file `stat`) reduces cold-cache scan times by ~3–5× by saving one syscall per file.

## Resource defaults

- **Threads**: defaults to half of available cores (`half_cores()`). Pass `--threads all` to use every core, or `--threads N` for an explicit count.
- **Memory** (when `--no-limit` is set): a soft cap at ~half of available RAM is applied automatically (Linux, via `/proc/meminfo`). Use `--no-mem-cap` to disable. On non-Linux platforms there's no cap.

## Security

Filenames are sanitized at scan time: control characters (including ANSI escape sequences) are replaced with `?` before being stored or rendered. Malicious filenames cannot inject terminal escapes into your shell session via the tree output.

## Building

```bash
cargo build --release    # binary at target/release/file_system_a_star
cargo test --release     # run all unit + integration tests
```

Release profile uses LTO + single codegen unit + `panic = "abort"` for maximum runtime speed at the cost of slightly slower builds.

## Dependencies

- `mimalloc` — fast multi-threaded allocator
- `rustc-hash` — `FxHashMap` for non-cryptographic hashing
- `crossbeam-deque` — lock-free work-stealing queues
- `serde` + `serde_json` — JSON output

## Team

`file_system_a_star` is developed by **UnityAILab**, a sovereign, independent research and engineering team:

- **Sponge** — `sponge@unityailab.com`
- **Alfreddo**
- **Gee**
- **Red**

Contact: `contact@unityailab.com`

### Notice

UnityAILab is not affiliated with, endorsed by, or connected in any way to Unity Technologies, Unity Software Inc., the Unity game engine, or any of their subsidiaries, products, or trademarks. The "Unity" in our name refers to the unity of the AI and systems-research disciplines we pursue. See [NOTICE](NOTICE) for the full disclaimer.

## License

MIT — see [LICENSE](LICENSE) and [NOTICE](NOTICE). You may use, modify, and redistribute this software freely, including in derivative works, provided that:

1. The original copyright notice and the contents of `LICENSE` and `NOTICE` are retained.
2. Attribution to **UnityAILab** and its contributors (Sponge, Alfreddo, Gee, Red) is preserved in derivative works.
