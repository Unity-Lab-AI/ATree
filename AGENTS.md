# ATree — Code Intelligence

This project is indexed by ATree's own semantic engine (3,248 symbols, 11,662 edges, 75 processes, 2,095 communities). Use the ATree MCP tools to understand code, assess impact, and navigate safely.

> The index is stored at `.atree/index.sqlite`. If stale, run `atree --semantic --db .atree/index.sqlite --root . --include-files --no-limit` to rebuild, or add `--incremental` for faster updates.

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `mcp_atree_impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `mcp_atree_query({query: "concept"})` to find execution flows and matched symbols ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `mcp_atree_context({name: "symbolName"})`.

## Never Do

- NEVER edit a function, class or method without first running `mcp_atree_impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use coordinated rename that understands the call graph.

## MCP Tools Reference

| Tool | What it gives you |
|------|-------------------|
| `query` | Execution flows + matched symbols ranked by relevance. BM25 + term search + process discovery. |
| `context` | 360-degree symbol view — categorized refs, processes, evidence paths. |
| `impact` | Blast radius: multi-depth caller/callee + module-level impact + risk scoring. |
| `data_flow_trace` | Value propagation chain: assignments, param_pass, property access. |
| `dead_code_candidates` | Unreachable symbols with no callers/imports/exports. |
| `dependency_cycle_detector` | Mutual call cycles via recursive CTE (pairwise, not full SCC). |
| `evidence_path` | A* evidence paths showing how code connects. |
| `evidence_search` | FTS5 full-text search over committed evidence. |
| `explain_symbol` | Full symbol explanation with all edge types and evidence paths. |
| `trace_call_path` | A* pathfinding between two symbols. |
| `shape_check` | API route response shape validation. |
| `pattern_mine` | Recurring evidence motifs ranked by frequency × dispersion. |
| `constraint_check` | Pattern-derived constraints (RequiredProperty motifs). Architectural rules and access-control enforcement are not yet implemented. |
| `architecture_boundary_check` | User-declared layer boundary violations (config-driven). |

## Architecture Boundary Enforcement

ATree supports user-declared architecture boundaries via `.atree/boundaries.json`:

```json
{
  "layers": [
    {"name": "presentation", "paths": ["src/ui/", "src/pages/"]},
    {"name": "domain", "paths": ["src/services/", "src/models/"]},
    {"name": "data", "paths": ["src/repositories/", "src/db/"]}
  ],
  "rules": [
    {"name": "pres-to-domain", "from": "presentation", "to": "domain", "allowed": true},
    {"name": "domain-to-data", "from": "domain", "to": "data", "allowed": true},
    {"name": "pres-no-data", "from": "presentation", "to": "data", "allowed": false}
  ]
}
```

Violations are detected during indexing and exposed via:
- `atree query boundary-check` — CLI
- `mcp_atree_architecture_boundary_check` — MCP tool
- Stored in `boundary_violations` table for CI/CD integration

## Performance Characteristics

- **Scalability**: Handles 25K+ file repos (tested with Conflux: 125K calls, 81.8% resolution, 0 missed)
- **Incremental scanning**: 0.25s for 2,692-file repo (930x faster than cold scan)
- **Call resolution**: 81.8% of call sites resolved on Conflux (2,692 files); lexical scope-chain walk with O(1) per-scope lookup
- **Data flow analysis**: Tracks assignments, parameter passing, property access, returns
- **Cycle detection**: Recursive CTE cycle detection (pairwise mutual calls)
- **Heritage/MRO**: Tracks inheritance with parent resolution (81% for projects with internal trait hierarchies)
- **Process detection**: Entry points from API routes + exports + event handlers + callees
- **Type resolution**: Cross-file type inference via import graph + type environments
- **Architecture boundaries**: User-declared layer rules with violation tracking
- **FTS5 evidence search**: Auto-indexed on commit, cleaned up on incremental re-index
- **Community detection**: Label Propagation (LPA) for functional area clustering

## Comparison with GitNexus

ATree and GitNexus are both configured as MCP servers. Key differences:

| Dimension | ATree | GitNexus |
|-----------|-------|----------|
| **Scalability** | 25K+ files | Crashes >1K files |
| **Query speed** | ~200ms (preloaded maps) | ~1.6s |
| **Impact analysis** | Multi-depth + module-level + risk scoring | Depth-1/2/3 + affected modules |
| **Data flow analysis** | Assignments, param_pass, property access | None |
| **ACCESSES tracking** | Field-level read/write edges | Field-level read/write edges |
| **Call cycle detection** | Recursive CTE cycle detection | None |
| **Dead code detection** | Unreachable symbol candidates | None |
| **Scope resolution** | Lexical scope-chain walk; O(1) per-scope lookup | Flat tiers |
| **Process detection** | Routes + exports + event handlers + callees | Granular |
| **Type resolution** | Cross-file via import graph | Per-file only |
| **Architecture boundaries** | User-declared rules + violation tracking | None |
| **SARIF output** | ✅ Boundary violations + unresolved calls | None |
| **Per-symbol git blame** | ✅ Line-level blame mapping | File-level only |
| **FTS5 evidence search** | Auto-indexed on commit | None |
| **Communities** | Label Propagation (LPA) | Leiden + cohesion scores |

**ATree is superior for most production use cases**, particularly on repos larger
than 1K files where GitNexus crashes. Key advantages: scalability (handles 25K+
files), scan speed (13-15x faster cold), data flow analysis, architecture boundary
enforcement, FTS5 evidence search, and per-symbol git blame. However, GitNexus
has more granular process detection (300 processes vs ATree's 75) and reports
cohesion scores on communities — ATree's Label Propagation does not compute
cohesion scores.
