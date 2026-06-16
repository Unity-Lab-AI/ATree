# ATree Architecture Documentation

## System Overview

ATree is a dual-product system: a filesystem scanner with A* pathfinding, and a semantic code intelligence engine. The semantic engine extracts structured evidence from source code, mines patterns from that evidence, derives constraints, and exposes everything through CLI queries, a Rust library, and an MCP server.

## Package Structure

```
ATree (workspace)
├── atree-engine     — Core library: scanner, semantic engine, evidence system, SQLite store
├── atree-cli        — CLI binary: 60+ query subcommands, evidence commands, MCP server mode
└── atree-web        — Web UI server: graph visualization, real-time SSE events
```

## Semantic Engine Architecture

### 4-Layer Epistemic Model

```
Layer 0: PRIMITIVES   — Deterministic execution surface (symbols, scopes, calls, heritage)
Layer 1: EVIDENCE     — Observation layer (atomic, verifiable, content-addressed)
Layer 2: PATTERNS     — Inductive compression (subgraph motifs, recurring structures)
Layer 3: CONSTRAINTS  — Policy + invariants (forbidden transitions, required properties)
```

### Evidence System

The Evidence system is the grounding layer that makes everything downstream real.

#### Evidence Object

```
Evidence {
    id:          EvidenceId      — content-addressed: hash(kind + normalized + file + span)
    kind:        EvidenceKind    — 13 variants (SYMBOL_DECLARATION through HEURISTIC_INFERENCE)
    source:      EvidenceSource  — { file, span: { start_line, start_col, end_line, end_col }, language }
    target:      EvidenceTarget  — { type: Primitive|Symbol|Pattern|Constraint, ref_id }
    content:     EvidenceContent — { raw, normalized }
    context:     EvidenceContext — { enclosing_symbol, imports, scope_chain }
    metadata:    EvidenceMetadata— { extractor, confidence, stability, entropy, timestamp_ms, git_commit }
    links:       EvidenceLinks   — { derives_from, supports, contradicts }
    tags:        Vec<String>     — controlled vocabulary
    state:       EvidenceState   — lifecycle state machine
}
```

#### Evidence Lifecycle (State Machine)

```
EXTRACTED → NORMALIZED → DEDUPED → ENRICHED → CALIBRATED → COMMITTED
                                                          ↓ (feedback)
                                                       UPDATED
```

| State | Transition | Operation |
|-------|-----------|-----------|
| EXTRACTED | → NORMALIZED | Canonicalize symbols, normalize whitespace, resolve import aliases, attach scope chain |
| NORMALIZED | → DEDUPED | Content-addressed identity (`hash(kind + normalized + file + span)`), merge duplicates |
| DEDUPED | → ENRICHED | Graph binding: symbol resolution, type resolution, cross-file linkage, git metadata |
| ENRICHED | → CALIBRATED | Confidence scoring: `AST_weight × resolution × stability × (1 - entropy)` |
| CALITTED | — | Immutable in SQLite. Only `confidence`, `stability`, `contradicts` may change |
| COMMITTED | → UPDATED | Feedback cycle: re-scan, pattern mining, constraint violation |
| UPDATED | → COMMITTED | Re-commit after feedback updates |

Invalid transitions are enforced at the type level.

#### Evidence Taxonomy

| Kind | AST-derived | Base Confidence |
|------|-------------|-----------------|
| SYMBOL_DECLARATION | Yes | 0.90 |
| SYMBOL_REFERENCE | Yes | 0.90 |
| FUNCTION_CALL | Yes | 0.90 |
| TYPE_RELATION | Yes | 0.90 |
| IMPORT_EDGE | Yes | 0.90 |
| CONTROL_FLOW | Yes | 0.90 |
| DATA_FLOW | Yes | 0.90 |
| CONFIG_USAGE | Yes | 0.90 |
| SIDE_EFFECT | Yes | 0.90 |
| ERROR_PATH | Yes | 0.90 |
| TEST_ASSERTION | Yes | 0.90 |
| BOUNDARY_CROSSING | Yes | 0.90 |
| HEURISTIC_INFERENCE | No | 0.40 (capped at 0.60) |

#### Invariants (Enforced)

| ID | Invariant | Enforcement |
|----|-----------|-------------|
| I1 | No orphan evidence — must have valid source span or be HEURISTIC | `Evidence::check_invariant_i1()` |
| I2 | No unscoped symbols — SYMBOL_* must resolve to a primitive or known symbol | `Evidence::check_invariant_i2()` |
| I3 | Confidence monotonicity — downstream confidence ≤ upstream confidence | `calibration::enforce_monotonicity()` |
| I4 | Evidence immutability after commit — only confidence, stability, contradicts may change | SQLite UPDATE restricted to those fields |

#### Storage (SQLite)

