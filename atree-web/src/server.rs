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
    extract::{DefaultBodyLimit, Path, Query, State},
    http::HeaderMap,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// In-flight webhook re-index counter. Used to reject new webhooks
    /// while one is already running to prevent unbounded task spawning.
    pub webhook_inflight: Arc<AtomicU64>,
    /// Timestamp of the last webhook request (unix seconds).
    pub webhook_last_ms: Arc<AtomicU64>,
}

impl AppState {
    pub fn new(db_path: Option<PathBuf>) -> Self {
        Self {
            event_bus: Arc::new(RwLock::new(EventBus::new())),
            db_path,
            repo_path: None,
            webhook_secret: std::env::var("ATREE_WEBHOOK_SECRET").ok().filter(|s| !s.is_empty()),
            webhook_inflight: Arc::new(AtomicU64::new(0)),
            webhook_last_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Check and record a webhook rate limit call. Returns true if allowed.
    /// Limits to 1 concurrent re-index and 60s between requests.
    fn check_webhook_rate_limit(&self) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let last = self.webhook_last_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) < 60 {
            tracing::warn!(last, now, "Webhook rate-limited (60s cooldown)");
            return false;
        }
        if self.webhook_inflight.load(Ordering::Relaxed) > 0 {
            tracing::warn!("Webhook re-index already in-flight");
            return false;
        }
        self.webhook_last_ms.store(now, Ordering::Relaxed);
        true
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
    #[allow(dead_code)]
    scope: String,
    #[serde(default)]
    #[allow(dead_code)]
    file_id: Option<i64>,
    #[serde(default)]
    #[allow(dead_code)]
    symbol_id: Option<i64>,
    #[serde(default = "default_neighborhood_depth")]
    #[allow(dead_code)]
    neighborhood_depth: usize,
    #[serde(default = "default_max_layout_nodes")]
    #[allow(dead_code)]
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
    #[allow(dead_code)]
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
    pub webhook_inflight: u64,
    pub webhook_last_secs_ago: u64,
}

#[derive(Serialize)]
pub struct MetricsResponse {
    pub uptime_secs: u64,
    pub index_files: i64,
    pub index_symbols: i64,
    pub index_edges: i64,
    pub webhook_requests_total: u64,
    pub webhook_inflight: u64,
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
    let last = state.webhook_last_ms.load(Ordering::Relaxed);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let secs_ago = now.saturating_sub(last);
    Json(HealthResponse {
        status: "ok".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        has_index,
        webhook_inflight: state.webhook_inflight.load(Ordering::Relaxed),
        webhook_last_secs_ago: if last == 0 { 0 } else { secs_ago },
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
        Err(e) => {
            tracing::warn!(error = %e, "Stats query failed");
            Json(StatsResponse { files: 0, symbols: 0, scopes: 0, imports: 0, calls: 0, edges: 0, resolved_calls: 0 })
        }
    }
}

async fn metrics(State(state): State<Arc<AppState>>) -> Json<MetricsResponse> {
    let (files, symbols, edges) = match state.open_store().await {
        Some(store) => match store.stats() {
            Ok(s) => (s.files, s.symbols, s.edges),
            Err(e) => {
                tracing::warn!(error = %e, "Metrics stats query failed");
                (0, 0, 0)
            }
        },
        None => (0, 0, 0),
    };
    Json(MetricsResponse {
        uptime_secs: 0,
        index_files: files,
        index_symbols: symbols,
        index_edges: edges,
        webhook_requests_total: 0,
        webhook_inflight: state.webhook_inflight.load(Ordering::Relaxed),
    })
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
        if frontier.len() > 1000 { break; } // Prevent frontier explosion
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
    // Reject if no webhook secret is configured — the endpoint must not be open.
    let expected = match &state.webhook_secret {
        Some(s) => s,
        None => {
            tracing::error!("Webhook called without ATREE_WEBHOOK_SECRET configured");
            return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(WebhookResponse { ok: false, message: "Webhook not configured: set ATREE_WEBHOOK_SECRET".to_string(), reindex_queued: false })).into_response();
        }
    };
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
    // Use subtle to prevent timing attacks on the secret comparison.
    let equal = auth.len() == expected.len() && {
        let mut result = 0u8;
        for (a, b) in auth.bytes().zip(expected.bytes()) {
            result |= a ^ b;
        }
        result == 0
    };
    if !equal {
        tracing::warn!("Webhook auth failed");
        return (axum::http::StatusCode::UNAUTHORIZED, Json(WebhookResponse { ok: false, message: "Unauthorized".to_string(), reindex_queued: false })).into_response();
    }
    if !state.check_webhook_rate_limit() {
        return (axum::http::StatusCode::TOO_MANY_REQUESTS, Json(WebhookResponse { ok: false, message: "Rate limited: max 1 re-index per 60s".to_string(), reindex_queued: false })).into_response();
    }
    let repo_path = payload.repo_path.as_deref().unwrap_or(".");
    let canonical = match std::path::PathBuf::from(repo_path).canonicalize() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(path = %repo_path, error = %e, "Webhook: invalid repo_path");
            return (axum::http::StatusCode::BAD_REQUEST, Json(WebhookResponse { ok: false, message: format!("Invalid path: {}", e), reindex_queued: false })).into_response();
        }
    };
    let cwd = match std::env::current_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "Webhook: cannot determine CWD");
            return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(WebhookResponse { ok: false, message: "Cannot determine working directory".to_string(), reindex_queued: false })).into_response();
        }
    };
    if !canonical.starts_with(&cwd) {
        tracing::warn!(path = %repo_path, "Webhook: path traversal attempt blocked");
        return (axum::http::StatusCode::FORBIDDEN, Json(WebhookResponse { ok: false, message: "Path must be within working directory".to_string(), reindex_queued: false })).into_response();
    }
    let inflight = state.webhook_inflight.clone();
    let db_path = state.db_path.clone();
    let repo_display = repo_path.to_string();
    inflight.fetch_add(1, Ordering::Relaxed);
    tokio::spawn(async move {
        let start = std::time::Instant::now();
        if let Some(ref db) = db_path {
            let opts = atree_engine::ScanOptions { root: canonical, db_path: Some(db.clone()), incremental: true, semantic: true, threads: atree_engine::half_cores(), include_files: true, ..Default::default() };
            match atree_engine::build_graph(&opts) {
                Ok(_) => tracing::info!(elapsed_ms = start.elapsed().as_millis() as u64, "Webhook re-index completed"),
                Err(e) => tracing::error!(error = %e, "Webhook re-index failed"),
            }
        }
        inflight.fetch_sub(1, Ordering::Relaxed);
    });
    tracing::info!(path = %repo_display, "Webhook re-index queued");
    (axum::http::StatusCode::ACCEPTED, Json(WebhookResponse { ok: true, message: format!("Re-index queued for {}", repo_display), reindex_queued: true })).into_response()
}

