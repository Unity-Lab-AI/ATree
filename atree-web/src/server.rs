//! HTTP server for the ATree web UI.
//!
//! Provides:
//! - `GET /api/health` — health check
//! - `GET /api/stats` — index statistics
//! - `GET /api/search` — search symbols by name
//! - `GET /api/search/semantic` — semantic search that returns a focused subgraph
//! - `GET /api/graph/layout` — compute and return graph layout
//! - `GET /api/graph/overview` — high-level graph summary
//! - `GET /api/graph/clusters` — community-level abstraction graph
//! - `GET /api/graph/cluster/{id}` — cluster detail
//! - `GET /api/graph/cluster/{id}/meta` — cluster metadata
//! - `GET /api/graph/node/{id}` — node details with edges
//! - `GET /api/graph/focus` — trigger focus shift (MCP tool)
//! - `GET /api/graph/query` — run graph query
//! - `POST /api/events` — SSE stream for real-time events
//! - `GET /api/webhook/push` — CI/CD webhook for push-triggered re-index

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;

use atree_engine::store::GraphStore;

use crate::events::{create_sse_stream, EventBus, GraphFocusEvent};
use crate::layout::{self, LayoutConfig};

// ── Shared state ─────────────────────────────────────────────────────────────

pub struct AppState {
    pub event_bus: Arc<RwLock<EventBus>>,
    pub db_path: Option<PathBuf>,
    pub repo_path: Option<String>,
    pub webhook_secret: Option<String>,
}

impl AppState {
    pub fn new(db_path: Option<PathBuf>) -> Self {
        Self {
            event_bus: Arc::new(RwLock::new(EventBus::new())),
            db_path,
            repo_path: None,
            webhook_secret: std::env::var("ATREE_WEBHOOK_SECRET").ok(),
        }
    }

    pub async fn open_store(&self) -> Option<GraphStore> {
        let path = self.db_path.as_ref()?;
        GraphStore::open(path).ok()
    }
}

// ── Query parameters ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct LayoutQuery {
    #[serde(default = "default_iterations")]
    iterations: usize,
    #[serde(default)]
    types: String,
    #[serde(default)]
    edges: String,
    #[serde(default = "default_seed")]
    seed: u64,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default = "default_scope")]
    scope: String,
    #[serde(default)]
    file_id: Option<i64>,
    #[serde(default)]
    symbol_id: Option<i64>,
    #[serde(default = "default_neighborhood_depth")]
    neighborhood_depth: usize,
    #[serde(default = "default_max_layout_nodes")]
    max_layout_nodes: usize,
    #[serde(default = "default_layout_algo")]
    algorithm: String,
}

fn default_scope() -> String { "full".to_string() }
fn default_layout_algo() -> String { "force".to_string() }
fn default_neighborhood_depth() -> usize { 2 }
fn default_max_layout_nodes() -> usize { 2000 }
fn default_iterations() -> usize { 300 }
fn default_seed() -> u64 { 42 }

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize { 20 }

#[derive(Debug, Deserialize)]
pub struct GraphQueryInput {
    pub query: String,
    #[serde(default = "default_max_symbols")]
    max_symbols: usize,
    #[serde(default = "default_max_depth")]
    max_depth: usize,
    pub task_context: Option<String>,
    pub goal: Option<String>,
}

fn default_max_symbols() -> usize { 20 }
fn default_max_depth() -> usize { 3 }

#[derive(Debug, Deserialize)]
pub struct WebhookPayload {
    pub repo: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub repo_path: Option<String>,
    #[serde(default = "default_event_type")]
    event_type: String,
}

fn default_event_type() -> String { "push".to_string() }

// ── Response types ───────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct StatsResponse {
    pub files: i64,
    pub symbols: i64,
    pub scopes: i64,
    pub imports: i64,
    pub calls: i64,
    pub edges: i64,
    pub resolved_calls: i64,
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
    pub has_index: bool,
}

#[derive(Serialize)]
pub struct SearchResult {
    pub id: String,
    pub label: String,
    pub node_type: String,
    pub file_path: String,
    pub line: Option<i64>,
    pub qualified_name: String,
    pub score: f64,
}

#[derive(Serialize)]
pub struct SearchResponse {
    pub results: Vec<SearchResult>,
    pub total: usize,
}

#[derive(Serialize)]
pub struct QueryResponse {
    pub text: String,
    pub node_ids: Vec<String>,
    pub symbols_found: usize,
}

