# file_system_a_star

> Production-grade parallel filesystem analysis and A\* pathfinding, written in Rust.

| | |
|---|---|
| **Version** | `0.6.0-alpha` |
| **Status** | Alpha — public API stabilizing. JSON `schema_version` is `1`. |
| **Authors** | UnityAILab — Sponge, Alfreddo, Gee, Red |
| **License** | MIT (see [LICENSE](LICENSE) and [NOTICE](NOTICE)) |
| **Contact** | `contact@unityailab.com` |

`file_system_a_star` performs high-throughput parallel directory enumeration and computes shortest paths across the resulting graph using the A\* algorithm with an admissible depth-difference heuristic. It is available both as a Rust library and as a single-binary CLI with structured JSON output for integration with consumers in any language.

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

## Installation

```bash
git clone <repository-url>
cd file_system_a_star
cargo build --release
# binary: ./target/release/file_system_a_star
```

A `cargo install` workflow will be supported once the project is published to crates.io.

## Quick start

### Command-line interface

```bash
# Tree overview at depth 3 of the current directory
./target/release/file_system_a_star -L 3

# Scan everything fast (no caps, files included, skip per-file stat)
./target/release/file_system_a_star --path /home/me --tree --no-limit --files

# Run A* between two nodes; emit JSON to stdout, status to stderr
./target/release/file_system_a_star --dir . --from src --to Cargo.toml --files --json > report.json

# Plain text suitable for piping
./target/release/file_system_a_star --dir . --no-color > tree.txt
```

Run `file_system_a_star --help` for the full flag list, including aliases (`-r`/`--root`/`--path`/`--dir`/`--directory`, `-L`/`--maxdepth`/`--depth`, `--from`/`--to`, `--jobs`/`--workers`, `--fast`/`--no-stat`, and others).

### Rust library

```rust
use file_system_a_star::{
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

The complete machine-readable JSON Schema (Draft 7) is in [`docs/schema.json`](docs/schema.json) and validates every document produced by the binary.

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
const schema = require('./docs/schema.json');
const validate = new Ajv().compile(schema);

const report = JSON.parse(execFileSync(
  'file_system_a_star',
  ['--root', '/project', '--tree', '--no-limit', '--json']
));
if (!validate(report)) throw new Error(JSON.stringify(validate.errors));
if (report.schema_version !== 1) throw new Error('incompatible schema');
```

**Python:**
```python
import json, subprocess, jsonschema

schema = json.load(open('docs/schema.json'))
report = json.loads(subprocess.check_output([
    'file_system_a_star', '--root', project_path,
    '--tree', '--no-limit', '--json',
]))
jsonschema.validate(report, schema)
assert report['schema_version'] == 1
```

## Performance

50,000-node `/usr` scan, warm cache, 12-core machine:

| Threads | Time   | Speedup        |
|--------:|-------:|---------------:|
| 1       | 118 ms | 1.0× baseline  |
| 4       |  37 ms | 3.2×           |
| 8       |  29 ms | 4.1×           |
| 12      | **28 ms** | **5.3×**    |

Cold-cache scans on file-heavy trees see an additional ~3–5× win when `--tree` mode skips per-file `stat` syscalls.

## Architecture

The parallel scanner uses `crossbeam-deque`'s per-thread LIFO work-stealing queues:

- Each worker pushes newly-discovered subdirectories onto its own queue (cache-hot, lock-free)
- Idle workers steal from siblings' queues
- Termination is detected via an atomic `pending` counter; no condition variables and no `notify_all` syscalls

Each worker accumulates results into thread-local `FxHashMap` instances, eliminating contention on the global maps during the scan. A single-threaded merge runs once after all workers complete and is bounded by the final node count rather than by inter-thread synchronization.

The entire scan-time hot path is `unsafe`-free Rust over `std::fs` syscalls.

## Resource defaults

- **Threads** — half of available logical cores (`half_cores()`). Override with `--threads N` or `--threads all`.
- **Memory soft-cap** — applied only when `--no-limit` is set. Approximately half of available RAM, computed from `/proc/meminfo` on Linux. Disable with `--no-mem-cap`. No cap is applied on non-Linux platforms (the value of `MemAvailable` is unavailable).
- **Default node cap** — `150`, intended for quick demos. Use `--max-nodes N` for larger scans or `--no-limit` to remove the cap entirely.
- **Default depth cap** — `4`. Override with `--max-depth N`, `-L N`, or `--no-limit`.

## Security

- **Filename sanitization** — control characters (including ANSI escape sequences) in filenames are replaced with `?` at scan time before being stored or rendered. Hostile filenames cannot inject terminal escapes into output, JSON consumers, or DOT renderers.
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

```bash
cargo build --release      # binary at target/release/file_system_a_star
cargo test --release       # all unit + integration tests
cargo doc --open           # generate and view rustdoc for the library
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

| Crate              | Purpose                                                              |
|--------------------|----------------------------------------------------------------------|
| `mimalloc`         | Fast multi-threaded memory allocator                                 |
| `rustc-hash`       | `FxHashMap` — non-cryptographic hash for higher throughput on string keys |
| `crossbeam-deque`  | Lock-free work-stealing queues                                       |
| `serde`            | Derive macros for serialize / deserialize                            |
| `serde_json`       | JSON output and roundtrip-safe parsing                               |

## Project information

### Team

`file_system_a_star` is developed by **UnityAILab**, a sovereign, independent research and engineering team:

- **Sponge** — `sponge@unityailab.com`
- **Alfreddo**
- **Gee**
- **Red**

### Contact

`contact@unityailab.com`

### Notice

UnityAILab is a sovereign, independent team. **It is not affiliated with, endorsed by, or connected in any way to Unity Technologies, Unity Software Inc., the Unity game engine, or any of their subsidiaries, products, or trademarks.** The "Unity" in our name refers to the unity of the AI and systems-research disciplines we pursue. See [NOTICE](NOTICE) for the full disclaimer.

### License

MIT — see [LICENSE](LICENSE) and [NOTICE](NOTICE).

You may use, modify, and redistribute this software freely, including in derivative works, provided that:

1. The original copyright notice and the contents of `LICENSE` and `NOTICE` are retained.
2. Attribution to **UnityAILab** and its contributors (Sponge, Alfreddo, Gee, Red) is preserved in derivative works.

### Changelog

See [CHANGELOG.md](CHANGELOG.md) for the release history.
