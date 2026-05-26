# atree

> Production-grade parallel filesystem analysis and A\* pathfinding, written in Rust.

| | |
|---|---|
| **Version** | `0.7.0-alpha` |
| **Status** | Alpha — semantic engine v2 with persistent SQLite indexing, type-aware extraction, and query API. |
| **Authors** | UnityAILab — Sponge, Alfreddo, Gee, Red, B-A-M-N |
| **License** | MIT (see [LICENSE](LICENSE) and [NOTICE](NOTICE)) |
| **Contact** | `contact@unityailab.com` |

`atree` performs high-throughput parallel directory enumeration and computes shortest paths across the resulting graph using the A\* algorithm with an admissible depth-difference heuristic. It is available both as a Rust library and as a single-binary CLI with structured JSON output for integration with consumers in any language.

## Contents

- [Overview](#overview)
- [Installation](#installation)
- [Quick start](#quick-start)
- [JSON output](#json-output)
- [Performance](#performance)
- [Architecture](#architecture)
- [Resource defaults](#resource-defaults)
- [Security](#security)
- [Exit codes](#exit-codes)
- [Building and testing](#building-and-testing)
- [Dependencies](#dependencies)
- [Project information](#project-information)

## Overview

The tool walks a directory tree using a lock-free work-stealing parallel scanner, builds an undirected graph (nodes are files and folders, edges are parent–child relationships), and supports:

- Optimal-path search via A\* between any two nodes, with blind-BFS comparison for efficiency reporting
- Three output formats: terminal-rendered tree (Unicode or ASCII, color-aware), Graphviz DOT, and structured JSON
- Memory-aware resource defaults, deterministic output ordering, and a stable JSON Schema (Draft 7) for downstream tooling

### Use cases

- **AI-agent and LLM tooling** — rapid project-context indexing for code-aware assistants, retrieval-augmented generation pipelines, and knowledge-base assembly
- **Build and deployment systems** — dependency-relationship mapping, incremental-build optimization, deployment-artifact auditing
- **Filesystem auditing and forensics** — structural analysis at million-file scale, security investigations, change detection
- **Visualization** — terminal trees for development, Graphviz DOT for documentation, JSON for programmatic consumers
- **Cross-language integration** — emit structured filesystem snapshots from one process and consume them in any language that can parse JSON

### Key features

- Pure Rust; no `unsafe` in this crate
- Parallel scanner using `crossbeam-deque` work-stealing queues with per-thread accumulators
- Filename sanitization at scan time (control characters replaced with `?`) prevents terminal injection through hostile filenames
- `--threads all` for full parallelism; default is half-cores for resource-friendly behavior
- Half-RAM soft cap on `--no-limit` scans (Linux); disable with `--no-mem-cap`
- `--tree` mode skips per-file `stat` for ~3–5× cold-cache speedup on file-heavy workloads
- JSON output with sorted keys for diff-friendly, deterministic results
- Stable `schema_version` independent of the binary version
- Status on stderr, data on stdout — pipeable through `jq`, `head`, etc.
- Familiar flag aliases drawn from `find`, `tree`, and `du`

### Semantic code intelligence (v2)

ATree is both a CLI tool and a Rust library (`atree-engine`). Install the CLI for terminal use, or depend on `atree-engine` to embed the code intelligence engine in your own tools.

### CLI

```bash
# Build a semantic index (persistent SQLite DB)
./target/release/atree --semantic --db .atree/index.sqlite --root . --include-files

# Query the index
./target/release/atree query symbols "UserService" --db .atree/index.sqlite
./target/release/atree query callers "build_graph" --db .atree/index.sqlite
./target/release/atree query impact "UserService" --db .atree/index.sqlite
./target/release/atree query routes --db .atree/index.sqlite
./target/release/atree query search "type annotation" --db .atree/index.sqlite
./target/release/atree query stats --db .atree/index.sqlite

# Git history queries (who changed what, when, how often)
./target/release/atree query symbol-ownership "processRequest" --db .atree/index.sqlite
./target/release/atree query change-risk "src/config.ts" --db .atree/index.sqlite
./target/release/atree query find-experts "src/main.ts" --db .atree/index.sqlite
./target/release/atree query smart-co-change "UserService" 10 --db .atree/index.sqlite
./target/release/atree query file-history "src/main.ts" 20 --db .atree/index.sqlite
./target/release/atree query git-blame "src/main.ts" --db .atree/index.sqlite

# Incremental re-index (only changed files, ~99x faster)
./target/release/atree --semantic --db .atree/index.sqlite --root . --incremental
```

### Rust library

```toml
[dependencies]
atree-engine = "0.7"
```

```rust
use atree_engine::{build_graph, ScanOptions};
use atree_engine::store::GraphStore;
use atree_engine::git_history::{extract_and_persist, get_symbol_ownership};

// Index a repo
let scan = build_graph(&ScanOptions::default())?;
let store = GraphStore::open("index.sqlite")?;
extract_and_persist("/path/to/repo", store.conn(), &Default::default())?;

// Combined structural + git intelligence
let owners = get_symbol_ownership(store.conn(), "processRequest")?;
let risk = atree_engine::git_history::get_change_risk(store.conn(), "src/config.ts")?;
```

Feature flags (for the library):
- `git` (default): git history, blame, co-change analysis
- `embeddings`: semantic vector search (requires ONNX runtime)
- `mcp`: MCP server for AI agent integration
- `perf`: performance timing instrumentation

Omit `default-features = false` if you don't need git history or embeddings.

**Supported languages (Tier 1):** TypeScript, JavaScript, Python, Rust
**Supported languages (Tier 2):** Go, Java, C#, PHP
**Supported languages (Tier 3):** C, C++, Ruby, Kotlin, Swift, Bash, JSON, YAML

## Performance

### qwen-code (real-world large repo)

2,555 source files (TypeScript, JavaScript, Java, Python, JSON, Bash, YAML) across a monorepo with 87,888 total files.

| Metric | ATree (cold) | GitNexus | Speedup |
|--------|-------------|----------|---------|
| **Scan time** | **32s** | ~480s (8 min) | **~15x** |
| **Files indexed** | 2,555 | 2,555 | — |
| **Symbols extracted** | 78,763 | — | — |
| **Calls extracted** | 241,904 | — | — |
| **Edges (scope-res)** | 1,260 | — | — |
| **Commits** | 521 | — | — |
| **Authors** | 58 | — | — |
| **File-commit links** | 7,106 | — | — |
| **Scope resolution** | 2.5s | — | — |

Scan taken on 20-core machine with `--no-limit`. GitNexus figure is average of repeated runs.

### anthropic-cookbook (medium repo)

198 source files (Python, TypeScript, JavaScript). Fresh clone, full git history.

| Metric | ATree (cold) | GitNexus | Speedup |
|--------|-------------|----------|---------|
| **Scan time** | **1.6s** | 21.8s | **~13x** |
| **Files indexed** | 158 | (in 3,280 nodes) | — |
| **Symbols extracted** | 2,719 | (in 3,280 nodes) | — |
| **Calls extracted** | 4,390 | (in 4,643 edges) | — |
| **Commits** | 576 | (in 3,280 nodes) | — |
| **Authors** | 98 | — | — |

### ATree's own codebase (small repo)

42 source files, ~7,300 LOC:

| Metric | Cold Index | Incremental (warm) |
|--------|-----------|-------------------|
| **Time** | 28.6s | 0.29s |
| **Speedup** | 1× | **99×** |
| **Files indexed** | 23 | 0 (all reused) |
| **Symbols extracted** | 780 | — |
| **Calls extracted** | 3,771 | — |

Run your own benchmarks:

```bash
cargo build --release
./scripts/benchmark.sh
```

## Installation

### CLI tool

```bash
git clone <repository-url>
cd atree
cargo build --release -p atree
# binary: ./target/release/atree
```

A `cargo install` workflow will be supported once the project is published to crates.io.

### Rust library

```toml
[dependencies]
atree-engine = "0.7"
```

The engine crate is published as `atree-engine` on crates.io. Feature flags:
- `git` (default): git history analysis — omit with `default-features = false` to skip the `git2` dependency
- `embeddings`: semantic vector search via ONNX — omit to skip `fastembed`
- `mcp`: MCP server for AI agent tool integration — omit to skip `rmcp`/`tokio`/`schemars`

Minimal dependency tree (no git, no embeddings, no MCP):
```toml
[dependencies]
atree-engine = { version = "0.7", default-features = false }
```

## Quick start

### Command-line interface

```bash
# Tree overview at depth 3 of the current directory
./target/release/atree -L 3

# Scan everything fast (no caps, files included, skip per-file stat)
./target/release/atree --path /home/me --tree --no-limit --files

# Run A* between two nodes; emit JSON to stdout, status to stderr
./target/release/atree --dir . --from src --to Cargo.toml --files --json > report.json

# Plain text suitable for piping
./target/release/atree --dir . --no-color > tree.txt

# Print the bundled JSON Schema (Draft 7) — no scan, no filesystem access
./target/release/atree --print-schema > schema.json
```

Run `atree --help` for the full flag list, including aliases (`-r`/`--root`/`--path`/`--dir`/`--directory`, `-L`/`--maxdepth`/`--depth`, `--from`/`--to`, `--jobs`/`--workers`, `--fast`/`--no-stat`, `--print-schema`/`--schema`, and others).

### Rust library

```rust
use atree::{
    astar, build_graph, compute_depths, half_cores, ScanOptions,
};
use std::path::PathBuf;

fn main() {
    let opts = ScanOptions {
        root: PathBuf::from("."),
        max_depth: 10,
        max_nodes: 100_000,
        include_files: true,
        threads: half_cores(),
        tree_mode: true,
    };
    let scan = build_graph(&opts).expect("scan failed");
    let depths = compute_depths(&scan.adj, &scan.root_name);
    if let Some((path, expanded)) =
        astar(&scan.adj, &scan.root_name, "src/main.rs", &depths)
    {
        println!("{} hops, A* expanded {} nodes", path.len() - 1, expanded);
    }
}
```

The full public API is documented in `src/lib.rs`. Run `cargo doc --open` for browsable rustdoc.

## JSON output

`--json` emits a single JSON document on stdout. Status messages remain on stderr.

### Schema

The complete machine-readable JSON Schema (Draft 7) is in [`docs/schema.json`](docs/schema.json) and validates every document produced by the binary. The same schema is embedded in the binary at compile time and can be retrieved with no filesystem scan via `atree --print-schema` — useful for consumers that want a self-contained pipeline without shipping the source repo alongside the binary.

Top-level structure:

```jsonc
{
  "schema_version": 1,             // pin this in your consumer
  "version": "0.6.0-alpha",        // binary version (changes more often)
  "root": "/abs/path/to/scanned/dir",
  "root_name": "dir",
  "elapsed_ms": 12.34,
  "threads": 6,
  "options": {
    "max_depth": 4,                // null when unbounded
    "max_nodes": 150,              // null when unbounded
    "include_files": true,
    "tree_mode": false
  },
  "stats": {
    "total_nodes": 1234, "folders": 100, "files": 1100, "symlinks": 30,
    "executables": 50, "hidden": 5, "total_size_bytes": 12345678
  },
  "truncated": false,
  "depths": { /* node id -> depth */ },
  "nodes":  { /* node id -> NodeMeta */ },
  "edges":  { /* node id -> [neighbor ids] */ },
  "path": {
    "start": "src", "goal": "src/main.rs", "hops": 1,
    "nodes": ["src", "src/main.rs"],
    "astar_expanded": 1, "bfs_expanded": 2, "efficiency_pct": 50.0
  }
}
```

All maps are serialized in sorted key order (`BTreeMap`), producing deterministic, diff-friendly output across runs.

### Stability and versioning

| Field | Bumps when |
|---|---|
| `schema_version` | The JSON format breaks (renamed or removed fields, changed value types). Currently `1`. |
| `version` | Any release of the binary, including non-breaking changes. |

Consumers should pin `schema_version` and treat `version` as informational. Within a single `schema_version`, additive (non-breaking) fields may be introduced; consumers should ignore unknown keys.

### Calling from other languages

**Node.js:**
```js
const { execFileSync } = require('child_process');
const Ajv = require('ajv');

// Pull the schema straight from the binary — no source-repo file needed.
const schema = JSON.parse(execFileSync('atree', ['--print-schema']));
const validate = new Ajv().compile(schema);

const report = JSON.parse(execFileSync(
  'atree',
  ['--root', '/project', '--tree', '--no-limit', '--json']
));
if (!validate(report)) throw new Error(JSON.stringify(validate.errors));
if (report.schema_version !== 1) throw new Error('incompatible schema');
```

**Python:**
```python
import json, subprocess, jsonschema

schema = json.loads(subprocess.check_output(['atree', '--print-schema']))
report = json.loads(subprocess.check_output([
    'atree', '--root', project_path,
    '--tree', '--no-limit', '--json',
]))
jsonschema.validate(report, schema)
assert report['schema_version'] == 1
```

## Filesystem scan performance

50,000-node `/usr` scan, warm cache, 12-core machine:

| Threads | Time   | Speedup        |
|--------:|-------:|---------------:|
| 1       | 118 ms | 1.0× baseline  |
| 4       |  37 ms | 3.2×           |
| 8       |  29 ms | 4.1×           |
| 12      | **28 ms** | **5.3×**    |

Cold-cache scans on file-heavy trees see an additional ~3–5× win when `--tree` mode skips per-file `stat` syscalls.

## Architecture

ATree is a Cargo workspace with two packages:

- **`atree-engine`** — the code intelligence library. Tree-sitter extraction, scope resolution, git history analysis, A* evidence paths, SQLite persistence. Feature-flagged modules for embeddings and MCP.
- **`atree-cli`** — the CLI binary. 55+ query subcommands, JSON output, A* filesystem pathfinding. Thin wrapper over the engine.

### Parallel scanner

The filesystem scanner uses `crossbeam-deque`'s per-thread LIFO work-stealing queues:

- Each worker pushes newly-discovered subdirectories onto its own queue (cache-hot, lock-free)
- Idle workers steal from siblings' queues
- Termination is detected via an atomic `pending` counter; no condition variables

Each worker accumulates results into thread-local `FxHashMap` instances, eliminating contention on the global maps during the scan. A single-threaded merge runs once after all workers complete.

### Semantic pipeline

The semantic engine runs a DAG of analysis phases over the parsed files:

1. **Scan/Parse** — parallel tree-sitter extraction across all source files
2. **Cross-file** — batch SQLite insert, scope resolution (C3 MRO, receiver-bound, free-call), edge persistence
3. **Git history** — commit log walk, per-file change tracking, author aggregation
4. **Graph analytics** — community detection (label propagation), process tracing (BFS call chains)
5. **Search index** — BM25 FTS5 index + optional semantic embeddings

The entire scan-time hot path is `unsafe`-free Rust over `std::fs` syscalls.

## Resource defaults

- **Threads** — half of available logical cores (`half_cores()`). Override with `--threads N` or `--threads all`.
- **Memory soft-cap** — applied only when `--no-limit` is set. Approximately half of available RAM, computed from `/proc/meminfo` on Linux. Disable with `--no-mem-cap`. No cap is applied on non-Linux platforms (the value of `MemAvailable` is unavailable).
- **Default node cap** — `150`, intended for quick demos. Use `--max-nodes N` for larger scans or `--no-limit` to remove the cap entirely.
- **Default depth cap** — `4`. Override with `--max-depth N`, `-L N`, or `--no-limit`.

## Security

- **Filename sanitization** — control characters (including ANSI escape sequences) in filenames are replaced with `?` at scan time before being stored or rendered. Hostile filenames cannot inject terminal escapes into output, JSON consumers, or DOT renderers.
- **Strict root validation** — `--root` paths that don't exist (or aren't directories) are rejected with explicit `NotFound` / `InvalidInput` errors before any scan work begins, instead of silently producing a single-node fake-folder result that could mask scripting typos.
- **No `unsafe` code** in this crate.
- **No panics** in normal operation. Metadata-read failures and unreadable directory entries are skipped rather than propagated.
- **Iterative scan** — recursion is replaced by an explicit work queue, so deeply nested directories cannot overflow the stack.
- **Determinism** — JSON output is sorted; the leaf auto-pick (when `--goal` is omitted or unresolvable) sorts candidates before selection.
- **TOCTOU note** — `read_dir` and `metadata` are separate syscalls; concurrent filesystem mutation during a scan may produce slightly inconsistent snapshots. This is not exploitable but is documented here for completeness.

## Exit codes

| Code | Meaning |
|-----:|---------|
| `0`  | Success |
| `1`  | Runtime error — I/O failure, no path found between specified start and goal |
| `2`  | Argument error — unknown flag, missing value, malformed numeric |

## Building and testing

ATree is a Cargo workspace with two packages: `atree-engine` (library) and `atree` (CLI binary).

```bash
# Build everything
cargo build --release --workspace

# Build just the CLI
cargo build --release -p atree

# Build just the engine library
cargo build --release -p atree-engine

# Run all tests
cargo test -p atree-engine

# Generate and view rustdoc
cargo doc --open -p atree-engine
```

The release profile uses `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, and `strip = true` for maximum runtime performance at the cost of slightly slower builds.

### Multi-platform release artifacts

To produce binaries for every supported platform (Linux glibc, Linux musl static, Windows x86_64, macOS Apple Silicon, macOS Intel), use the bundled build script:

```bash
scripts/build_release.sh                # all host-buildable targets
scripts/build_release.sh linux-musl     # one specific target
scripts/build_release.sh --help         # full target list
```

Cross-compilation prerequisites and per-platform instructions are documented in [BUILD.md](BUILD.md).

## Dependencies

### atree-engine (library)

| Crate | Purpose |
|-------|---------|
| `tree-sitter` + 16 language grammars | Multi-language AST parsing and symbol extraction |
| `rusqlite` (bundled) | Persistent SQLite graph store with recursive CTEs |
| `git2` | Git history extraction (commits, blame, co-change) — optional feature |
| `crossbeam-deque` | Lock-free work-stealing parallel scanner |
| `rustc-hash` | `FxHashMap` — fast non-cryptographic hashing |
| `mimalloc` | Fast multi-threaded memory allocator |
| `serde` + `serde_json` | Structured output and config parsing |
| `fastembed` | Semantic vector embeddings (optional feature) |
| `rmcp` + `tokio` + `schemars` | MCP server for AI agent tool integration (optional feature) |

### atree-cli (binary)

All of the above via `atree-engine`, plus CLI argument parsing. No additional heavy dependencies.

## Project information

### Team

`atree` is developed by **UnityAILab**, a sovereign, independent research and engineering team:

- **Sponge** — `sponge@unityailab.com`
- **Alfreddo**
- **Gee**
- **Red**
- **B-A-M-N**

### Contact

`contact@unityailab.com`

### Notice

UnityAILab is a sovereign, independent team. **It is not affiliated with, endorsed by, or connected in any way to Unity Technologies, Unity Software Inc., the Unity game engine, or any of their subsidiaries, products, or trademarks.** The "Unity" in our name refers to the unity of the AI and systems-research disciplines we pursue. See [NOTICE](NOTICE) for the full disclaimer.

### License

MIT — see [LICENSE](LICENSE) and [NOTICE](NOTICE).

You may use, modify, and redistribute this software freely, including in derivative works, provided that:

1. The original copyright notice and the contents of `LICENSE` and `NOTICE` are retained.
2. Attribution to **UnityAILab** and its contributors (Sponge, Alfreddo, Gee, Red, B-A-M-N) is preserved in derivative works.

### Changelog

See [CHANGELOG.md](CHANGELOG.md) for the release history.