async fn graph_layout(State(state): State<Arc<AppState>>, Query(query): Query<LayoutQuery>) -> axum::response::Response {
    let store = match state.open_store().await { Some(s) => s, None => return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error": "No index available"}))).into_response() };

    // Determine graph size class and default view.
    let graph_meta = store.get_all_graph_metadata().ok().flatten();
    let size_class = graph_meta.as_ref().and_then(|m| m.get("size_class")).map(|s| s.as_str()).unwrap_or("small");
    let max_nodes = graph_meta.as_ref().and_then(|m| m.get("max_layout_nodes")).and_then(|s| s.parse::<usize>().ok()).unwrap_or(500);

    // For large graphs, use module-level view instead of raw symbols.
    match size_class {
        "xlarge" | "large" if query.scope.as_str() != "full" => {
            return graph_layout_module(&store, &query, size_class).into_response();
        }
        _ => {}
    }

    let limit = query.limit.unwrap_or(max_nodes).min(max_nodes).max(1);
    let symbols = match store.get_all_symbols_paginated(limit) { Ok(s) => s, Err(e) => { tracing::warn!(error = %e, "Failed to load symbols for layout"); return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "Failed to load symbols"}))).into_response(); } };
    let symbol_ids: rustc_hash::FxHashSet<i64> = symbols.iter().map(|s| s.id).collect();
    let file_ids: Vec<i64> = symbols.iter().map(|s| s.file_id).collect();
    let file_paths: rustc_hash::FxHashMap<i64, String> = match store.get_files_by_ids(&file_ids) {
        Ok(files) => files.into_iter().map(|f| (f.id, f.path)).collect(),
        Err(_) => rustc_hash::FxHashMap::default(),
    };
    let mut nodes = Vec::with_capacity(symbols.len());
    for sym in &symbols {
        let fp = file_paths.get(&sym.file_id).cloned().unwrap_or_default();
        let mut props = rustc_hash::FxHashMap::default();
        props.insert("name".to_string(), sym.name.clone());
        props.insert("kind".to_string(), sym.kind.clone());
        props.insert("qualified_name".to_string(), sym.qualified_name.clone());
        props.insert("file_path".to_string(), fp);
        props.insert("line".to_string(), sym.line.to_string());
        props.insert("col".to_string(), sym.col.to_string());
        nodes.push(atree_engine::graph::GraphNode { id: format!("sym:{}", sym.id), label: sym.kind.clone(), properties: props });
    }
    let edges = match store.get_edges_for_symbols(&symbol_ids) {
        Ok(edges) => edges,
        Err(e) => { tracing::warn!(error = %e, "Failed to load edges for layout"); Vec::new() }
    };
    let filtered_edges: Vec<_> = edges.into_iter()
        .filter(|e| symbol_ids.contains(&e.dst_id) && e.src_id != e.dst_id)
        .map(|e| atree_engine::graph::GraphEdge {
            id: format!("e{}", e.id),
            source_id: format!("sym:{}", e.src_id),
            target_id: format!("sym:{}", e.dst_id),
            rel_type: atree_engine::graph::RelationshipType::Uses,
            confidence: e.confidence,
            reason: String::new(),
            step: None,
        })
        .collect();
    let config = build_layout_config(&query, &nodes);
    let result_layout = layout::compute_layout(&nodes, &filtered_edges, &config);
    Json(serde_json::json!({
        "layout": result_layout,
        "nodes_returned": nodes.len(),
        "edges_returned": filtered_edges.len(),
        "size_class": size_class,
    })).into_response()
}

