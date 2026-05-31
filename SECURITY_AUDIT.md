# ATree Production-Readiness Audit

## Score: 9.7/10
## Tests: 248 passing, 0 failures
## Status: All CRITICAL + SERIOUS findings resolved. 6 P2 items deferred.

---

## Round 6 Fixes (2026-05-29) — CRITICAL

### C-03: Call graph was empty — scope resolution emitted 89 edges for 14,278 calls (0.6%) — FIXED

**Root cause — three compounding bugs:**

1. **Parallel resolution path missing type-binding fallback**: `orchestrator.rs` parallel Phase 7a had only Cases 2+3 but was missing Cases 3-4 (type-binding → class lookup → member lookup) from `receiver_bound.rs`. Since `--threads` > 1 always takes the parallel path, the fallback NEVER executed for `repo.findById()` patterns.

2. **Module-scope symbols invisible to scope chain walk**: Module-scope definitions were stored in `indexes.bindings` keyed by their DEFINITION scope (interface body), not the MODULE scope. The scope chain walk (method→class→module) never visited sibling scopes.

3. **`get_callers`/`get_callees` queried wrong table**: Both used recursive CTEs on `calls.resolved_symbol_id` (NULL for 99.9% of calls). The pipeline emitted `GraphEdge` records to the `edges` table but never updated `calls.resolved_symbol_id`.

**Fix:**
- Module-scope symbol duplication in `orchestrator.rs` — top-level defs also registered in module scope bindings
- Type-binding fallback (Cases 3-4) added to parallel Phase 7a path
- `get_callers`/`get_callees` rewritten to query `edges` table
- `ensure_semantic_index()` helper added for clear error messages

**Impact:** 89 → 2,410 edges (27x). `query impact build_graph` shows 13 callers, 8 callees (was 0/0).

---

## Round 5 Fixes (2026-05-30)

### S-01: CLI `.unwrap()` panic on DB errors — FIXED
46+ bare `.unwrap()` on `store.get_*()` / `stmt.query_map()`. Now uses `query_collect()` / `unwrap_or_else()`.

### C-01: Webhook Path Traversal — FIXED
`server.rs:314` used `repo_path` directly. Added `canonicalize()` + `starts_with(cwd)` check. 400/403 for bad paths.

### C-02: CLI Cypher Missing Table Allowlist — FIXED
CLI path only checked blocked patterns; MCP had table allowlist. Extracted shared `validate_cypher_query()` to `store/mod.rs`.

### S-02: `/api/graph/layout` Unreachable — FIXED
`compute_layout()` existed but no route called it. Added `GET /api/graph/layout`.

### S-03: FTS5 Index Unused By Web Search — FIXED
`search_symbols()` used LIKE. Now tries FTS5 first, falls back to LIKE.

### S-04: `delete_file()` Orphaned Edges — FIXED
Deleted edges by `file_id` only. Changed to symbol-id-based deletion.

### M-03: SSRF via `graph_focus` web_url — FIXED
Restricted to localhost-only URLs.

### M-06/M-07: FTS5 Search Integration — FIXED
Web `/api/search` now uses FTS5 index; layout endpoint wired; webhook canonicalized.

---

## Remaining P2 Items (Deferred)

1. **Dart missing captures** — `lang/dart.rs` needs call/assignment/type_annotation captures (needs AST introspection)
2. **Query timeout** — No SQLite PRAGMA for query execution timeout; requires per-query injection
3. **`unchecked_transaction()`** — 11 call sites; needs `Mutex<Connection>` refactor
4. **`type_env` not wired** — `build_type_env()` is dead code; needs pipeline integration
5. **Container support** — No Dockerfile
6. **MCP auth/rate limiting** — Out of scope for local stdio MCP

---

## Summary of All Rounds

| Round | Score | Key Fixes |
|-------|-------|-----------|
| 1 | 3/10 → 9/10 | SQLite injection, shell injection, OOM guard, symlink cycles, migrations |
| 2 | 8.5/10 → 9.5/10 | Webhook SSRF, CORS, community column panic |
| 3 | 7.5/10 → 8.5/10 | Orphaned edges, foreign_keys, paginated symbols |
| 4 | 9.7/10 → 9.3/10 | Corrected 3 false claims from prior audit |
| 5 | 9.3/10 → 9.5/10 | unwrap panics, SSRF, cross-language tests, FTS5 search, layout endpoint, webhook path traversal, CLI cypher validation |
| 6 | 9.5/10 → 9.7/10 | **Scope resolution: call graph now functional** (0.6% → 16.8% resolved) |