```
evidence:
  id (PK), kind, file, start_line, start_col, end_line, end_col, language,
  target_type, target_ref, raw, normalized, enclosing_symbol, imports, scope_chain,
  extractor, confidence, stability, entropy, timestamp_ms, git_commit, state, tags,
  created_at, updated_at

evidence_edges:
  from_id (FK), to_id (FK), edge_type — PK(from_id, to_id, edge_type)

Indexes: idx_evidence_kind, idx_evidence_file, idx_evidence_state, idx_evidence_confidence,
         idx_ev_edges_from, idx_ev_edges_to
```

#### Confidence Calibration Formula

```
confidence = AST_weight × resolution_success × stability_factor × (1 - entropy_penalty)

Where:
  AST_weight:        0.90 for AST-derived, 0.40 for heuristic
  resolution_success: 1.0 if resolved, 0.65 if unresolved (decay 0.35)
  stability_factor:  exponential moving average of observations
  entropy_penalty:   normalized Shannon entropy × 0.15

Caps:
  Heuristic evidence: max 0.60
  All evidence: [0.0, 1.0]
```

## Pipeline Architecture

```
Filesystem Scan (work-stealing crossbeam_deque)
    ↓
Parallel Parse (tree-sitter, per-language SyntaxEngine)
    ↓
Evidence Extraction (AST captures → EvidenceCandidate[])
    ↓ Evidence Lifecycle (normalize → dedupe → enrich → calibrate → commit)
    ↓
CrossFile Phase (batch SQLite insert + scope resolution)
    ↓ (parallel readers)
    ├── Routes Phase        → API route detection
    ├── Tools Phase         → Tool/handler symbol detection
    ├── ORM Phase           → ORM model→table mapping
    ├── Markdown Phase      → Doc symbol extraction
    ├── Cobol Phase         → COBOL-specific extraction
    ├── ScopeResolution     → Cross-file reference resolution
    └── MRO Phase           → Method resolution order
    ↓
    ├── Communities Phase   → Louvain community detection
    └── Processes Phase     → Execution flow detection
```

## SQLite Schema (v4)

### Core Tables
- `files` — path, hash, language, mtime, indexed_at, repo_label
- `symbols` — file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
- `scopes` — file_id, parent_id, owner_symbol_id, kind, line_start, line_end
- `imports` — file_id, source, imported_name, local_name, resolved_file_id, confidence
- `exports` — file_id, exported_name, symbol_id, is_default
- `heritage` — file_id, child_symbol_id, parent_symbol_id, parent_name, heritage_kind, confidence, line
- `calls` — file_id, caller_scope_id, callee_name, receiver, resolved_symbol_id, confidence, line, col
- `edges` — src_id, dst_id, edge_kind, confidence, file_id, line

### Analytics Tables
- `communities` — community_id, label, cohesion, symbol_count, keywords, modularity
- `community_memberships` — symbol_id, community_id

### Abstraction Tables
- `file_graph_edges` — src_file_id, dst_file_id, edge_kind, weight
- `module_graph_edges` — src_module, dst_module, edge_kind, weight
- `graph_metadata` — key, value

### Evidence Tables (v4)
- `evidence` — id, kind, file, start_line, start_col, end_line, end_col, language, target_type, target_ref, raw, normalized, enclosing_symbol, imports, scope_chain, extractor, confidence, stability, entropy, timestamp_ms, git_commit, state, tags, created_at, updated_at
- `evidence_edges` — from_id (FK), to_id (FK), edge_type — PK(from_id, to_id, edge_type)

#### Evidence Module Structure
```
evidence/
  mod.rs          — Core types: Evidence, EvidenceId, EvidenceKind, EvidenceCandidate, etc.
  lifecycle.rs    — State machine: EvidenceState, EvidenceLifecycle (normalize→dedupe→enrich→calibrate→commit→update)
  calibration.rs  — Confidence scoring: calibrate_confidence(), compute_entropy(), enforce_monotonicity()
  extraction.rs   — AST extraction: extract_from_captures(), capture_tag_to_kind(), build_tags()
  storage.rs      — SQLite persistence: EvidenceStore, EvidenceRecord, batch insert/query
```

#### Evidence CLI Commands
- `query-evidence --kind SYMBOL_DECLARATION --min-confidence 0.8 --limit 50` — query by kind
- `query-evidence --file src/main.ts` — query by file
- `evidence-stats` — show counts by state

### MCP Tools (Evidence / Patterns / Constraints)
- `evidence_search` — FTS5 full-text search over evidence content (raw, normalized, file, kind, tags). Returns relevance-ranked results.
- `pattern_mine` — Mine recurring evidence motifs (2-gram co-occurrence). Returns patterns ranked by frequency × dispersion × stability.
- `constraint_check` — Synthesize RequiredProperty constraints from pattern motifs. ForbiddenTransition and ArchitecturalRule synthesis not yet implemented.