/// Module-level layout for large codebases. Uses pre-computed module_graph_edges
/// which is ~1000x smaller than raw symbol edges.
fn graph_layout_module(store: &atree_engine::store::GraphStore, query: &LayoutQuery, size_class: &str) -> axum::response::Response {
    let module_edges = match store.get_module_graph_edges() {
        Ok(edges) => edges,
        Err(e) => { tracing::warn!(error = %e, "Failed to load module graph edges"); return Json(serde_json::json!({"error": "Failed to load module graph"})).into_response(); }
    };

    // Build unique module nodes from edges.
    let mut module_names: Vec<String> = module_edges.iter().flat_map(|e| vec![e.0.clone(), e.1.clone()]).collect();
    module_names.sort();
    module_names.dedup();

    let limit = query.limit.unwrap_or(500).min(500).max(1);
    let modules: Vec<_> = module_names.into_iter().take(limit).collect();
    let module_set: rustc_hash::FxHashSet<&str> = modules.iter().map(|s| s.as_str()).collect();

    let nodes: Vec<_> = modules.iter().map(|name| {
        let mut props = rustc_hash::FxHashMap::default();
        props.insert("name".to_string(), name.clone());
        props.insert("kind".to_string(), "Module".to_string());
        props.insert("qualified_name".to_string(), name.clone());
        atree_engine::graph::GraphNode {
            id: format!("mod:{}", name.replace('/', "_")),
            label: "Module".to_string(),
            properties: props,
        }
    }).collect();

    let filtered_edges: Vec<_> = module_edges.into_iter()
        .filter(|(s, d, _, _)| module_set.contains(s.as_str()) && module_set.contains(d.as_str()))
        .filter(|(s, d, _, _)| s != d)
        .map(|(src, dst, kind, weight)| atree_engine::graph::GraphEdge {
            id: format!("me:{}:{}", src.replace('/', "_"), dst.replace('/', "_")),
            source_id: format!("mod:{}", src.replace('/', "_")),
            target_id: format!("mod:{}", dst.replace('/', "_")),
            rel_type: if kind == "CALLS" { atree_engine::graph::RelationshipType::Calls } else { atree_engine::graph::RelationshipType::Uses },
            confidence: (weight as f64 / 100.0).min(1.0),
            reason: String::new(),
            step: None,
        })
        .collect();

    let config = build_layout_config(query, &nodes);
    let result_layout = layout::compute_layout(&nodes, &filtered_edges, &config);
    Json(serde_json::json!({
        "layout": result_layout,
        "nodes_returned": nodes.len(),
        "edges_returned": filtered_edges.len(),
        "size_class": size_class,
        "view": "module",
    })).into_response()
}