#[derive(Serialize)]
pub struct WebhookResponse {
    pub ok: bool,
    pub message: String,
    pub reindex_queued: bool,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

async fn health(State(state): State<Arc<AppState>>) -> Json<HealthResponse> {
    let has_index = state.db_path.as_ref().map(|p| p.exists()).unwrap_or(false);
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        has_index,
    })
}

async fn stats(State(state): State<Arc<AppState>>) -> Json<StatsResponse> {
    let store = match state.open_store().await {
        Some(s) => s,
        None => return Json(StatsResponse { files: 0, symbols: 0, scopes: 0, imports: 0, calls: 0, edges: 0, resolved_calls: 0 }),
    };
    match store.stats() {
        Ok(s) => Json(StatsResponse {
            files: s.files, symbols: s.symbols, scopes: s.scopes,
            imports: s.imports, calls: s.calls, edges: s.edges, resolved_calls: s.resolved_calls,
        }),
        Err(_) => Json(StatsResponse { files: 0, symbols: 0, scopes: 0, imports: 0, calls: 0, edges: 0, resolved_calls: 0 }),
    }
}

async fn search(State(state): State<Arc<AppState>>, Query(query): Query<SearchQuery>) -> Json<SearchResponse> {
    let q = query.q.trim().to_lowercase();
    if q.is_empty() { return Json(SearchResponse { results: vec![], total: 0 }); }
    let store = match state.open_store().await { Some(s) => s, None => return Json(SearchResponse { results: vec![], total: 0 }) };
    let symbols = match store.search_symbols(&q, query.limit) { Ok(s) => s, Err(_) => return Json(SearchResponse { results: vec![], total: 0 }) };
    let total = symbols.len();
    Json(SearchResponse { results: symbols.into_iter().map(|sym| {
        let file_path = store.get_file_by_id(sym.file_id).ok().flatten().map(|f| f.path).unwrap_or_default();
        let name_lower = sym.name.to_lowercase();
        let score = if name_lower == q { 1.0 } else if name_lower.starts_with(&q) { 0.8 } else { 0.5 };
        SearchResult { id: format!("sym:{}", sym.id), label: sym.name, node_type: sym.kind, file_path, line: Some(sym.line as i64), qualified_name: sym.qualified_name, score }
    }).collect(), total })
}

#[derive(Debug, Deserialize)]
pub struct SemanticSearchQuery { pub q: String, #[serde(default = "default_search_limit")] limit: usize, #[serde(default = "default_neighborhood_depth")] depth: usize }

async fn semantic_search(State(state): State<Arc<AppState>>, Query(query): Query<SemanticSearchQuery>) -> axum::response::Response {
    let q = query.q.trim().to_lowercase();
    if q.is_empty() { return Json(serde_json::json!({"error": "Query is empty"})).into_response(); }
    let store = match state.open_store().await { Some(s) => s, None => return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error": "No index available."}))).into_response() };
    let matched_symbols = match store.search_symbols(&q, query.limit) { Ok(s) => s, Err(_) => return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "Search failed"}))).into_response() };
    if matched_symbols.is_empty() { return Json(serde_json::json!({"layout": {"nodes": [], "edges": []}, "matched_symbols": [], "message": "No matching symbols found."})).into_response(); }
    let depth = query.depth.min(5);
    let mut all_symbol_ids: rustc_hash::FxHashSet<i64> = matched_symbols.iter().map(|s| s.id).collect();
    let mut frontier: Vec<i64> = matched_symbols.iter().map(|s| s.id).collect();
    for _ in 0..depth {
        let mut next = Vec::new();
        for sid in &frontier {
            if let Ok(edges) = store.get_edges_for_node(*sid) { for e in &edges { if !all_symbol_ids.contains(&e.dst_id) { all_symbol_ids.insert(e.dst_id); next.push(e.dst_id); } if !all_symbol_ids.contains(&e.src_id) { all_symbol_ids.insert(e.src_id); next.push(e.src_id); } } }
        } if next.is_empty() { break; } frontier = next;
        if all_symbol_ids.len() > 500 { break; }
    }
    let mut nodes = Vec::new();
    let mut loaded = rustc_hash::FxHashSet::default();
    for sid in &all_symbol_ids {
        if let Ok(Some(sym)) = store.get_symbol_by_id(*sid) {
            let fp = store.get_file_by_id(sym.file_id).ok().flatten().map(|f| f.path).unwrap_or_default();
            let mut props = rustc_hash::FxHashMap::default();
            props.insert("name".to_string(), sym.name.clone()); props.insert("kind".to_string(), sym.kind.clone()); props.insert("qualified_name".to_string(), sym.qualified_name.clone()); props.insert("file_path".to_string(), fp); props.insert("line".to_string(), sym.line.to_string()); props.insert("col".to_string(), sym.col.to_string());
            let is_match = matched_symbols.iter().any(|m| m.id == sym.id);
            props.insert("is_match".to_string(), is_match.to_string());
            nodes.push(atree_engine::graph::GraphNode { id: format!("sym:{}", sym.id), label: sym.kind.clone(), properties: props });
            loaded.insert(*sid);
        }
    }
    let mut edges = Vec::new();
    for sid in &loaded {
        if let Ok(es) = store.get_edges_for_node(*sid) { for e in &es { if loaded.contains(&e.dst_id) && e.src_id != e.dst_id { edges.push(atree_engine::graph::GraphEdge { id: format!("e{}", e.id), source_id: format!("sym:{}", e.src_id), target_id: format!("sym:{}", e.dst_id), rel_type: atree_engine::graph::RelationshipType::Uses, confidence: e.confidence, reason: String::new(), step: None }); } } }
    }
    let matched_ids: Vec<String> = matched_symbols.iter().map(|s| format!("sym:{}", s.id)).collect();
    Json(serde_json::json!({ "layout": layout::compute_layout(&nodes, &edges, &LayoutConfig::default()), "scope": "semantic", "query": q, "matched_symbols": matched_ids, "nodes_returned": nodes.len(), "edges_returned": edges.len(), "neighborhood_depth": depth })).into_response()
}

