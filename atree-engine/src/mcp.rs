//! MCP (Model Context Protocol) Server
//!
//! Exposes ATree's semantic code intelligence as MCP tools.
//!
//! ## Two modes
//!
//! 1. **CLI subprocess** (legacy): Spawns `atree` binary for each tool call.
//!    Used for tools that don't have in-process support yet.
//!
//! 2. **In-process evidence** (preferred): Uses the evidence bundle layer
//!    directly — no subprocess, token-bounded, confidence-ranked results.
//!    Used for: `query`, `context`, `evidence_path`, `explain_symbol`,
//!    `trace_call_path`, `impact`.

use rmcp::{
    model::*,
    ServerHandler,
    service::{RequestContext, ServiceExt},
    ErrorData,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// =====================================================================
// Tool Input Types
// =====================================================================

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ListReposInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct IndexInput { pub path: Option<String>, #[serde(default)] pub force: bool, #[serde(default)] pub embeddings: bool, pub name: Option<String> }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct QueryInput { pub query: String, pub task_context: Option<String>, pub goal: Option<String>, #[serde(default = "dl")] pub max_seeds: u32, #[serde(default = "dms")] pub max_symbols: u32, #[serde(default)] pub include_content: bool }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ContextInput { pub name: Option<String>, pub kind: Option<String>, #[serde(default)] pub include_content: bool }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ImpactInput { pub target: String, pub direction: String, pub kind: Option<String>, #[serde(default = "dd")] pub max_depth: u32 }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct DetectChangesInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct RenameInput { pub symbol_name: Option<String>, pub new_name: String, #[serde(default = "ddr")] pub dry_run: bool }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct CypherInput { pub query: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct RouteMapInput { pub route: Option<String> }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ShapeCheckInput { pub route: Option<String> }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ToolMapInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ApiImpactInput { pub route: Option<String>, pub file: Option<String> }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct VerifyInput { #[serde(default = "dvt")] pub verify_type: String, pub command: Option<String> }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct GroupListInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct GroupSyncInput { pub name: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ExplainInput { pub symbol: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct EntrypointsInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct TracePathInput { pub from: String, pub to: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct PublicApiInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct AffectedTestsInput { pub symbol: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ValidationPlanInput { pub symbol: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ContractChangesInput { pub base_ref: Option<String> }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct BoundaryCheckInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ScopeViolationsInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ConfigMapInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ImpactByKindInput { pub target: String, pub kind: String, pub direction: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct SemanticDiffInput { pub base_ref: Option<String> }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct SideEffectsInput { pub symbol: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ChangeCouplingInput { pub symbol: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ConcurrencyInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct EditScopeInput { pub symbol: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct IssueLocatorInput { pub issue_id: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct DocsDriftInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct RenameSafetyInput { pub symbol_name: String, pub new_name: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct DeadCodeInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct HotspotsInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ErrorTraceInput { pub symbol: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ResourceLifecycleInput { pub symbol: String }
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct DepCyclesInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct UncoveredInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct ResolutionStatsInput {}
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)] pub struct EvidencePathInput { pub query: String, #[serde(default = "dd")] pub max_depth: u32, #[serde(default = "dbw")] pub beam_width: u32, #[serde(default = "dme")] pub max_evidence: u32, #[serde(default)] pub include_content: bool, pub task_context: Option<String>, pub goal: Option<String> }

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct GraphFocusInput { pub node_ids: Vec<String>, #[serde(default = "gfl")] pub label: String, #[serde(default = "gfs")] pub source: String, #[serde(default)] pub zoom: Option<f64>, #[serde(default = "gfd")] pub anim_duration_ms: Option<u64>, pub web_url: Option<String> }

fn gfl() -> String { "Agent focus".to_string() }
fn gfs() -> String { "mcp_tool".to_string() }
fn gfd() -> Option<u64> { Some(600) }

fn dl() -> u32 { 5 } fn dms() -> u32 { 10 } fn dd() -> u32 { 3 }
fn ddr() -> bool { true } fn dvt() -> String { "all".to_string() }
fn dbw() -> u32 { 5 } fn dme() -> u32 { 10 }
fn dsl() -> u32 { 20 }
fn dmp() -> u32 { 3 }

// ── New tool input types (evidence/patterns/constraints) ─────────────────────

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct EvidenceSearchInput { pub query: String, #[serde(default = "dsl")] pub limit: u32, pub kind: Option<String>, pub file: Option<String>, #[serde(default)] pub min_confidence: Option<f64> }

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct PatternMineInput { #[serde(default = "dmp")] pub min_frequency: u32, #[serde(default)] pub max_patterns: Option<u32> }

#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct ConstraintCheckInput { pub symbol: Option<String>, pub kind: Option<String> }

// =====================================================================
// MCP Server
// =====================================================================

#[derive(Debug, Clone)]
pub struct ATreeMcpServer {
    pub atree_bin: String,
    pub db_path: Option<PathBuf>,
    /// When true, use in-process evidence bundles instead of CLI subprocess.
    pub use_evidence: bool,
}

impl ATreeMcpServer {
    pub fn new(atree_bin: Option<String>, db_path: Option<PathBuf>) -> Self {
        Self {
            atree_bin: atree_bin.unwrap_or_else(|| "atree".to_string()),
            db_path,
            use_evidence: true,
        }
    }

    /// Open a GraphStore from the configured db_path.
    /// Falls back to `.atree/index.sqlite` in the current directory if no path is configured.
    fn open_store(&self) -> Result<crate::store::GraphStore, ErrorData> {
        let path = self.db_path.clone().or_else(|| {
            let default = std::path::PathBuf::from(".atree/index.sqlite");
            if default.exists() { Some(default) } else { None }
        }).ok_or_else(|| ErrorData::internal_error(
            "No db_path configured and .atree/index.sqlite not found. Pass --db <path> or run from a project with an index.".to_string(),
            None,
        ))?;
        crate::store::GraphStore::open(&path)
            .map_err(|e| ErrorData::internal_error(format!("Failed to open store: {}", e), None))
    }

    fn run_atree(&self, args: &[&str]) -> Result<String, ErrorData> {
        let mut cmd = std::process::Command::new(&self.atree_bin);
        cmd.args(args);
        if let Some(ref db) = self.db_path { cmd.env("ATREE_DB", db); }
        let output = cmd.output()
            .map_err(|e| ErrorData::internal_error(format!("Failed to run atree: {}", e), None))?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !output.status.success() {
            return Err(ErrorData::internal_error(format!(
                "atree exited with code {:?}\nstderr: {}", output.status.code(), stderr
            ), None));
        }
        Ok(stdout)
    }

    fn tool(name: &str, desc: &str, input_schema: serde_json::Value) -> Tool {
        let schema: JsonObject = serde_json::from_value(input_schema).unwrap_or_default();
        Tool::new(name.to_string(), desc.to_string(), std::sync::Arc::new(schema))
    }

    // ── In-process evidence handlers ───────────────────────────────────

    /// Handle the `query` tool using evidence bundles.
    fn handle_query_evidence(&self, input: QueryInput) -> Result<String, ErrorData> {
        let store = self.open_store()?;

        // Ensure the search index exists.
        crate::search::init_search_index(&store)
            .map_err(|e| ErrorData::internal_error(format!("Search index init failed: {}", e), None))?;

        // Build an enriched query from the raw query + task_context + goal.
        let enriched_query = build_enriched_query(&input.query, input.task_context.as_deref(), input.goal.as_deref());

        // When include_content is true, increase the token budget to make
        // room for code snippets (~80 tokens per snippet).
        let token_budget = if input.include_content { 5000 } else { 3000 };

        let evidence_config = crate::evidence_path::EvidenceConfig {
            max_seeds: input.max_seeds as usize,
            beam_width: 5,
            max_depth: 4,
            max_evidence: input.max_symbols as usize,
            token_budget,
            ..Default::default()
        };

        // Format with or without code content.
        let text = if input.include_content {
            let bundle = crate::evidence_bundle::query_evidence_with_content(&store, &enriched_query, &evidence_config)
                .map_err(|e| ErrorData::internal_error(format!("Evidence query failed: {}", e), None))?;
            // Resolve repo root: db_path is typically <repo>/.atree/index.sqlite,
            // so the repo root is the grandparent of the db file.
            let repo_root = self.db_path.as_ref()
                .and_then(|p| p.parent())           // <repo>/.atree/
                .and_then(|p| p.parent())            // <repo>/
                .unwrap_or_else(|| std::path::Path::new("."));
            crate::evidence_bundle::format_bundle_with_content(&bundle, repo_root, 5)
        } else {
            let bundle = crate::evidence_bundle::query_evidence(&store, &enriched_query, &evidence_config)
                .map_err(|e| ErrorData::internal_error(format!("Evidence query failed: {}", e), None))?;
            crate::evidence_bundle::format_bundle_as_text(&bundle)
        };

        // Prepend context/goal as structured metadata.
        let mut out = text;
        if input.task_context.is_some() || input.goal.is_some() {
            out = format!(
                "Query: {}{}{}\n\n{}",
                input.query,
                input.task_context.as_ref()
                    .map(|c| format!(" | Context: {}", c))
                    .unwrap_or_default(),
                input.goal.as_ref()
                    .map(|g| format!(" | Goal: {}", g))
                    .unwrap_or_default(),
                out,
            );
        }

        Ok(out)
    }

    /// Handle the `context` tool using evidence bundles.
    fn handle_context_evidence(&self, input: ContextInput) -> Result<String, ErrorData> {
        let store = self.open_store()?;

        let name = input.name.as_deref()
            .ok_or_else(|| ErrorData::invalid_params("Missing 'name'".to_string(), None))?;

        let kind_filter = input.kind.as_deref();

        // Build the knowledge graph once; reuse for both context and evidence paths.
        let graph = crate::graph::KnowledgeGraph::from_store(&store)
            .map_err(|e| ErrorData::internal_error(format!("Graph build failed: {}", e), None))?;

        // Part 1: Symbol context (callers, callees, inheritance, community, process).
        let mut text = crate::evidence_bundle::format_symbol_context(&store, &graph, name, kind_filter)
            .map_err(|e| ErrorData::internal_error(format!("Context query failed: {}", e), None))?;

        // Part 2: Evidence paths showing how this symbol connects to the codebase.
        let evidence_config = crate::evidence_path::EvidenceConfig {
            max_seeds: 10,
            beam_width: 5,
            max_depth: 4,
            max_evidence: 8,
            token_budget: if input.include_content { 5000 } else { 3000 },
            ..Default::default()
        };

        let bundle = if input.include_content {
            crate::evidence_bundle::query_evidence_with_graph_and_content(&store, &graph, name, &evidence_config)
        } else {
            crate::evidence_bundle::query_evidence_with_graph(&store, &graph, name, &evidence_config)
        }.map_err(|e| ErrorData::internal_error(format!("Context evidence failed: {}", e), None))?;

        if !bundle.is_empty() {
            text.push_str("\n── Evidence Paths ──────────────────────────────────\n\n");
            if input.include_content {
                let repo_root = self.db_path.as_ref()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.parent())
                    .unwrap_or_else(|| std::path::Path::new("."));
                text.push_str(&crate::evidence_bundle::format_bundle_with_content(&bundle, repo_root, 3));
            } else {
                text.push_str(&crate::evidence_bundle::format_bundle_as_text(&bundle));
            }
        }

        Ok(text)
    }

    /// Handle the `evidence_path` tool using evidence bundles.
    fn handle_evidence_path_evidence(&self, input: EvidencePathInput) -> Result<String, ErrorData> {
        let store = self.open_store()?;

        let enriched_query = build_enriched_query(&input.query, input.task_context.as_deref(), input.goal.as_deref());

        let evidence_config = crate::evidence_path::EvidenceConfig {
            max_seeds: 10,
            beam_width: input.beam_width as usize,
            max_depth: input.max_depth as usize,
            max_evidence: input.max_evidence as usize,
            token_budget: if input.include_content { 6000 } else { 4000 },
            ..Default::default()
        };

        let bundle = crate::evidence_bundle::query_evidence(&store, &enriched_query, &evidence_config)
            .map_err(|e| ErrorData::internal_error(format!("Evidence path query failed: {}", e), None))?;

        let text = if input.include_content {
            let repo_root = self.db_path.as_ref()
                .and_then(|p| p.parent())
                .and_then(|p| p.parent())
                .unwrap_or_else(|| std::path::Path::new("."));
            crate::evidence_bundle::format_bundle_with_content(&bundle, repo_root, 3)
        } else {
            crate::evidence_bundle::format_bundle_as_text(&bundle)
        };

        Ok(text)
    }

    /// Handle the `explain_symbol` tool using evidence bundles.
    fn handle_explain_evidence(&self, input: ExplainInput) -> Result<String, ErrorData> {
        let store = self.open_store()?;

        // Build the knowledge graph once; reuse for context + evidence paths.
        let graph = crate::graph::KnowledgeGraph::from_store(&store)
            .map_err(|e| ErrorData::internal_error(format!("Graph build failed: {}", e), None))?;

        // Part 1: Symbol context (all edge types, community, process — all from graph).
        let mut text = crate::evidence_bundle::format_symbol_context(&store, &graph, &input.symbol, None)
            .map_err(|e| ErrorData::internal_error(format!("Explain failed: {}", e), None))?;

        // Part 2: Evidence paths originating from this symbol (reuse the graph).
        let evidence_config = crate::evidence_path::EvidenceConfig {
            max_seeds: 10,
            beam_width: 5,
            max_depth: 4,
            max_evidence: 8,
            token_budget: 3000,
            ..Default::default()
        };

        let bundle = crate::evidence_bundle::query_evidence_with_graph(&store, &graph, &input.symbol, &evidence_config)
            .map_err(|e| ErrorData::internal_error(format!("Explain evidence failed: {}", e), None))?;

        if !bundle.is_empty() {
            text.push_str("\n── Evidence Paths ──────────────────────────────────\n\n");
            text.push_str(&crate::evidence_bundle::format_bundle_as_text(&bundle));
        }

        Ok(text)
    }

    /// Handle the `impact` tool using in-process analysis.
    fn handle_impact_evidence(&self, input: ImpactInput) -> Result<String, ErrorData> {
        let store = self.open_store()?;

        let evidence_config = crate::evidence_path::EvidenceConfig {
            max_seeds: 5,
            beam_width: 3,
            max_depth: 4,
            max_evidence: 3,
            token_budget: 3000,
            ..Default::default()
        };

        let depth = input.max_depth as usize;
        let direction = if input.direction.is_empty() { None } else { Some(input.direction.as_str()) };
        let kind = if input.kind.as_ref().map_or(true, |k| k.is_empty()) { None } else { input.kind.as_deref() };
        let text = crate::evidence_bundle::impact_evidence(&store, &input.target, depth, &evidence_config, direction, kind)
            .map_err(|e| ErrorData::internal_error(format!("Impact analysis failed: {}", e), None))?;

        Ok(text)
    }

    /// Handle the `trace_call_path` tool using A* pathfinding.
    fn handle_trace_path_evidence(&self, input: TracePathInput) -> Result<String, ErrorData> {
        let store = self.open_store()?;

        let evidence_config = crate::evidence_path::EvidenceConfig {
            max_seeds: 5,
            beam_width: 5,
            max_depth: 6,
            max_evidence: 5,
            token_budget: 4000,
            ..Default::default()
        };

        let text = crate::evidence_bundle::trace_path_evidence(&store, &input.from, &input.to, &evidence_config)
            .map_err(|e| ErrorData::internal_error(format!("Trace path failed: {}", e), None))?;

        Ok(text)
    }

    /// Handle the `graph_focus` tool — POST to the ATree web server to shift visual focus.
    fn handle_graph_focus(&self, input: GraphFocusInput) -> Result<String, ErrorData> {
        let web_url = input.web_url.as_deref().unwrap_or("http://localhost:3020");

        // Resolve symbol names to node IDs using the store
        let store = self.open_store()?;
        let mut resolved_node_ids = Vec::new();

        for node_id in &input.node_ids {
            // Try as-is first (may already be a node ID like "sym:123")
            if node_id.starts_with("sym:") || node_id.starts_with("file:") {
                resolved_node_ids.push(node_id.clone());
                continue;
            }
            // Try to resolve as a symbol name
            if let Ok(syms) = store.get_symbols_by_name(node_id) {
                for sym in &syms {
                    resolved_node_ids.push(format!("sym:{}", sym.id));
                }
            }
        }

        if resolved_node_ids.is_empty() {
            return Ok("No matching nodes found for the given identifiers.".to_string());
        }

        // Build the focus event payload
        let payload = serde_json::json!({
            "event_type": "focus_node",
            "node_ids": resolved_node_ids,
            "label": input.label,
            "source": input.source,
            "zoom": input.zoom,
            "anim_duration_ms": input.anim_duration_ms,
        });

        // POST to the web server
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .map_err(|e| ErrorData::internal_error(format!("HTTP client error: {}", e), None))?;

        let resp = client
            .post(format!("{}/api/graph/focus", web_url))
            .json(&payload)
            .send()
            .map_err(|e| ErrorData::internal_error(format!("Failed to connect to ATree web at {}: {}", web_url, e), None))?;

        if resp.status().is_success() {
            let result: serde_json::Value = resp.json()
                .map_err(|e| ErrorData::internal_error(format!("Bad response: {}", e), None))?;
            let recipients = result.get("recipients").and_then(|v| v.as_u64()).unwrap_or(0);
            Ok(format!(
                "Graph focus shifted to {} node(s). {} browser(s) updated.\nOpen {} to see the visual graph.",
                resolved_node_ids.len(), recipients, web_url
            ))
        } else {
            Err(ErrorData::internal_error(format!(
                "ATree web server returned {}: {}",
                resp.status(),
                resp.text().unwrap_or_default()
            ), None))
        }
    }
}

// =====================================================================
// ServerHandler implementation
// =====================================================================

impl ServerHandler for ATreeMcpServer {
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<CallToolResult, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        let db = self.db_path.as_ref().map(|p| p.to_str().unwrap_or(".atree/index.sqlite"));

        // ── In-process evidence tools (preferred path) ──────────────────
        if self.use_evidence {
            let result = match request.name.as_ref() {
                "query" => {
                    let input: QueryInput = serde_json::from_value(serde_json::Value::Object(args))
                        .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                    self.handle_query_evidence(input)?
                }
                "context" => {
                    let input: ContextInput = serde_json::from_value(serde_json::Value::Object(args))
                        .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                    self.handle_context_evidence(input)?
                }
                "evidence_path" => {
                    let input: EvidencePathInput = serde_json::from_value(serde_json::Value::Object(args))
                        .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                    self.handle_evidence_path_evidence(input)?
                }
                "explain_symbol" => {
                    let input: ExplainInput = serde_json::from_value(serde_json::Value::Object(args))
                        .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                    self.handle_explain_evidence(input)?
                }
                "trace_call_path" => {
                    let input: TracePathInput = serde_json::from_value(serde_json::Value::Object(args))
                        .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                    self.handle_trace_path_evidence(input)?
                }
                "impact" => {
                    let input: ImpactInput = serde_json::from_value(serde_json::Value::Object(args))
                        .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                    self.handle_impact_evidence(input)?
                }
                "graph_focus" => {
                    let input: GraphFocusInput = serde_json::from_value(serde_json::Value::Object(args))
                        .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                    self.handle_graph_focus(input)?
                }
                // Fall through to CLI subprocess for all other tools.
                _ => {
                    return self.call_tool_cli(request.name.as_ref(), args, db);
                }
            };
            return Ok(CallToolResult::success(vec![Content::text(result)]));
        }

        // ── CLI subprocess path (legacy) ───────────────────────────────
        self.call_tool_cli(request.name.as_ref(), args, db)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        macro_rules! schema_for {
            ($t:ty) => {{
                let s = schemars::schema_for!($t);
                serde_json::to_value(s).unwrap_or_default()
            }};
        }
        let tools: Vec<Tool> = vec![
            Self::tool("list_repos", "List all indexed repositories.", schema_for!(ListReposInput)),
            Self::tool("index", "Index a repository (full analysis).", schema_for!(IndexInput)),
            Self::tool("query", "Query the code knowledge graph for execution flows. Returns token-bounded, confidence-ranked evidence paths.", schema_for!(QueryInput)),
            Self::tool("context", "360-degree view of a symbol — all edge types (calls, imports, inheritance, data flow, structure, behavior, community) with confidence scores and evidence paths.", schema_for!(ContextInput)),
            Self::tool("impact", "Blast radius analysis with multi-depth caller/callee traversal, weighted risk scoring (LOW/MEDIUM/HIGH/CRITICAL), and evidence paths showing why callers matter.", schema_for!(ImpactInput)),
            Self::tool("detect_changes", "Analyze uncommitted git changes and find affected symbols.", schema_for!(DetectChangesInput)),
            Self::tool("rename", "Multi-file coordinated rename using the knowledge graph.", schema_for!(RenameInput)),
            Self::tool("cypher", "Execute a raw Cypher-like query against the code knowledge graph.", schema_for!(CypherInput)),
            Self::tool("route_map", "Show API route mappings.", schema_for!(RouteMapInput)),
            Self::tool("shape_check", "Check response shapes for API routes.", schema_for!(ShapeCheckInput)),
            Self::tool("tool_map", "Show tool-like symbols in the codebase.", schema_for!(ToolMapInput)),
            Self::tool("api_impact", "Pre-change impact report for an API route handler.", schema_for!(ApiImpactInput)),
            Self::tool("verify", "Run project verification: tests, linter, type-checker.", schema_for!(VerifyInput)),
            Self::tool("group_list", "List all configured repository groups.", schema_for!(GroupListInput)),
            Self::tool("group_sync", "Rebuild the Contract Registry for a group.", schema_for!(GroupSyncInput)),
            Self::tool("explain_symbol", "Full symbol explanation — all edge types, process/community membership, and A* evidence paths with confidence scores.", schema_for!(ExplainInput)),
            Self::tool("find_entrypoints", "Find entry points — main functions, exported handlers.", schema_for!(EntrypointsInput)),
            Self::tool("trace_call_path", "A* pathfinding between two symbols across all edge types. Returns ranked paths with direction arrows, edge types, and quality signals.", schema_for!(TracePathInput)),
            Self::tool("public_api_surface", "List the public API surface.", schema_for!(PublicApiInput)),
            Self::tool("affected_tests", "Find tests affected by changes to a symbol.", schema_for!(AffectedTestsInput)),
            Self::tool("validation_plan", "Generate a validation plan for a proposed change.", schema_for!(ValidationPlanInput)),
            Self::tool("contract_change_detector", "Detect changes to API contracts between versions.", schema_for!(ContractChangesInput)),
            Self::tool("architecture_boundary_check", "Check for architecture boundary violations.", schema_for!(BoundaryCheckInput)),
            Self::tool("scope_violation_detector", "Detect scope violations — private symbols used externally.", schema_for!(ScopeViolationsInput)),
            Self::tool("config_surface_map", "Map configuration surface — env vars, config keys.", schema_for!(ConfigMapInput)),
            Self::tool("impact_by_symbol_kind", "Impact analysis filtered by symbol kind.", schema_for!(ImpactByKindInput)),
            Self::tool("semantic_diff_summary", "Summarize semantic differences between versions.", schema_for!(SemanticDiffInput)),
            Self::tool("side_effect_scanner", "Scan a symbol for side effects — I/O, global state.", schema_for!(SideEffectsInput)),
            Self::tool("change_coupling", "Find symbols that are change-coupled.", schema_for!(ChangeCouplingInput)),
            Self::tool("concurrency_surface_detector", "Detect concurrency surface — async, locks.", schema_for!(ConcurrencyInput)),
            Self::tool("minimal_edit_scope", "Find the minimal set of files to change.", schema_for!(EditScopeInput)),
            Self::tool("issue_to_code_locator", "Map an issue/ticket ID to code locations.", schema_for!(IssueLocatorInput)),
            Self::tool("docs_drift_detector", "Detect drift between documentation and code.", schema_for!(DocsDriftInput)),
            Self::tool("rename_safety_check", "Check if a rename is safe.", schema_for!(RenameSafetyInput)),
            Self::tool("dead_code_candidates", "Find potential dead code — no callers, not exported.", schema_for!(DeadCodeInput)),
            Self::tool("ownership_hotspots", "Find ownership hotspots — high fan-in/fan-out.", schema_for!(HotspotsInput)),
            Self::tool("error_path_trace", "Trace error paths from a symbol.", schema_for!(ErrorTraceInput)),
            Self::tool("resource_lifecycle_map", "Map resource lifecycle — allocation, usage, cleanup.", schema_for!(ResourceLifecycleInput)),
            Self::tool("dependency_cycle_detector", "Detect dependency cycles in the call graph.", schema_for!(DepCyclesInput)),
            Self::tool("find_uncovered_symbols", "Find symbols with no test coverage.", schema_for!(UncoveredInput)),
            Self::tool("resolution_stats", "Show resolution quality stats — call/import resolution rates per language, top unresolved patterns, and confidence distribution.", schema_for!(ResolutionStatsInput)),
            Self::tool("evidence_path", "Find evidence paths for a query using A* + beam search over the layered code graph. Returns token-bounded, confidence-ranked evidence paths.", schema_for!(EvidencePathInput)),
            Self::tool("evidence_search", "Full-text search over committed evidence. Searches raw content, normalized text, file paths, kinds, and tags using FTS5. Returns matching evidence with confidence scores and relevance ranks. Use for: 'find all function calls related to X', 'show evidence in file Y', 'high-confidence type relations'.", schema_for!(EvidenceSearchInput)),
            Self::tool("pattern_mine", "Mine recurring patterns from the evidence graph. Extracts motifs (co-occurring evidence kinds) ranked by frequency × dispersion × stability. Returns patterns with evidence IDs and composite scores. Use for: 'what call patterns are common', 'show architectural motifs', 'find recurring import-declaration-call chains'.", schema_for!(PatternMineInput)),
            Self::tool("constraint_check", "Synthesize and check constraints from evidence patterns. Detects forbidden transitions (evidence contradictions), required properties (stable pattern components), and architectural violations. Returns active constraints with confidence and violation counts. Use for: 'what rules emerge from the codebase', 'check if symbol X violates constraints', 'show architectural invariants'.", schema_for!(ConstraintCheckInput)),
            Self::tool("graph_focus", "Shift the visual graph focus to specific nodes in real-time. Triggers a smooth camera animation on the ATree web UI and highlights the target nodes. Use after query/context/impact to show the agent what it found visually.", schema_for!(GraphFocusInput)),
        ];
        Ok(ListToolsResult { tools, next_cursor: None, meta: None })
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            rmcp::model::ServerCapabilities::builder().enable_tools().build(),
        )
    }
}

// ── Query enrichment ────────────────────────────────────────────────────────

/// Build an enriched search query by combining the raw query with
/// task context and goal. This makes context/goal influence FTS5 seed
/// selection instead of being passive labels.
///
/// Strategy: extract key terms from context/goal and append them to the
/// query so FTS5 can match additional relevant seeds. We keep the original
/// query terms (they're the primary signal) and add context terms as
/// optional expansions.
/// Noise words to skip when enriching queries from context/goal text.
const STOP_WORDS: &[&str] = &[
    "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
    "have", "has", "had", "do", "does", "did", "will", "would", "could",
    "should", "may", "might", "shall", "can", "need", "dare", "ought",
    "used", "to", "of", "in", "for", "on", "with", "at", "by", "from",
    "as", "into", "through", "during", "before", "after", "above", "below",
    "between", "out", "off", "over", "under", "again", "further", "then",
    "once", "here", "there", "when", "where", "why", "how", "all", "both",
    "each", "few", "more", "most", "other", "some", "such", "no", "nor",
    "not", "only", "own", "same", "so", "than", "too", "very", "just",
    "because", "but", "and", "or", "if", "while", "about", "up", "down",
    "adding", "adding", "want", "like", "get", "got", "make", "made",
    "using", "find", "found", "look", "looking", "check", "checking",
];

fn build_enriched_query(query: &str, task_context: Option<&str>, goal: Option<&str>) -> String {
    let mut terms: Vec<String> = query.split_whitespace().map(String::from).collect();
    let mut seen: rustc_hash::FxHashSet<String> = terms.iter().cloned().collect();

    // Extract terms from task_context (lower weight — added once).
    if let Some(text) = task_context {
        for word in text.split_whitespace() {
            let cleaned = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            if cleaned.len() >= 3
                && !seen.contains(&cleaned)
                && !STOP_WORDS.contains(&cleaned.as_str())
            {
                seen.insert(cleaned.clone());
                terms.push(cleaned);
            }
        }
    }

    // Extract terms from goal (higher weight — added once; the evidence engine's
    // heuristic already scores goal-matched nodes higher via text_relevance).
    // Note: duplicating terms in the FTS5 OR query doesn't change the result set
    // since FTS5 deduplicates MATCH terms. The real goal-weighting happens in
    // the A* heuristic, not in seed selection.
    if let Some(text) = goal {
        for word in text.split_whitespace() {
            let cleaned = word.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
            if cleaned.len() >= 3
                && !seen.contains(&cleaned)
                && !STOP_WORDS.contains(&cleaned.as_str())
            {
                seen.insert(cleaned.clone());
                terms.push(cleaned);
            }
        }
    }

    terms.join(" ")
}

// ── CLI subprocess fallback ─────────────────────────────────────────────────

impl ATreeMcpServer {
    /// Handle tool calls via CLI subprocess (legacy path).
    fn call_tool_cli(
        &self,
        name: &str,
        args: serde_json::Map<String, serde_json::Value>,
        db: Option<&str>,
    ) -> Result<CallToolResult, ErrorData> {
        let result = match name {
            "list_repos" => self.run_atree(&["query", "repos"])?,
            "index" => {
                let input: IndexInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let path = input.path.as_deref().unwrap_or(".");
                let mut a = vec!["--semantic", "--root", path];
                if input.force { a.push("--force"); }
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "query" => {
                let input: QueryInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "search", &input.query];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "context" => {
                let input: ContextInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let name = input.name.as_deref()
                    .ok_or_else(|| ErrorData::invalid_params("Missing 'name'".to_string(), None))?;
                let mut a = vec!["query", "context", name];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "impact" => {
                let input: ImpactInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "impact", &input.target, "--direction", &input.direction];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "detect_changes" => {
                let mut a = vec!["query", "detect-changes"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "rename" => {
                let input: RenameInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let sn = input.symbol_name.as_deref()
                    .ok_or_else(|| ErrorData::invalid_params("Missing 'symbol_name'".to_string(), None))?;
                let mut a = vec!["query", "rename", sn, "--new-name", &input.new_name];
                if !input.dry_run { a.push("--apply"); }
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "cypher" => {
                let input: CypherInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                // Validate the query against an allowlist of permitted tables and columns.
                validate_cypher_query(&input.query).map_err(|e| {
                    ErrorData::invalid_params(format!("Query rejected: {}", e), None)
                })?;
                let mut a = vec!["query", "cypher", &input.query];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "route_map" => {
                let input: RouteMapInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "routes"];
                if let Some(ref r) = input.route { a.push("--route"); a.push(r); }
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "shape_check" => {
                let input: ShapeCheckInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "shape-check"];
                if let Some(ref r) = input.route { a.push("--route"); a.push(r); }
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "tool_map" => {
                let mut a = vec!["query", "tool-map"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "api_impact" => {
                let input: ApiImpactInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let r = input.route.as_deref().or(input.file.as_deref())
                    .ok_or_else(|| ErrorData::invalid_params("Need 'route' or 'file'".to_string(), None))?;
                let mut a = vec!["query", "api-impact", r];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "verify" => {
                let input: VerifyInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "verify", "--type", &input.verify_type];
                if let Some(ref c) = input.command { a.push("--command"); a.push(c); }
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "group_list" => {
                let mut a = vec!["query", "repos"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                let output = self.run_atree(&a)?;
                let conn_output = std::process::Command::new(&self.atree_bin)
                    .args(["query", "cypher", "SELECT COUNT(*) as cross_links FROM edges WHERE edge_kind = 'CROSS_REPO_DEP'"])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                if !conn_output.trim().is_empty() {
                    format!("{}\nCross-repo links: {}", output, conn_output.trim())
                } else {
                    output
                }
            }
            "group_sync" => {
                let mut a = vec!["query", "group-sync"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "explain_symbol" => {
                let input: ExplainInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "explain", &input.symbol];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "find_entrypoints" => {
                let mut a = vec!["query", "entrypoints"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "trace_call_path" => {
                let input: TracePathInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "trace-path", &input.from, "--to", &input.to];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "public_api_surface" => {
                let mut a = vec!["query", "public-api"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "affected_tests" => {
                let input: AffectedTestsInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "affected-tests", &input.symbol];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "validation_plan" => {
                let input: ValidationPlanInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "validation-plan", &input.symbol];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "contract_change_detector" => {
                let input: ContractChangesInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "contract-changes"];
                if let Some(ref b) = input.base_ref { a.push("--base"); a.push(b); }
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "architecture_boundary_check" => {
                let mut a = vec!["query", "boundary-check"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "scope_violation_detector" => {
                let mut a = vec!["query", "scope-violations"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "config_surface_map" => {
                let mut a = vec!["query", "config-map"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "impact_by_symbol_kind" => {
                let input: ImpactByKindInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "impact-by-kind", &input.target, "--kind", &input.kind, "--direction", &input.direction];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "semantic_diff_summary" => {
                let input: SemanticDiffInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "semantic-diff"];
                if let Some(ref b) = input.base_ref { a.push("--base"); a.push(b); }
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "side_effect_scanner" => {
                let input: SideEffectsInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "side-effects", &input.symbol];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "change_coupling" => {
                let input: ChangeCouplingInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "change-coupling", &input.symbol];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "concurrency_surface_detector" => {
                let mut a = vec!["query", "concurrency"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "minimal_edit_scope" => {
                let input: EditScopeInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "edit-scope", &input.symbol];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "issue_to_code_locator" => {
                let input: IssueLocatorInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "issue-locator", &input.issue_id];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "docs_drift_detector" => {
                let mut a = vec!["query", "docs-drift"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "rename_safety_check" => {
                let input: RenameSafetyInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "rename-safety", &input.symbol_name, "--new-name", &input.new_name];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "dead_code_candidates" => {
                let mut a = vec!["query", "dead-code"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "ownership_hotspots" => {
                let mut a = vec!["query", "hotspots"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "error_path_trace" => {
                let input: ErrorTraceInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "error-trace", &input.symbol];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "resource_lifecycle_map" => {
                let input: ResourceLifecycleInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "resource-lifecycle", &input.symbol];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "dependency_cycle_detector" => {
                let mut a = vec!["query", "dep-cycles"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "find_uncovered_symbols" => {
                let mut a = vec!["query", "uncovered"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "resolution_stats" => {
                let mut a = vec!["query", "resolution-stats"];
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "evidence_path" => {
                let input: EvidencePathInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let mut a = vec!["query", "evidence-path", &input.query];
                let md = input.max_depth.to_string();
                let bw = input.beam_width.to_string();
                let me = input.max_evidence.to_string();
                if input.max_depth != 3 { a.push(&md); }
                if input.beam_width != 5 { a.push(&bw); }
                if input.max_evidence != 10 { a.push(&me); }
                if let Some(db) = db { a.push("--db"); a.push(db); }
                self.run_atree(&a)?
            }
            "evidence_search" => {
                let input: EvidenceSearchInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let store = self.open_store()?;
                let ev_store = crate::evidence::storage::EvidenceStore::new(store.conn());
                let results = ev_store.search(&input.query, input.limit as usize)
                    .map_err(|e| ErrorData::internal_error(format!("Evidence search failed: {}", e), None))?;
                if results.is_empty() {
                    "No evidence found matching the query.".to_string()
                } else {
                    let mut out = format!("Evidence Search Results ({} matches):\n\n", results.len());
                    for rec in results {
                        out.push_str(&format!(
                            "[{:.2}] {} {} @ {} (lang={})\n",
                            rec.rank, rec.kind, rec.target_ref, rec.file, rec.language
                        ));
                    }
                    out
                }
            }
            "pattern_mine" => {
                let input: PatternMineInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let store = self.open_store()?;
                let ev_store = crate::evidence::storage::EvidenceStore::new(store.conn());
                // Fetch all committed evidence across all kinds.
                let all_records: Vec<crate::evidence::storage::EvidenceRecord> = [
                    crate::evidence::EvidenceKind::SymbolDeclaration,
                    crate::evidence::EvidenceKind::FunctionCall,
                    crate::evidence::EvidenceKind::ImportEdge,
                    crate::evidence::EvidenceKind::TypeRelation,
                ].iter()
                    .filter_map(|k| ev_store.by_kind(*k).ok())
                    .flatten()
                    .collect();
                let evidence: Vec<crate::evidence::Evidence> = all_records.into_iter()
                    .map(|rec| {
                        use crate::evidence::*;
                        let imports: Vec<String> = serde_json::from_str(&rec.imports).unwrap_or_default();
                        let scope_chain: Vec<String> = serde_json::from_str(&rec.scope_chain).unwrap_or_default();
                        let tags: Vec<String> = serde_json::from_str(&rec.tags).unwrap_or_default();
                        let kind = rec.kind.parse().unwrap_or(EvidenceKind::HeuristicInference);
                        Evidence {
                            id: EvidenceId(rec.id), kind,
                            source: EvidenceSource { file: rec.file.clone(), span: SourceSpan { start_line: rec.start_line, start_col: rec.start_col, end_line: rec.end_line, end_col: rec.end_col }, language: rec.language },
                            target: EvidenceTarget { target_type: TargetType::Symbol, ref_id: rec.target_ref },
                            content: EvidenceContent { raw: rec.raw, normalized: rec.normalized },
                            context: EvidenceContext { enclosing_symbol: rec.enclosing_symbol, imports, scope_chain },
                            metadata: crate::evidence::EvidenceMetadata { extractor: rec.extractor, confidence: rec.confidence, stability: rec.stability, entropy: rec.entropy, timestamp_ms: rec.timestamp_ms, commit: rec.commit },
                            links: crate::evidence::EvidenceLinks::default(), tags,
                            state: crate::evidence::EvidenceState::Committed,
                        }
                    })
                    .collect();
                let config = crate::patterns::PatternMiningConfig { min_frequency: input.min_frequency as usize, ..Default::default() };
                let patterns = crate::patterns::mine_patterns(&evidence, &config);
                let max = input.max_patterns.unwrap_or(50) as usize;
                let out_patterns: Vec<_> = patterns.into_iter().take(max).collect();
                if out_patterns.is_empty() {
                    serde_json::json!({"patterns": [], "message": "No patterns found matching the criteria (try lowering min_frequency)."}).to_string()
                } else {
                    let json_patterns: Vec<serde_json::Value> = out_patterns.iter().map(|p| {
                        serde_json::json!({
                            "id": p.id,
                            "name": p.name,
                            "description": p.description,
                            "motif": p.motif.iter().map(|k| format!("{:?}", k)).collect::<Vec<_>>(),
                            "score": {
                                "frequency": p.score.frequency,
                                "dispersion": p.score.dispersion,
                                "stability": p.score.stability,
                                "overall": p.score.overall,
                            },
                            "evidence_count": p.evidence_ids.len(),
                        })
                    }).collect();
                    serde_json::json!({"patterns": json_patterns, "total": json_patterns.len()}).to_string()
                }
            }
            "constraint_check" => {
                let input: ConstraintCheckInput = serde_json::from_value(serde_json::Value::Object(args))
                    .map_err(|e| ErrorData::invalid_params(format!("Invalid input: {}", e), None))?;
                let store = self.open_store()?;
                let ev_store = crate::evidence::storage::EvidenceStore::new(store.conn());
                // Gather evidence for constraint synthesis.
                let all_records: Vec<crate::evidence::storage::EvidenceRecord> = [
                    crate::evidence::EvidenceKind::SymbolDeclaration,
                    crate::evidence::EvidenceKind::FunctionCall,
                    crate::evidence::EvidenceKind::ImportEdge,
                ].iter()
                    .filter_map(|k| ev_store.by_kind(*k).ok())
                    .flatten()
                    .collect();
                let evidence: Vec<crate::evidence::Evidence> = all_records.into_iter()
                    .map(|rec| crate::pipeline::phases::record_to_evidence(rec))
                    .collect();
                // Mine patterns first, then synthesize constraints.
                let pattern_config = crate::patterns::PatternMiningConfig::default();
                let patterns = crate::patterns::mine_patterns(&evidence, &pattern_config);
                let constraint_config = crate::constraints::ConstraintSynthesisConfig::default();
                let constraints = crate::constraints::synthesize_constraints(&evidence, &patterns, &constraint_config);
                let violations = if let Some(ref sym) = input.symbol {
                    crate::constraints::detect_violations(&constraints, &evidence)
                        .into_iter().filter(|(_, ev_id)| {
                            evidence.iter().any(|e| &e.id == ev_id && e.content.raw == *sym)
                        }).collect()
                } else { vec![] };
                let json_constraints: Vec<serde_json::Value> = constraints.iter().map(|c| {
                    serde_json::json!({
                        "id": c.id,
                        "name": c.name,
                        "kind": c.kind.as_str(),
                        "confidence": c.confidence,
                        "active": c.active,
                        "description": c.description,
                    })
                }).collect();
                serde_json::json!({
                    "constraints": json_constraints,
                    "total": json_constraints.len(),
                    "evidence_units": evidence.len(),
                    "patterns_mined": patterns.len(),
                    "violations": violations.len(),
                }).to_string()
            }
            unknown => {
                return Err(ErrorData::invalid_params(
                    format!("Unknown tool: {}", unknown), None
                ));
            }
        };
        Ok(CallToolResult::success(vec![Content::text(result)]))
    }
}

// =====================================================================
// Server runner
// =====================================================================

pub async fn run_mcp_server(
    atree_bin: Option<String>,
    db_path: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let server = ATreeMcpServer::new(atree_bin, db_path);
    let running = server.serve((tokio::io::stdin(), tokio::io::stdout())).await?;
    // Wait for either the MCP client to disconnect OR a Ctrl+C signal.
    tokio::select! {
        result = running.waiting() => {
            log::info!("MCP server disconnected");
            result?;
        }
        _ = tokio::signal::ctrl_c() => {
            log::info!("Received Ctrl+C, shutting down MCP server gracefully");
        }
    }
    Ok(())
}

// =====================================================================
// SQL validation for cypher tool
// =====================================================================

/// Allowed tables and their permitted columns for cypher queries.
const ALLOWED_TABLES: &[(&str, &[&str])] = &[
    ("files", &["id", "path", "hash", "language", "mtime", "indexed_at", "repo_label"]),
    ("symbols", &["id", "file_id", "name", "qualified_name", "kind", "line", "col", "is_exported", "scope_id", "owner_symbol_id"]),
    ("scopes", &["id", "file_id", "parent_id", "owner_symbol_id", "kind", "line_start", "line_end"]),
    ("imports", &["id", "file_id", "source", "imported_name", "local_name", "resolved_file_id", "confidence"]),
    ("exports", &["id", "file_id", "exported_name", "symbol_id", "is_default"]),
    ("heritage", &["id", "file_id", "child_symbol_id", "parent_symbol_id", "parent_name", "heritage_kind", "confidence", "line"]),
    ("calls", &["id", "file_id", "caller_scope_id", "callee_name", "receiver", "resolved_symbol_id", "confidence", "line", "col"]),
    ("edges", &["id", "src_id", "dst_id", "edge_kind", "confidence", "file_id", "line"]),
];

/// Validate a cypher query against the allowlist.
///
/// Rejects:
/// - References to sqlite_master, sqlite_temp_master, pg_catalog, information_schema
/// - PRAGMA statements
/// - INSERT, UPDATE, DELETE, DROP, ALTER, CREATE, ATTACH, DETACH
/// - Semicolons (multi-statement injection)
/// - Comments that could mask injected SQL
/// - Tables/columns not in the allowlist
fn validate_cypher_query(query: &str) -> Result<(), String> {
    let lower = query.to_lowercase();

    // Block dangerous keywords/patterns.
    let blocked_patterns = [
        "sqlite_master", "sqlite_temp_master", "pg_catalog", "information_schema",
        "pragma", ";", "--", "/*", "*/",
        "insert", "update", "delete", "drop", "alter", "create",
        "attach", "detach", "replace",
    ];
    for pat in &blocked_patterns {
        if lower.contains(pat) {
            return Err(format!("Query contains forbidden pattern: '{}'", pat));
        }
    }

    // Must start with SELECT or WITH.
    let trimmed = lower.trim();
    if !trimmed.starts_with("select") && !trimmed.starts_with("with") {
        return Err("Only SELECT and WITH queries are allowed".to_string());
    }

    // Extract and validate all referenced table names.
    let table_names: std::collections::HashSet<&str> = ALLOWED_TABLES.iter().map(|(t, _)| *t).collect();
    // Simple word-boundary check: find any word that looks like a table name
    // not in our allowlist but appears after FROM or JOIN.
    for table_word in lower.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if table_word.is_empty() { continue; }
        // Check if this word is a known SQL keyword we should skip.
        let sql_keywords = ["select", "from", "where", "join", "left", "right", "inner",
            "outer", "on", "and", "or", "not", "in", "is", "null", "as", "group",
            "order", "by", "limit", "offset", "having", "union", "all", "distinct",
            "case", "when", "then", "else", "end", "exists", "between", "like",
            "count", "sum", "avg", "min", "max", "asc", "desc", "using",
            "with", "recursive", "cast", "coalesce"];
        if sql_keywords.contains(&table_word) { continue; }
        // If it looks like an identifier (starts with letter), it should be in our allowlist.
        if table_word.chars().next().map_or(false, |c| c.is_alphabetic())
            && !table_names.contains(table_word)
            && table_word.len() > 1
        {
            return Err(format!("Query references unknown table: '{}'", table_word));
        }
    }

    Ok(())
}
