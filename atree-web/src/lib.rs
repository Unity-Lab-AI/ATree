//! ATree Web — Visual code intelligence graph with real-time agent focus.
//!
//! Provides an HTTP server that:
//! - Serves a self-contained web UI (Canvas-based graph visualization)
//! - Computes force-directed graph layouts in parallel (Rust, server-side)
//! - Streams real-time graph focus shifts via SSE (Server-Sent Events)
//! - Exposes MCP-compatible endpoints for agent tool integration
//!
//! ## Architecture
//!
//! ```text
//!   Agent (Crush/Claude)
//!       │  POST /api/graph/focus
//!       ▼
//!   ┌─────────────┐    SSE    ┌──────────────┐
//!   │  atree-web   │ ───────► │  Browser UI   │
//!   │  (axum)      │          │  (Canvas)     │
//!   └──────┬──────┘          └──────────────┘
//!          │ reads
//!          ▼
//!   ┌─────────────┐
//!   │  GraphStore  │  (SQLite, populated by atree --semantic)
//!   └─────────────┘
//! ```

pub mod events;
pub mod layout;
pub mod server;

use std::path::PathBuf;
use tokio::net::TcpListener;

/// Run the ATree web server.
///
/// # Arguments
/// * `db_path` — Path to the SQLite index (`.atree/index.sqlite`)
/// * `repo_path` — Path to the repository root (for display)
/// * `port` — Port to listen on
pub async fn run(db_path: PathBuf, repo_path: String, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let webhook_secret = std::env::var("ATREE_WEBHOOK_SECRET").ok();
    let state = Arc::new(server::AppState {
        event_bus: Arc::new(tokio::sync::RwLock::new(events::EventBus::new())),
        db_path: Some(db_path),
        repo_path: Some(repo_path),
        webhook_secret,
    });

    let app = server::build_router(state);
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;

    log::info!("ATree web server listening on http://{}", addr);
    log::info!("Open in browser: http://localhost:{}", port);

    axum::serve(listener, app).await?;
    Ok(())
}

// Re-exports for convenience
pub use events::{EventBus, GraphFocusEvent};
pub use layout::{compute_layout, GraphLayout, LayoutConfig};
pub use server::AppState;

use std::sync::Arc;
