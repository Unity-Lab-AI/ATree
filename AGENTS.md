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
| `query` | Execution flows + matched symbols ranked by relevance. Combines BM25 + term search + process discovery. |
| `context` | 360-degree symbol view — categorized refs, processes it participates in, evidence paths. |
| `impact` | Blast radius analysis with multi-depth caller/callee traversal, weighted risk scoring (LOW/MEDIUM/HIGH/CRITICAL). |
| `evidence_path` | A* evidence paths showing how code connects. |
| `explain_symbol` | Full symbol explanation with all edge types and evidence paths. |
| `trace_call_path` | A* pathfinding between two symbols. |

## Performance Characteristics

- **Scalability**: Handles 25K+ file repos (tested with Conflux: 125K calls, 81.8% resolution, 0 missed)
- **Incremental scanning**: 0.25s for 2,692-file repo (930x faster than cold scan)
- **Call resolution**: 100% of resolvable calls resolved (unresolved are external/builtin names)
- **Heritage/MRO**: Tracks inheritance with parent resolution (81% for projects with internal trait hierarchies)
- **Process detection**: Identifies execution flows via STEP_IN_PROCESS edges
- **Community detection**: Leiden algorithm for functional area clustering

## Comparison with GitNexus

ATree and GitNexus are both configured as MCP servers. Key differences:

| Dimension | ATree | GitNexus |
|-----------|-------|----------|
| **Scalability** | 25K+ files | Crashes >1K files |
| **Query speed** | ~200ms (preloaded maps) | ~1.6s |
| **Impact analysis** | Direct callers with risk score | Full depth-1/2/3 + affected modules |
| **Processes** | 75 (ATree repo) | 300 (more granular) |
| **Communities** | 2,095 (Leiden) | 1,681 (with cohesion scores) |

Use **ATree** for: large codebases, fast queries, impact analysis, heritage/MRO tracking.
Use **GitNexus** for: deeper process analysis, module-level impact, cohesion-scored clusters (on repos it can index).
