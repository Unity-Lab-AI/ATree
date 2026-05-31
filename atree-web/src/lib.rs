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
use std::sync::atomic::AtomicU64;
use tokio::net::TcpListener;
use tokio::signal;

/// Run the ATree web server.
///
/// # Arguments
/// * `db_path` — Path to the SQLite index (`.atree/index.sqlite`)
/// * `repo_path` — Path to the repository root (for display)
/// * `port` — Port to listen on
pub async fn run(db_path: PathBuf, repo_path: String, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    let start_time = std::time::Instant::now();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), %port, "atree-web starting");

    let webhook_secret = std::env::var("ATREE_WEBHOOK_SECRET").ok()
        .filter(|s| !s.is_empty());
    let state = Arc::new(server::AppState {
        event_bus: Arc::new(tokio::sync::RwLock::new(events::EventBus::new())),
        db_path: Some(db_path),
        repo_path: Some(repo_path),
        webhook_secret,
        webhook_inflight: Arc::new(AtomicU64::new(0)),
        webhook_last_ms: Arc::new(AtomicU64::new(0)),
    });

    let app = server::build_router(state);
    let addr = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&addr).await?;

    tracing::info!(%addr, "ATree web server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(start_time))
        .await?;
    Ok(())
}

async fn shutdown_signal(start_time: std::time::Instant) {
    let ctrl_c = async {
        signal::ctrl_c().await.expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    let uptime = start_time.elapsed().as_secs();
    tracing::info!(uptime_secs = uptime, "Shutdown signal received, stopping gracefully");
}

// Re-exports for convenience
pub use events::{EventBus, GraphFocusEvent};
pub use layout::{compute_layout, GraphLayout, LayoutConfig};
pub use server::AppState;

use std::sync::Arc;