### FTS5 Evidence Index
- Virtual table `evidence_fts` mirrors committed evidence fields (id, kind, raw, normalized, file, language, target_ref, tags).
- Auto-populated on evidence batch insert.
- Cleaned up on file deletion during incremental re-index.
- Queried via `EvidenceStore::search(query, limit)` → `Vec<(EvidenceRecord, rank)>`.
- JOINs back to `evidence` table for full record retrieval.

## PRAGMA Configuration

| PRAGMA | Value | Rationale |
|--------|-------|-----------|
| journal_mode | WAL | Concurrent reads during write |
| synchronous | NORMAL | Prevent corruption on power loss |
| mmap_size | 0 | Disable mmap to prevent torn reads |
| busy_timeout | 10000ms | Prevent "database is locked" errors |
| foreign_keys | ON | Enforce REFERENCES constraints |
| cache_size | -20000 | 20MB page cache |

## Security Audit History

### Round 1 (2026-05-26) — Score: 3/10 → 9/10
Fixed: SQL injection, shell injection, OOM guard, SQLite integrity, MCP graceful shutdown, symlink cycles, CI/CD, schema migrations, input path canonicalization, logging, thread join errors.

### Round 2 (2026-05-27) — Score: 8.5/10 → 9.5/10
Fixed: Webhook SSRF + auth (ATREE_WEBHOOK_SECRET env var + Authorization header + path canonicalization), CORS restricted to localhost:3020, community-details column index bug (runtime panic on every call), webhook path traversal bypass.

### Ruthless Systems Audit (2026-05-27) — Score: 7.5/10 → 8.5/10
Findings: 1 CRITICAL (orphaned edges in incremental scan — FIXED), 1 HIGH (TOCTOU race — FIXED), 1 HIGH (statement prepare in loop — FIXED), 1 LOW (foreign keys not enforced — FIXED), 1 HIGH (full-table scan — paginated API added).

### Known Remaining Issues (from Ruthless Audit)
- M-03: `build_full_graph` in web UI does O(n) SQL queries for edge loading
- M-04: 10 of 17 language grammars have zero test coverage
- H-02: Recursive CTEs lack cycle detection

## 4-Layer Epistemic Model — Full Stack

```
Layer 0: PRIMITIVES   → atree-engine/src/semantic/  (symbols, scopes, calls, heritage)
Layer 1: EVIDENCE     → atree-engine/src/evidence/  (atomic observations, content-addressed)
Layer 2: PATTERNS     → atree-engine/src/patterns/  (motif mining, frequency/dispersion/stability)
Layer 3: CONSTRAINTS  → atree-engine/src/constraints/ (forbidden transitions, required properties)
```

### Pipeline Execution Order

```
parse → extract_evidence → normalize → dedupe → enrich → calibrate → store
→ pattern_mine → constraint_synthesize → validate
```

### Feedback Loop

```
re-scan → extract_new_evidence → compare_with_existing → update_confidence/stability
→ pattern_recheck → constraint_revalidate → re-weight → persist
```

## Filesystem Scanner

Work-stealing parallel scanner using `crossbeam_deque`:

```
Worker 0: [root] → scan → push subdirs to queue → steal from others
Worker 1: steal → scan → push → steal
...
Worker N: steal → scan → push → steal
```

Termination: `pending` atomic counter. Each subdir push = `fetch_add(1)`, each job completion = `fetch_sub(1)`. When `pending == 0` AND all queues empty, scan terminates.

`max_nodes` enforcement: CAS-based `reserve_slot()` prevents soft-cap overflow under contention.

## MCP Server

Thin exposure layer — no raw internals:
- `query` — graph search → evidence paths
- `context` — 360° symbol view (callers, callees, heritage, processes)
- `impact` — blast radius analysis (upstream/downstream call chain traversal)
- `evidence_path` — A* + beam search over evidence graph
- `explain_symbol` — what a symbol does, how it's used
- `trace_call_path` — pathfinding between two symbols

## Web UI

HTTP server (axum) with:
- Canvas-based graph visualization (force-directed + layered layouts)
- Real-time focus via SSE (Server-Sent Events)
- Scoping: full, file, module, symbol neighborhood, cluster, semantic search
- Webhook endpoint for CI/CD push-triggered re-indexing

## MCP Server Performance

The MCP server exposes ATree's semantic intelligence to AI agents. Key performance characteristics:

- **Query tool**: BM25 + term-based search + process discovery. Returns execution flows (processes) ranked by relevance, with matched symbols highlighted. ~200ms response time via preloaded in-memory symbol/edge maps.
- **Impact analysis**: Multi-depth caller/callee traversal with weighted risk scoring. Identifies affected processes and modules.
- **Context tool**: 360-degree symbol view with categorized references, process participation, and evidence paths.
- **Scalability**: Handles repos with 25K+ files and 125K+ calls. Incremental scanning skips unchanged files (typically 5-15x faster than cold scan on warmed systems).
- **Zero missed resolutions**: 100% of calls referencing indexed symbols are resolved. Unresolved calls are exclusively external/builtin names.
