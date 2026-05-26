# GitNexusRelay → ATree Rust Conversion Tracking

## Overview

Converting GitNexusRelay's TypeScript pipeline into Rust, using ATree as the scanner/topology substrate. The Rust engine adds a typed phase DAG, stable IDs, confidence tiers, EvidencePath, and incremental indexing.

## Architecture

```
GitNexusRelay (TS)                    ATree (rust)
─────────────────                     ─────────────
scan phase          ──────►  process_dir() [DONE]
structure phase     ──────►  build_graph() [DONE]
parse phase         ──────►  SyntaxEngine + ParsedFile [DONE]
routes phase        ──────►  routes/mod.rs [DONE]
tools phase         ──────►  MCP tool detection [DONE]
orm phase           ──────►  (pending)
crossFile phase     ──────►  resolver/ [PARTIAL]
scopeResolution     ──────►  scope_resolution/ [DONE]
mro phase           ──────►  resolver/c3.rs [DONE]
communities phase   ──────►  community/mod.rs [DONE]
processes phase     ──────►  process/mod.rs [DONE]
KnowledgeGraph      ──────►  (pending: in-memory graph)
Pipeline orchestr.  ──────►  (pending: phase DAG runner)
EvidencePath        ──────►  (pending)
```

## Conversion Status

### Type Definitions

| TypeScript Type | Rust Type | Status | Location |
|----------------|-----------|--------|----------|
| `NodeLabel` | `NodeLabel` | ✅ Done | `graph/mod.rs` (as string) |
| `NodeProperties` | `HashMap<String, String>` | ✅ Done | `graph/mod.rs` |
| `GraphNode` | `CodeNode` | ✅ Done | `graph/mod.rs` |
| `GraphRelationship` | `CodeEdge` | ✅ Done | `graph/mod.rs` |
| `RelationshipType` | `String` | ⚠️ Partial | Should be enum |
| `KnowledgeGraph` | `KnowledgeGraph` | ✅ Done | `graph/knowledge.rs` |
| `PipelinePhase` | `PipelinePhase` trait | ✅ Done | `pipeline/mod.rs` |
| `PipelineContext` | `PipelineContext` | ✅ Done | `pipeline/mod.rs` |
| `PipelineProgress` | `PipelineProgress` | ✅ Done | `pipeline/mod.rs` |
| `PipelineResult` | `PipelineResult` | ✅ Done | `pipeline/mod.rs` |
| `AnalyzeOptions` | `PipelineOptions` | ✅ Done | `pipeline/mod.rs` |
| `AnalyzeResult` | `PipelineResult` | ✅ Done | `pipeline/mod.rs` |
| `AnalyzeCallbacks` | (closures) | ✅ Done | `PipelineContext::on_progress` |
| `ScopeId` | `String` | ✅ Done | `scope_resolution/mod.rs` |
| `DefId` | `u64` | ✅ Done | Implicit via Symbol.id |
| `ScopeKind` | `ScopeKind` | ✅ Done | `semantic/mod.rs` |
| `Range` | `tree_sitter::Range` | ✅ Done | Reused from tree-sitter |
| `Capture` | `RawCapture` | ✅ Done | `syntax/mod.rs` |
| `ParsedFile` | `ParsedFile` | ✅ Done | `semantic/mod.rs` |
| `ParsedImport` | `Import` | ✅ Done | `semantic/mod.rs` |
| `ImportEdge` | `Import` | ⚠ Partial | Missing link_status |
| `BindingRef` | (implicit) | ⚠ Partial | Needs provenance tracking |
| `TypeRef` | `TypeBinding` | ✅ Done | `syntax/mod.rs` |
| `Scope` | `Scope` | ✅ Done | `semantic/mod.rs` |
| `Resolution` | (in resolver) | ⚠ Partial | Needs evidence composition |
| `Reference` | `Reference` | ✅ Done | `semantic/mod.rs` |
| `ReferenceIndex` | `ReferenceIndex` | ✅ Done | `semantic/reference_index.rs` |
| `Confidence` | `Confidence` | ✅ Done | `semantic/mod.rs` |
| `EvidenceWeights` | `EvidenceWeights` | ✅ Done | `semantic/evidence.rs` |
| `ClassRegistry` | `lookup_core()` | ✅ Done | `semantic/registries.rs` |
| `MethodRegistry` | `lookup_core()` | ✅ Done | `semantic/registries.rs` |
| `FieldRegistry` | `lookup_core()` | ✅ Done | `semantic/registries.rs` |

### Pipeline Phases

| Phase | Status | Location |
|-------|--------|----------|
| scan | ✅ Done | `lib.rs` `process_dir()` |
| structure | ✅ Done | `lib.rs` `build_graph()` |
| markdown | ✅ Done | `pipeline/phases.rs` — MarkdownPhase |
| cobol | ❌ Missing | Needs COBOL regex tagger |
| parse | ✅ Done | `syntax/mod.rs` + `semantic/mod.rs` |
| routes | ✅ Done | `routes/mod.rs` |
| tools | ✅ Done | MCP tool detection in `mcp.rs` |
| orm | ✅ Done | `pipeline/phases.rs` — OrmPhase |
| crossFile | ⚠ Partial | `resolver/import_resolver.rs` |
| scopeResolution | ✅ Done | `scope_resolution/` + `reference_resolver.rs` (registry-wired) |
| mro | ✅ Done | `resolver/c3.rs` |
| communities | ✅ Done | `community/mod.rs` |
| processes | ✅ Done | `process/mod.rs` |

### Infrastructure

| Component | Status | Notes |
|-----------|--------|-------|
| Phase DAG runner | ✅ Done | `pipeline/mod.rs` — Kahn's algorithm |
| Stable IDs | ⚠ Partial | File IDs in SQLite, symbol IDs in ParsedFile |
| Incremental indexing | ✅ Done | `build_graph_incremental()` |
| BM25 search | ✅ Done | `search/mod.rs` |
| Hybrid search | ✅ Done | `search/mod.rs` |
| Embeddings | ⚠ Partial | `embeddings/mod.rs` (fastembed) |
| EvidencePath | ✅ Done | `evidence.rs` — A* + beam traversal |
| Confidence tiers | ✅ Done | `Confidence` enum with scores |

## Priority Order

1. ~~**KnowledgeGraph in-memory graph**~~ ✅
2. ~~**Pipeline DAG runner**~~ ✅
3. ~~**EvidencePath**~~ ✅
4. ~~**ORM phase**~~ ✅
5. ~~**Markdown phase**~~ ✅
6. **COBOL phase** — niche but needed for completeness
7. ~~**ReferenceIndex**~~ ✅
8. ~~**Evidence weights**~~ ✅