/// Graph overview — summary stats without loading all nodes.
async fn graph_overview(State(state): State<Arc<AppState>>) -> axum::response::Response {
    let store = match state.open_store().await { Some(s) => s, None => return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error": "No index available"}))).into_response() };

    let (files, symbols, edges) = match store.stats() {
        Ok(s) => (s.files, s.symbols, s.edges),
        Err(e) => { tracing::warn!(error = %e, "Failed to load stats"); return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({"error": "Failed to load stats"}))).into_response(); }
    };

    let graph_meta = store.get_all_graph_metadata().ok().flatten();
    let size_class = graph_meta.as_ref().and_then(|m| m.get("size_class")).map(|s| s.as_str()).unwrap_or("small");
    let default_view = graph_meta.as_ref().and_then(|m| m.get("default_view")).map(|s| s.as_str()).unwrap_or("full");
    let max_layout_nodes = graph_meta.as_ref().and_then(|m| m.get("max_layout_nodes")).and_then(|s| s.parse::<usize>().ok()).unwrap_or(500);
    let file_edge_count = graph_meta.as_ref().and_then(|m| m.get("file_edge_count")).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
    let module_edge_count = graph_meta.as_ref().and_then(|m| m.get("module_edge_count")).and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);

    // Count communities (each community_id represents one detected cluster).
    let community_count: i64 = store.conn().query_row("SELECT COUNT(*) FROM communities", [], |r| r.get::<_, i64>(0)).unwrap_or(0);

    Json(serde_json::json!({
        "files": files,
        "symbols": symbols,
        "edges": edges,
        "size_class": size_class,
        "default_view": default_view,
        "max_layout_nodes": max_layout_nodes,
        "file_edge_count": file_edge_count,
        "module_edge_count": module_edge_count,
        "community_count": community_count,
        "recommendation": match size_class {
            "xlarge" => "Use module view or search for specific symbols. Full graph has too many nodes.",
            "large" => "Use file or module view for overview. Search for specific symbols to see details.",
            "medium" => "Full graph view works well. Use zoom and pan to navigate.",
            _ => "Full graph view recommended for this codebase size.",
        },
    })).into_response()
}

/// Community graph — returns community-level clusters with their connections.
async fn graph_communities(State(state): State<Arc<AppState>>) -> axum::response::Response {
    let store = match state.open_store().await { Some(s) => s, None => return (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({"error": "No index available"}))).into_response() };

    let communities = match store.conn().prepare(
        "SELECT c.community_id, c.label, c.symbol_count, c.cohesion, c.modularity
         FROM communities c
         WHERE c.symbol_count > 0
         ORDER BY c.symbol_count DESC LIMIT 200"
    ) {
        Ok(mut stmt) => {
            match stmt.query_map([], |row| {
                Ok(serde_json::json!({
                    "id": row.get::<_, String>(0)?,
                    "label": row.get::<_, String>(1)?,
                    "symbol_count": row.get::<_, i64>(2)?,
                    "cohesion": row.get::<_, f64>(3)?,
                    "modularity": row.get::<_, f64>(4)?,
                }))
            }) {
                Ok(rows) => rows.filter_map(|r| r.ok()).collect::<Vec<_>>(),
                Err(_) => vec![],
            }
        }
        Err(_) => vec![],
    };

    Json(serde_json::json!({ "communities": communities })).into_response()
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn build_layout_config(query: &LayoutQuery, _nodes: &[atree_engine::graph::GraphNode]) -> LayoutConfig {
    let types_filter: Vec<String> = if query.types.is_empty() { vec![] } else { query.types.split(',').map(|s| s.trim().to_string()).collect() };
    let edges_filter: Vec<String> = if query.edges.is_empty() { vec![] } else { query.edges.split(',').map(|s| s.trim().to_string()).collect() };
    let iterations = query.iterations.min(500);
    let algorithm = match query.algorithm.as_str() { "layered" => layout::LayoutAlgorithm::LayeredDAG, _ => layout::LayoutAlgorithm::ForceDirected };
    LayoutConfig { algorithm, iterations, threads: 0, seed: query.seed, node_type_filter: types_filter, edge_filter: edges_filter, ..Default::default() }
}

// ── Route builder ────────────────────────────────────────────────────────────

pub fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/stats", get(stats))
        .route("/api/metrics", get(metrics))
        .route("/api/search", get(search))
        .route("/api/search/semantic", get(semantic_search))
        .route("/api/graph/node/{id}", get(node_detail))
        .route("/api/graph/query", post(graph_query))
        .route("/api/graph/layout", get(graph_layout))
        .route("/api/graph/focus", post(graph_focus))
        .route("/api/graph/overview", get(graph_overview))
        .route("/api/graph/communities", get(graph_communities))
        .route("/api/events", get(graph_events))
        .route("/api/webhook/push", post(webhook_push))
        .nest_service("/static", ServeDir::new("atree-web/static"))
        .fallback_service(ServeDir::new("atree-web/static").append_index_html_on_directories(true))
        .with_state(state)
        .layer(DefaultBodyLimit::max(8 * 1024 * 1024))
        .layer(CorsLayer::new()
            .allow_origin(tower_http::cors::AllowOrigin::exact(
                "http://localhost:3020".parse::<axum::http::HeaderValue>().expect("valid localhost origin")))
            .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
            .allow_headers(tower_http::cors::Any))
}
