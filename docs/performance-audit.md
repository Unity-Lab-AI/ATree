# ATree Performance Audit

## Binary Size

| Binary | Size | Notes |
|--------|------|-------|
| `atree` (CLI) | 59 MB | Includes 16 tree-sitter grammars, SQLite, tokio, rmcp |
| `atree-web` | 61 MB | Above + axum, tower-http, uuid |

Both are statically linked. Using `strip = true` and `lto = thin` in release profile already helps. Binary could be reduced with `opt-level = "s"` (optimize for size) if needed.

## Benchmark Results

Self-indexing the ATree repo (95 source files, 41K LOC Rust):

| Metric | Value |
|--------|-------|
| Total scan time | 3.85s |
| Files scanned | 95 |
| Symbols extracted | 3,149 |
| Calls detected | 14,742 |
| Calls resolved | 1,860 (12.6%) |
| Edges created | 1,881 |
| Threads used | 10 |

### Incremental Scanning

| Repo | Cold | Warm | Speedup |
|------|------|------|---------|
| ATree fixtures (14 files) | 606ms | 526ms | 1.1x |
| GitNexusRelay (2,444 files, 55K symbols) | 158s | 1.2s | 127x |
| qwen-code (2,555 files, 78K symbols) | 102s | — | — |

Incremental scanning is ~127x faster than cold scan on a real-world repo.

## Performance Characteristics

### Strengths
- **Batch SQLite inserts**: All data inserted in single transactions with prepared statements
- **Parallel parsing**: Work-stealing thread pool for multi-file tree-sitter parsing
- **WAL mode SQLite**: Concurrent reads during writes
- **Batched edge lookups**: `get_edges_for_nodes()` and batched BFS in `get_symbol_neighborhood`
- **Incremental scanning**: Only re-indexes changed files, massive speedup
- **Memory protection**: 16MB file size limit, 512MB SQLite heap limit
- **Prepared statement reuse**: Statements prepared once per batch, not per row

### Bottlenecks Identified
1. **Evidence lifecycle**: 5-stage pipeline (normalize → dedupe → enrich → calibrate → commit) processes all evidence in memory. For very large repos (100K+ symbols), this could consume significant RAM.
2. **Scope resolution BFS**: Already batched (this session's work), but still O(depth) queries
3. **Non-streaming `get_all_symbols`/`get_all_files`**: Full table scans used during indexing pipeline phase (acceptable for current use case, not used during MCP serving)
4. **String clones in pipeline**: ~19 `clone()` calls in hot path for path/route/symbol data sharing across threads

### Recommendations for Large Repos (>10K files)
- Use `cargo-limited` wrapper (provided) to cap memory at 8GB and CPU at 6 cores
- Set `jobs = 4` in `~/.cargo/config.toml` (already done)
- Consider `PRAGMA mmap_size = 268435456` (256MB) for large SQLite databases on systems with sufficient RAM
- The `--incremental` flag should always be used for repeated scans
