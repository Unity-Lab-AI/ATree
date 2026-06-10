# ATree

> A\* filesystem pathfinder and semantic code intelligence engine — parallel multi-language symbol extraction, scope-aware resolution, git history analysis, and AI agent integration via MCP.

| | |
|---|---|
| **Version** | `0.7.0-alpha` |
| **Status** | Alpha — A\* filesystem tool and semantic engine with scope resolution, MCP server, and 60+ CLI/MCP query commands. |
| **Authors** | UnityAILab — Sponge, Alfreddo, Gee, Red, B-A-M-N |
| **License** | MIT (see [LICENSE](LICENSE) and [NOTICE](NOTICE)) |
| **Contact** | `contact@unityailab.com` |

ATree is two products sharing one CLI binary:

- **ATree** — parallel filesystem scanner with A\* pathfinding. Walks directory trees, renders Unicode/ASCII trees, finds optimal paths, emits JSON/Graphviz.
- **ATree Semantic Engine** — multi-language code intelligence. Parses source with tree-sitter, resolves symbols across files, analyzes git history, exposes everything through CLI queries, a Rust library, and an MCP server for AI agents.

Pass `--semantic` to activate the code intelligence layer.

## Contents

- [ATree — Filesystem tool](#atree--filesystem-tool)
  - [Overview](#overview)
  - [Key features](#key-features)
  - [Quick start](#quick-start)
  - [JSON output](#json-output)
  - [Filesystem performance](#filesystem-performance)
  - [Resource defaults](#resource-defaults)
- [ATree Semantic Engine](#atree-semantic-engine)
  - [Overview](#overview-1)
  - [Key features](#key-features-1)
  - [Quick start](#quick-start-1)
  - [Query reference](#query-reference)
  - [Semantic performance](#semantic-performance)
  - [Scope resolution](#scope-resolution)
  - [MCP server](#mcp-server)
- [Architecture](#architecture)
- [Security](#security)
- [Exit codes](#exit-codes)
- [Building and testing](#building-and-testing)
- [Dependencies](#dependencies)
- [Project information](#project-information)

---

# ATree — Filesystem tool

## Overview

ATree walks a directory tree using a lock-free work-stealing parallel scanner, builds an undirected graph (nodes are files and folders, edges are parent–child relationships), and supports:

- Optimal-path search via A\* between any two nodes, with blind-BFS comparison for efficiency reporting
- Three output formats: terminal-rendered tree (Unicode or ASCII, color-aware), Graphviz DOT, and structured JSON
- Memory-aware resource defaults, deterministic output ordering, and a stable JSON Schema (Draft 7) for downstream tooling

### Use cases

- **Filesystem auditing and forensics** — structural analysis at million-file scale, security investigations, change detection
- **Visualization** — terminal trees for development, Graphviz DOT for documentation, JSON for programmatic consumers
- **Build and deployment systems** — dependency-relationship mapping, deployment-artifact auditing
- **Cross-language integration** — emit structured filesystem snapshots from one process and consume them in any language that can parse JSON

## Key features

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

## Quick start

```bash
# Tree overview at depth 3
./target/release/atree -L 3

# Scan everything fast (no caps, files included, skip per-file stat)
./target/release/atree --path /home/me --tree --no-limit --files

# A* between two nodes; JSON to stdout, status to stderr
./target/release/atree --dir . --from src --to Cargo.toml --files --json > report.json

# Plain text
./target/release/atree --dir . --no-color > tree.txt

# Print the bundled JSON Schema (Draft 7)
./target/release/atree --print-schema > schema.json
```

Run `atree --help` for the full flag list, including aliases (`-r`/`--root`/`--path`/`--dir`/`--directory`, `-L`/`--maxdepth`/`--depth`, `--from`/`--to`, `--jobs`/`--workers`, `--fast`/`--no-stat`, `--print-schema`/`--schema`, and others).

### Rust library (filesystem)

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

## JSON output

`--json` emits a single JSON document on stdout. Status messages remain on stderr.

### Schema

The complete machine-readable JSON Schema (Draft 7) is in [`docs/schema.json`](docs/schema.json) and validates every document produced by the binary. The same schema is embedded in the binary at compile time and can be retrieved with no filesystem scan via `atree --print-schema`.

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

## Filesystem performance

50,000-node `/usr` scan, warm cache, 12-core machine:

| Threads | Time   | Speedup        |
|--------:|-------:|---------------:|
| 1       | 118 ms | 1.0× baseline  |
| 4       |  37 ms | 3.2×           |
| 8       |  29 ms | 4.1×           |
| 12      | **28 ms** | **5.3×**    |

Cold-cache scans on file-heavy trees see an additional ~3–5× win when `--tree` mode skips per-file `stat` syscalls.

## Resource defaults

- **Threads** — half of available logical cores (`half_cores()`). Override with `--threads N` or `--threads all`.
- **Memory soft-cap** — applied only when `--no-limit` is set. Approximately half of available RAM, computed from `/proc/meminfo` on Linux. Disable with `--no-mem-cap`. No cap is applied on non-Linux platforms.
- **Default node cap** — `150`, intended for quick demos. Use `--max-nodes N` for larger scans or `--no-limit` to remove the cap entirely.
- **Default depth cap** — `4`. Override with `--max-depth N`, `-L N`, or `--no-limit`.

---

# ATree Semantic Engine

## Overview

The ATree Semantic Engine builds a knowledge graph from your source code. It parses 16+ languages with tree-sitter, resolves symbols across files using scope-aware analysis (C3 MRO, receiver-bound calls, overload narrowing), analyzes git history, and exposes everything through CLI queries, a Rust library, and an MCP server for AI agents.

The engine is activated with the `--semantic` flag. It stores its index in a persistent SQLite database (`.atree/index.sqlite` by default) that can be queried without re-scanning.

### Use cases

- **AI coding assistants** — evidence-ranked code search, impact analysis, symbol context via MCP
- **Code review** — blast radius analysis, affected tests, change risk scoring
- **Refactoring** — coordinated rename, rename safety checks, minimal edit scope
- **Architecture** — boundary violation detection, dependency cycle detection, public API surface
- **Git intelligence** — symbol ownership, co-change analysis, expert finding

## Key features

- **17 languages** — TypeScript, JavaScript, Python, Rust, Go, Java, C, C++, C#, PHP, Ruby, Kotlin, Swift, Dart, Bash, JSON, YAML
- **Scope resolution** — C3 MRO, receiver-bound calls (7-case dispatcher), free-call fallback, import resolution (3-tier)
- **Type extraction** — AST type bindings extracted and stored; type environment (Tier 0 annotations) populated during indexing. Tier 1 (constructor inference) and Tier 2 (assignment propagation) are not yet wired into scope resolution.
- **Git history** — commit log walk, per-file change tracking, author aggregation, co-change coupling
- **Graph analytics** — community detection (label propagation), process tracing (BFS call chains)
- **Search** — BM25 FTS5 full-text search + optional semantic vector embeddings (ONNX)
- **Incremental indexing** — only re-indexes changed files (significant speedup on warm re-index)
- **MCP server** — 46 tools exposed via Model Context Protocol for AI agent integration
- **Evidence engine** — A* + beam search over the layered code graph, token-bounded, confidence-ranked results

## Quick start

### 1. Index a repository

```bash
# Full semantic index (persistent SQLite DB)
atree --semantic --db .atree/index.sqlite --root . --include-files

# Incremental re-index (only changed files, much faster)
atree --semantic --db .atree/index.sqlite --root . --incremental

# With embeddings for semantic vector search
atree --semantic --embeddings --db .atree/index.sqlite --root .
```

### 2. Query the index

```bash
# Search symbols
atree query symbols "UserService" --db .atree/index.sqlite

# Show callers/callees
atree query callers "build_graph" --db .atree/index.sqlite
atree query callees "build_graph" --db .atree/index.sqlite

# Impact analysis (blast radius)
atree query impact "UserService" --db .atree/index.sqlite

# Full symbol context (all edge types, evidence paths)
atree query context "build_graph" --db .atree/index.sqlite

# Explain a symbol
atree query explain "parse_args" --db .atree/index.sqlite

# Trace call path between two symbols
atree query trace-path "main" --to "build_graph" --db .atree/index.sqlite

# Full-text search
atree query search "type annotation" --db .atree/index.sqlite

# Index statistics
atree query stats --db .atree/index.sqlite
```

### 3. Git intelligence

```bash
# Who owns a symbol (recency + volume ranked)
atree query symbol-ownership "processRequest" --db .atree/index.sqlite

# Risk score for changing a file
atree query change-risk "src/config.ts" --db .atree/index.sqlite

# Find experts for a file
atree query find-experts "src/main.ts" --db .atree/index.sqlite

# Smart co-change (static + git coupling)
atree query smart-co-change "UserService" 10 --db .atree/index.sqlite

# File history and blame
atree query file-history "src/main.ts" 20 --db .atree/index.sqlite
atree query git-blame "src/main.ts" --db .atree/index.sqlite
```

### 4. Rust library (semantic engine)

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

Minimal dependency tree (no git, no embeddings, no MCP):
```toml
[dependencies]
atree-engine = { version = "0.7", default-features = false }
```

## Query reference

### Symbol queries

```bash
atree query symbols <name> --db <path>           # Search symbols by name (fuzzy)
atree query callers <symbol> [depth] --db <path> # Show callers of a symbol
atree query callees <symbol> [depth] --db <path> # Show callees of a symbol
atree query impact <symbol> [depth] --db <path>  # Impact analysis (blast radius)
atree query context <symbol> --db <path>          # 360-degree symbol context
atree query explain <symbol> --db <path>          # Full symbol explanation
```

### Pathfinding and search

```bash
atree query trace-path <from> --to <to> --db <path>  # A* call path between symbols
atree query evidence-path <query> --db <path>         # A* + beam search evidence paths
atree query search <text> --db <path>                  # Full-text BM25 search
atree query semantic-search <text> --db <path>        # Semantic vector search (requires --embeddings)
```

### Routes and API

```bash
atree query routes --db <path>                        # List detected API routes
atree query shape-check [route] --db <path>           # Check API route response shapes
atree query api-impact <route|file> --db <path>       # Pre-change impact for API handler
atree query public-api [module] --db <path>           # List public API surface
```

### Git intelligence

```bash
atree query symbol-ownership <symbol> --db <path>      # Who owns a symbol
atree query change-risk <file> --db <path>             # Risk score for changing a file
atree query find-experts <file> --db <path>             # Find experts for a file
atree query smart-co-change <symbol> [limit] --db <path> # Combined static + git co-change
atree query file-history <path> [limit] --db <path>    # Commit history for a file
atree query git-blame <path> --db <path>                # Git blame for a file
atree query top-authors [limit] --db <path>            # Top authors by commit count
atree query change-hotspots [limit] --db <path>        # Most frequently changed files
atree query co-change <path> [limit] --db <path>       # Files that change together
atree query git-stats --db <path>                      # Git history statistics
```

### Analysis and detection

```bash
atree query detect-changes --db <path>                 # Uncommitted changes + affected symbols
atree query semantic-diff [base_ref] --db <path>       # Transitive impact of changes
atree query affected-tests <symbol> --db <path>        # Tests affected by symbol changes
atree query validation-plan <symbol> --db <path>       # Validation plan for a change
atree query contract-changes [base_ref] --db <path>    # API contract changes
atree query boundary-check --db <path>                 # Architecture boundary violations
atree query scope-violations --db <path>               # Private symbol used externally
atree query config-map --db <path>                     # Configuration surface map
atree query impact-by-kind <target> <kind> [dir] --db <path> # Impact filtered by symbol kind
atree query side-effects <symbol> --db <path>          # Side effect scan (I/O, global state)
atree query change-coupling <symbol> --db <path>       # Symbols that change together
atree query concurrency --db <path>                    # Concurrency surface detection
atree query edit-scope <symbol> --db <path>            # Minimal edit scope for a change
atree query issue-locator <description> --db <path>    # Map issue description to code
atree query docs-drift --db <path>                     # Documentation drift detection
atree query rename-safety <old> <new> --db <path>      # Check if a rename is safe
atree query dead-code --db <path>                      # Dead code candidates
atree query hotspots --db <path>                       # Ownership hotspots (high fan-in/out)
atree query error-trace <symbol> --db <path>           # Error path tracing
atree query resource-lifecycle <symbol> --db <path>    # Resource lifecycle mapping
atree query dep-cycles --db <path>                     # Dependency cycle detection
atree query uncovered --db <path>                      # Symbols with no test coverage
atree query resolution-stats --db <path>               # Resolution quality statistics
atree query tool-map --db <path>                       # Tool-like symbols
```

### Verification and groups

```bash
atree query verify --type test|lint|typecheck --db <path>  # Run tests/lint/typecheck
atree query repos --db <path>                              # List indexed repos
atree query group-sync --db <path>                         # Rebuild cross-repo contract links
atree query stats --db <path>                              # Index statistics
```

## Semantic performance

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
| **Symbols extracted** | 2,719 | — | — |
| **Calls extracted** | 4,390 | — | — |
| **Commits** | 576 | — | — |
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

## Scope resolution

The scope-resolution pipeline (ported from GitNexus RFC #909 Ring 3) resolves symbols across files and languages:

- **Receiver-bound calls** — 7-case dispatcher handling: super, compound, namespace, class-name, dotted typeBinding, chain typeBinding, simple typeBinding
- **Free-call fallback** — resolves unqualified calls via scope bindings when receiver-bound resolution fails
- **Compound receiver resolver** — handles dotted expressions like `a.b.c.method()`
- **Overload narrowing** — matches by arity + argument types
- **C3 MRO** — correct linearization for multiple inheritance (Python, etc.)
- **Ownership reconciliation** — populates class-owned members after resolution
- **Namespace target collection** — gathers all possible targets for a reference site

All 286 tests pass, including stress tests for deep inheritance chains, mixed-language projects, and incremental scan correctness.

## MCP server

ATree exposes 46 MCP (Model Context Protocol) tools for AI agent integration via stdio transport.

### Configuration

Add to your MCP client config (e.g. `crush.json`):

```json
{
  "mcp": {
    "atree": {
      "type": "stdio",
      "command": "atree",
      "args": ["mcp-server"],
      "env": {}
    }
  }
}
```

Requires: `cargo build --release --features mcp -p atree`

### Starting the server

```bash
# Auto-detect .atree/index.sqlite in current directory:
atree mcp-server

# Or specify explicitly:
atree mcp-server --db .atree/index.sqlite
```

### Available tools (43)

**In-process** (fast, evidence-ranked, no subprocess):
| Tool | Description |
|------|-------------|
| `query` | Search the code knowledge graph for execution flows |
| `context` | 360-degree symbol view — all edge types with confidence scores |
| `impact` | Blast radius analysis with weighted risk scoring |
| `evidence_path` | A* + beam search evidence paths over the code graph |
| `evidence_search` | Full-text search over committed evidence (FTS5). Searches raw content, normalized text, file paths, kinds, and tags. |
| `pattern_mine` | Mine recurring patterns from the evidence graph. Extracts motifs (co-occurring evidence kinds) ranked by frequency × dispersion × stability. |
| `constraint_check` | Pattern-derived constraints (RequiredProperty motifs). ForbiddenTransition and ArchitecturalRule synthesis are not yet implemented. |
| `explain_symbol` | Full symbol explanation with evidence paths |
| `trace_call_path` | A* pathfinding between two symbols |

**CLI fallback** (spawns `atree` binary):
`list_repos`, `index`, `detect_changes`, `rename`, `cypher`, `route_map`, `shape_check`, `tool_map`, `api_impact`, `verify`, `group_list`, `group_sync`, `find_entrypoints`, `public_api_surface`, `affected_tests`, `validation_plan`, `contract_change_detector`, `architecture_boundary_check`, `scope_violation_detector`, `config_surface_map`, `impact_by_symbol_kind`, `semantic_diff_summary`, `side_effect_scanner`, `change_coupling`, `concurrency_surface_detector`, `minimal_edit_scope`, `issue_to_code_locator`, `docs_drift_detector`, `rename_safety_check`, `dead_code_candidates`, `ownership_hotspots`, `error_path_trace`, `resource_lifecycle_map`, `dependency_cycle_detector`, `find_uncovered_symbols`, `resolution_stats`

### Example MCP session

```bash
# Initialize
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1.0"}}}

# List tools
{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}

# Call a tool
{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"query","arguments":{"query":"build_graph","max_seeds":3,"max_symbols":5}}}
```

---

# Architecture

ATree is a Cargo workspace with two packages:

- **`atree-engine`** — the semantic code intelligence library. Tree-sitter extraction, scope resolution, git history analysis, A* evidence paths, SQLite persistence. Feature-flagged modules for embeddings and MCP.
- **`atree-cli`** — the CLI binary. Filesystem scanning, 60+ query subcommands, JSON output, A* filesystem pathfinding. Thin wrapper over the engine.

## Parallel scanner

The filesystem scanner uses `crossbeam-deque`'s per-thread LIFO work-stealing queues:

- Each worker pushes newly-discovered subdirectories onto its own queue (cache-hot, lock-free)
- Idle workers steal from siblings' queues
- Termination is detected via an atomic `pending` counter; no condition variables

Each worker accumulates results into thread-local `FxHashMap` instances, eliminating contention on the global maps during the scan. A single-threaded merge runs once after all workers complete.

## Semantic pipeline

The semantic engine runs a DAG of analysis phases over the parsed files:

1. **Scan/Parse** — parallel tree-sitter extraction across all source files
2. **Evidence Extraction** — AST captures → `EvidenceCandidate[]` (13 evidence kinds: SYMBOL_DECLARATION, FUNCTION_CALL, IMPORT_EDGE, TYPE_RELATION, etc.)
3. **Evidence Lifecycle** — normalize (canonicalize content) → dedupe (content-addressed identity) → enrich (symbol resolution) → calibrate (confidence scoring) → commit (SQLite persistence)
4. **Cross-file** — batch SQLite insert, scope resolution (C3 MRO, receiver-bound, free-call), edge persistence
5. **Pattern Mining** — 2-gram co-occurrence mining from the evidence graph, scoring (frequency × dispersion × stability × entropy)
6. **Constraint Synthesis** — forbidden transitions from evidence contradictions, required properties from stable patterns, violation detection
7. **Git history** — commit log walk, per-file change tracking, author aggregation
8. **Graph analytics** — community detection (label propagation), process tracing (BFS call chains)
9. **Search index** — BM25 FTS5 index + optional semantic embeddings

### 4-Layer Epistemic Model

```
Layer 0: PRIMITIVES   — Deterministic execution surface (symbols, scopes, calls, heritage)
Layer 1: EVIDENCE     — Observation layer (atomic, verifiable, content-addressed)
Layer 2: PATTERNS     — Inductive compression (subgraph motifs, recurring structures)
Layer 3: CONSTRAINTS  — Policy + invariants (forbidden transitions, required properties)
```

The entire scan-time hot path is `unsafe`-free Rust over `std::fs` syscalls.

# Security

- **Filename sanitization** — control characters (including ANSI escape sequences) in filenames are replaced with `?` at scan time before being stored or rendered. Hostile filenames cannot inject terminal escapes into output, JSON consumers, or DOT renderers.
- **Strict root validation** — `--root` paths that don't exist (or aren't directories) are rejected with explicit `NotFound` / `InvalidInput` errors before any scan work begins.
- **SQL injection prevention** — `validate_cypher_query()` with table/column allowlist; blocks PRAGMA, INSERT, UPDATE, DELETE, DROP, ALTER, CREATE, ATTACH, DETACH, comments, and multi-statement injection.
- **No `unsafe` code** in the engine or CLI.
- **No panics** in normal operation. Metadata-read failures and unreadable directory entries are skipped rather than propagated.
- **Iterative scan** — recursion is replaced by an explicit work queue, so deeply nested directories cannot overflow the stack.
- **Determinism** — JSON output is sorted; the leaf auto-pick (when `--goal` is omitted or unresolvable) sorts candidates before selection.
- **TOCTOU note** — `read_dir` and `metadata` are separate syscalls; concurrent filesystem mutation during a scan may produce slightly inconsistent snapshots. This is not exploitable but is documented here for completeness.

# Exit codes

| Code | Meaning |
|-----:|---------|
| `0` | Success |
| `1` | Runtime error — I/O failure, no path found between specified start and goal |
| `2` | Argument error — unknown flag, missing value, malformed numeric |

# Building and testing

ATree is a Cargo workspace with two packages: `atree-engine` (library) and `atree` (CLI binary).

```bash
# Build everything
cargo build --release --workspace

# Build just the CLI
cargo build --release -p atree

# Build just the engine library
cargo build --release -p atree-engine

# Build with MCP support
cargo build --release --features mcp -p atree

# Run all tests (220 tests)
cargo test -p atree-engine

# Generate and view rustdoc
cargo doc --open -p atree-engine
```

The release profile uses `lto = "fat"`, `codegen-units = 1`, `panic = "abort"`, and `strip = true` for maximum runtime performance at the cost of slightly slower builds.

## Installation

```bash
git clone <repository-url>
cd atree
cargo build --release -p atree
# binary: ./target/release/atree

# With MCP support:
cargo build --release --features mcp -p atree

# Install to PATH:
cp ./target/release/atree ~/.local/bin/atree
```

A `cargo install` workflow will be supported once the project is published to crates.io.

### Multi-platform release artifacts

To produce binaries for every supported platform (Linux glibc, Linux musl static, Windows x86_64, macOS Apple Silicon, macOS Intel), use the bundled build script:

```bash
scripts/build_release.sh                # all host-buildable targets
scripts/build_release.sh linux-musl     # one specific target
scripts/build_release.sh --help         # full target list
```

Cross-compilation prerequisites and per-platform instructions are documented in [BUILD.md](BUILD.md).

# Dependencies

## ATree — Filesystem tool

These deps are always included. Zero optional features needed.

| Crate | Purpose |
|-------|---------|
| `crossbeam-deque` | Lock-free work-stealing parallel scanner |
| `rustc-hash` | `FxHashMap` — fast non-cryptographic hashing |
| `mimalloc` | Fast multi-threaded memory allocator |
| `serde` + `serde_json` | Structured output and config parsing |

Minimal build (filesystem only, no semantic):
```bash
cargo build --release -p atree
```

## ATree Semantic Engine

Everything above, plus these engine deps. Activated with `--semantic` or `--db`.

### Core engine (always included with semantic)

| Crate | Purpose |
|-------|---------|
| `tree-sitter` + 16 language grammars | Multi-language AST parsing and symbol extraction |
| `rusqlite` (bundled) | Persistent SQLite graph store with recursive CTEs |
| `regex` | Pattern matching for symbol extraction |

### Optional: Git history (`git` feature, default on)

| Crate | Purpose |
|-------|---------|
| `git2` | Git history extraction (commits, blame, co-change) |
| `chrono` | Timestamp handling |

Disable: `atree-engine = { version = "0.7", default-features = false }`

### Optional: Semantic embeddings (`embeddings` feature)

| Crate | Purpose |
|-------|---------|
| `fastembed` | Semantic vector embeddings via ONNX runtime |

Enable: `--embeddings` flag during indexing.

### Optional: MCP server (`mcp` feature)

| Crate | Purpose |
|-------|---------|
| `rmcp` | Model Context Protocol server runtime |
| `tokio` | Async runtime for MCP stdio transport |
| `schemars` | JSON Schema generation for tool inputs |

Enable: `cargo build --release --features mcp -p atree`

### Full semantic build (everything)

```bash
cargo build --release --features mcp -p atree
```

Equivalently:
```toml
[dependencies]
atree-engine = { version = "0.7", features = ["git", "embeddings", "mcp"] }
```

# Project information

## Team

ATree is developed by **UnityAILab**, a sovereign, independent research and engineering team:

- **Sponge** — `sponge@unityailab.com`
- **Alfreddo**
- **Gee**
- **Red**
- **B-A-M-N**

## Contact

`contact@unityailab.com`

## Notice

UnityAILab is a sovereign, independent team. **It is not affiliated with, endorsed by, or connected in any way to Unity Technologies, Unity Software Inc., the Unity game engine, or any of their subsidiaries, products, or trademarks.** The "Unity" in our name refers to the unity of the AI and systems-research disciplines we pursue. See [NOTICE](NOTICE) for the full disclaimer.

## License

MIT — see [LICENSE](LICENSE) and [NOTICE](NOTICE).

You may use, modify, and redistribute this software freely, including in derivative works, provided that:

1. The original copyright notice and the contents of `LICENSE` and `NOTICE` are retained.
2. Attribution to **UnityAILab** and its contributors (Sponge, Alfreddo, Gee, Red, B-A-M-N) is preserved in derivative works.

## Changelog

See [CHANGELOG.md](CHANGELOG.md) for the release history.
