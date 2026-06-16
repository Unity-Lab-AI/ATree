//! Server-Sent Events (SSE) for real-time graph focus.
//!
//! Agents (via MCP tools) can POST to `/api/graph/focus` to shift the
//! graph view. All connected SSE clients (browser tabs) receive the focus
//! event and smoothly animate the camera to the target node/subgraph.

use axum::{
    response::sse::{Event, KeepAlive, Sse},
};
use futures::stream::{self, Stream};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

// ── Types ────────────────────────────────────────────────────────────────────

/// A graph focus event — broadcast to all SSE clients.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphFocusEvent {
    /// Event type: "focus_node" | "focus_subgraph" | "highlight" | "clear" | "blast_radius"
    pub event_type: String,
    /// Target node IDs
    pub node_ids: Vec<String>,
    /// Human-readable label for the focus
    pub label: String,
    /// Metadata about what triggered this focus
    pub source: String,
    /// Optional: zoom level (0 = fit all, higher = zoomed in)
    pub zoom: Option<f64>,
    /// Optional: animation duration in ms
    pub anim_duration_ms: Option<u64>,
}

// ── EventBus ─────────────────────────────────────────────────────────────────

pub struct EventBus {
    tx: broadcast::Sender<GraphFocusEvent>,
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(256);
        Self { tx }
    }

    pub fn publish(&self, event: GraphFocusEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<GraphFocusEvent> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

// ── SSE stream ───────────────────────────────────────────────────────────────

pub fn create_sse_stream(
    rx: broadcast::Receiver<GraphFocusEvent>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>> + Send + 'static> {
    let stream = stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(event) => {
                let json = serde_json::to_string(&event).unwrap_or_default();
                Some((Ok(Event::default().data(json).event("graph_focus")), rx))
            }
            Err(_) => None,
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}
