# Changelog

All notable changes to `atree` are documented here. The project
follows semantic versioning; the JSON output format is independently versioned
via the `schema_version` field (currently `2`).

## [0.7.0-alpha] - 2026-05-12

### Added
- **Native Semantic Engine**: Integrated Tree-sitter for parallel symbol extraction.
- **Multi-language Support**: 16 language providers implemented in Rust.
- **--semantic flag**: CLI and JSON output support for code intelligence.
- **Modular Architecture**: Restructured project into modular logic layers.
- **Scope-Resolution Pipeline**: Full scope-chain walks, receiver-bound resolution, C3 MRO, and cross-file import edges (RFC #909).
- **Persistent Graph Store**: SQLite-backed storage with recursive CTEs for graph traversal.
- **Incremental Scanning**: Hash-based change detection, only re-parses changed files.
- **Confidence Scoring**: 9-tier confidence system on all resolution edges.

## [0.6.0-alpha] — 2026-05-04

> *Released on May the Fourth. May the source be with you.*

First public alpha from UnityAILab. Establishes the production architecture:
library + CLI split, schema-versioned JSON output, parallel work-stealing
scan, sane resource defaults, and the security and observability hardening
needed to ship.

### Added
- **Library + CLI split** — `src/lib.rs` exposes a public API
  (`build_graph`, `astar`, `compute_depths`, `bfs_expanded`, `print_tree`,
  `generate_dot`, `build_json_report`) usable from any Rust crate;
  `src/main.rs` is now a thin CLI shell.
- **`--json` output mode** — emits a single, deterministic JSON document on
  stdout (status messages still go to stderr). Schema documented in
  `docs/schema.json` (Draft 7) with a stable `schema_version` field.
- **`--threads all` / `--jobs all`** — explicit "use every core" keyword in
  addition to the existing numeric form.
- **Half-cores default** — when `--threads` is omitted, the scanner uses
  half of the available logical cores rather than all of them.
- **Memory soft-cap on `--no-limit`** — auto-applied at ~½ available RAM
  on Linux to prevent runaway scans. Disable with `--no-mem-cap`.
- **Filename sanitization** — control characters in filenames are replaced
  with `?` at scan time, blocking ANSI-escape injection through hostile
  filenames.
- **stdout / stderr split** — data on stdout, status on stderr; the binary
  is now pipe-friendly (`| jq`, `| head`, etc.).
- **Tests** — 11 unit + integration tests covering algorithms, scanning,
  cap enforcement, sanitization, and JSON roundtrip.
- **README, NOTICE, CHANGELOG, LICENSE, JSON Schema** — full project
  documentation.
- **Sensible aliases** — `find`-, `tree`-, and `du`-style flag aliases
  (e.g. `-L`, `--maxdepth`, `--from`, `--to`, `--jobs`, `--fast`).

### Performance
- **Parallel work-stealing scan** via `crossbeam-deque` — eliminates
  the previous `Mutex<VecDeque>` queue contention.
- **Per-thread local accumulators** — scan-time HashMap inserts are
  contention-free; merging happens once on the main thread.
- **`mimalloc` global allocator** — fast multi-threaded heap.
- **`rustc-hash::FxHashMap`** — drop-in replacement for `std::HashMap`,
  faster on string keys.
- **Aggressive release profile** — `lto = "fat"`, `codegen-units = 1`,
  `panic = "abort"`, `strip = true`.
- **`--tree` mode** — skips per-file `stat()` for ~3–5× cold-cache
  speedup on file-heavy workloads.

### Numbers
On a 12-core machine, `/usr` 50,000-node scan, warm cache:
- 1 thread: 118 ms
- 4 threads: 37 ms
- 12 threads: **28 ms** (5.3× over sequential)

### Notes
- JSON output keys are sorted (`BTreeMap`) for diff-friendly,
  deterministic output across runs.
- The `version` field reports the binary version and bumps on any
  release; the `schema_version` field bumps only on breaking JSON
  format changes. Pin the latter in your consumer.
- `unsafe` code: zero.