async fn node_detail(State(state): State<Arc<AppState>>, Path(node_id): Path<String>) -> Json<serde_json::Value> {
    let store = match state.open_store().await { Some(s) => s, None => { return Json(serde_json::json!({"error": "No index"})); } };
    let (outgoing, incoming) = if let Some(id_str) = node_id.strip_prefix("sym:") {
        if let Ok(id) = id_str.parse::<i64>() {
            let edges = store.get_edges_for_node(id).unwrap_or_default();
            let out: Vec<_> = edges.iter().filter(|e| e.src_id == id).map(|e| serde_json::json!({"target": format!("sym:{}", e.dst_id), "rel_type": e.edge_kind, "confidence": e.confidence, "file_id": e.file_id, "line": e.line})).collect();
            let inc: Vec<_> = edges.iter().filter(|e| e.dst_id == id).map(|e| serde_json::json!({"source": format!("sym:{}", e.src_id), "rel_type": e.edge_kind, "confidence": e.confidence, "file_id": e.file_id, "line": e.line})).collect();
            (out, inc)
        } else { (vec![], vec![]) }
    } else { (vec![], vec![]) };
    Json(serde_json::json!({"node": {"id": node_id}, "outgoing": outgoing, "incoming": incoming}))
}

async fn graph_query(State(state): State<Arc<AppState>>, Json(input): Json<GraphQueryInput>) -> Json<QueryResponse> {
    let store = match state.open_store().await { Some(s) => s, None => return Json(QueryResponse { text: "No index available".to_string(), node_ids: vec![], symbols_found: 0 }) };
    let enriched = if input.task_context.is_some() || input.goal.is_some() { format!("{}{}{}", input.query, input.task_context.as_ref().map(|c| format!(" | Context: {}", c)).unwrap_or_default(), input.goal.as_ref().map(|g| format!(" | Goal: {}", g)).unwrap_or_default()) } else { input.query.clone() };
    let config = atree_engine::evidence_path::EvidenceConfig { max_seeds: 10, beam_width: 5, max_depth: input.max_depth, max_evidence: input.max_symbols, token_budget: 4000, ..Default::default() };
    match atree_engine::evidence_bundle::query_evidence(&store, &enriched, &config) {
        Ok(bundle) => {
            let text = atree_engine::evidence_bundle::format_bundle_as_text(&bundle);
            let node_ids: Vec<_> = bundle.paths.iter().flat_map(|p| p.steps.iter().map(|s| s.node_id.clone())).collect::<std::collections::HashSet<_>>().into_iter().collect();
            Json(QueryResponse { text, node_ids, symbols_found: bundle.paths.len() })
        }
        Err(_) => Json(QueryResponse { text: "Query failed".to_string(), node_ids: vec![], symbols_found: 0 }),
    }
}

async fn graph_focus(State(state): State<Arc<AppState>>, Json(input): Json<serde_json::Value>) -> Json<serde_json::Value> {
    let event = GraphFocusEvent {
        event_type: input.get("event_type").and_then(|v| v.as_str()).unwrap_or("focus_node").to_string(),
        node_ids: input.get("node_ids").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default(),
        label: input.get("label").and_then(|v| v.as_str()).unwrap_or("Agent focus").to_string(),
        source: input.get("source").and_then(|v| v.as_str()).unwrap_or("mcp_tool").to_string(),
        zoom: input.get("zoom").and_then(|v| v.as_f64()),
        anim_duration_ms: input.get("anim_duration_ms").and_then(|v| v.as_u64()),
    };
    let recipients = { let bus = state.event_bus.read().await; bus.publish(event) };
    Json(serde_json::json!({"ok": true, "event_id": uuid::Uuid::new_v4().to_string(), "recipients": recipients}))
}

async fn graph_events(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let rx = state.event_bus.read().await.subscribe();
    create_sse_stream(rx)
}

async fn webhook_push(State(state): State<Arc<AppState>>, headers: HeaderMap, Json(payload): Json<WebhookPayload>) -> axum::response::Response {
    if let Some(ref expected) = state.webhook_secret {
        let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
        if auth != expected { return (axum::http::StatusCode::UNAUTHORIZED, Json(WebhookResponse { ok: false, message: "Unauthorized".to_string(), reindex_queued: false })).into_response(); }
    }
    let repo_path = payload.repo_path.as_deref().unwrap_or(".");
    let branch = payload.branch.as_deref().unwrap_or("main");
    let db_path = state.db_path.clone();
    let event_bus = state.event_bus.clone();
    let repo_owned = repo_path.to_string();
    tokio::spawn(async move {
        if let Some(ref db) = db_path {
            let opts = atree_engine::ScanOptions { root: std::path::PathBuf::from(&repo_owned), db_path: Some(db.clone()), incremental: true, semantic: true, threads: atree_engine::half_cores(), include_files: true, ..Default::default() };
            let _ = atree_engine::build_graph(&opts);
        }
    });
    (axum::http::StatusCode::ACCEPTED, Json(WebhookResponse { ok: true, message: format!("Re-index queued for {}", repo_path), reindex_queued: true })).into_response()
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn build_layout_config(query: &LayoutQuery, nodes: &[atree_engine::graph::GraphNode]) -> LayoutConfig {
    let types_filter: Vec<String> = if query.types.is_empty() { vec![] } else { query.types.split(',').map(|s| s.trim().to_string()).collect() };
    let edges_filter: Vec<String> = if query.edges.is_empty() { vec![] } else { query.edges.split(',').map(|s| s.trim().to_string()).collect() };
    let iterations = if nodes.len() > 1000 { query.iterations.min(100) } else { query.iterations };
    let algorithm = match query.algorithm.as_str() { "layered" => layout::LayoutAlgorithm::LayeredDAG, _ => layout::LayoutAlgorithm::ForceDirected };
    LayoutConfig { algorithm, iterations, threads: 0, seed: query.seed, node_type_filter: types_filter, edge_filter: edges_filter, ..Default::default() }
}

// ── Route builder ────────────────────────────────────────────────────────────

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/stats", get(stats))
        .route("/api/search", get(search))
        .route("/api/search/semantic", get(semantic_search))
        .route("/api/graph/node/{id}", get(node_detail))
        .route("/api/graph/query", post(graph_query))
        .route("/api/graph/focus", post(graph_focus))
        .route("/api/events", get(graph_events))
        .route("/api/webhook/push", post(webhook_push))
        .nest_service("/static", ServeDir::new("atree-web/static"))
        .fallback_service(ServeDir::new("atree-web/static").append_index_html_on_directories(true))
        .with_state(state)
        .layer(CorsLayer::new()
            .allow_origin(tower_http::cors::AllowOrigin::exact(
                "http://localhost:3020".parse::<axum::http::HeaderValue>().expect("valid localhost origin")))
            .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
            .allow_headers(tower_http::cors::Any))
}
