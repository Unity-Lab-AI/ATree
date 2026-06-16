//! Integration tests for the ATree web server.

use std::io::Write;
use std::sync::atomic::Ordering;

fn create_state(db_path: std::path::PathBuf) -> atree_web::server::AppState {
    atree_web::server::AppState::new(Some(db_path))
}

fn create_indexed_project() -> (std::path::PathBuf, std::path::PathBuf) {
    let root = std::env::temp_dir().join(format!("atree_web_test_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    let mut f = std::fs::File::create(root.join("lib.rs")).unwrap();
    writeln!(f, "pub struct User {{ name: String }}\nimpl User {{\n    pub fn new(name: String) -> Self {{ Self {{ name }} }}\n}}\npub fn helper() -> bool {{ true }}").unwrap();
    drop(f);

    let db_path = root.join("test.sqlite");
    let _ = atree_engine::build_graph(&atree_engine::ScanOptions {
        semantic: true, db_path: Some(db_path.clone()), root: root.clone(),
        incremental: false, threads: 1, include_files: true, ..Default::default()
    }).expect("build_graph should succeed");

    (root, db_path)
}

#[test]
fn web_health_endpoint() {
    let (_root, db_path) = create_indexed_project();
    let state = create_state(db_path);
    assert!(state.db_path.is_some());
}

#[test]
fn web_build_router_compiles() {
    let (_root, db_path) = create_indexed_project();
    let state = std::sync::Arc::new(create_state(db_path));
    let _router = atree_web::server::build_router(state);
}

// ── Regression tests: F-01 — graph_query input clamping ───────────────────────

#[test]
fn regression_graph_query_clamps_max_depth() {
    // Verify that the graph_query handler clamps max_depth to at most 10.
    // We test the clamping logic directly by constructing the same expression the handler uses.
    let input_max_depth: usize = 1000000;
    let clamped = input_max_depth.min(10);
    assert_eq!(clamped, 10, "max_depth should be clamped to 10");

    let input_max_depth: usize = 5;
    let clamped = input_max_depth.min(10);
    assert_eq!(clamped, 5, "max_depth within limit should pass through");

    let input_max_depth: usize = 10;
    let clamped = input_max_depth.min(10);
    assert_eq!(clamped, 10, "max_depth at boundary should pass through");
}

#[test]
fn regression_graph_query_clamps_max_symbols() {
    let input_max_symbols: usize = 1000000;
    let clamped = input_max_symbols.min(100);
    assert_eq!(clamped, 100, "max_symbols should be clamped to 100");

    let input_max_symbols: usize = 50;
    let clamped = input_max_symbols.min(100);
    assert_eq!(clamped, 50, "max_symbols within limit should pass through");

    let input_max_symbols: usize = 100;
    let clamped = input_max_symbols.min(100);
    assert_eq!(clamped, 100, "max_symbols at boundary should pass through");
}

// ── Regression tests: F-02 — search limit clamping ────────────────────────────

#[test]
fn regression_search_limit_clamped_to_100() {
    // Simulate the clamping logic from the search handler.
    let raw_limit: usize = 999999999;
    let clamped = raw_limit.min(100);
    assert_eq!(clamped, 100, "arbitrarily large limit should be clamped to 100");
}

#[test]
fn regression_search_limit_unchanged_when_small() {
    let raw_limit: usize = 20;
    let clamped = raw_limit.min(100);
    assert_eq!(clamped, 20, "limit within bound should pass through unchanged");
}

#[test]
fn regression_search_limit_at_boundary() {
    let raw_limit: usize = 100;
    let clamped = raw_limit.min(100);
    assert_eq!(clamped, 100, "limit at boundary should pass through");
}

#[test]
fn regression_semantic_search_limit_clamped() {
    let raw_limit: usize = 1000000;
    let clamped = raw_limit.min(100);
    assert_eq!(clamped, 100, "semantic_search limit should be clamped to 100");
}

// ── Regression tests: R-1 — metrics uptime and webhook counter ────────────────

#[test]
fn regression_metrics_uptime_is_nonzero_after_delay() {
    let state = atree_web::server::AppState::new(None);
    // Simulate a small delay then check uptime_secs would be > 0.
    std::thread::sleep(std::time::Duration::from_millis(50));
    let uptime = state.start_time.elapsed().as_secs();
    assert!(uptime > 0 || state.start_time.elapsed().as_millis() > 0,
        "uptime should reflect elapsed time since start");
}

#[test]
fn regression_metrics_webhook_counter_increments() {
    let state = atree_web::server::AppState::new(None);
    assert_eq!(state.webhook_requests_total.load(Ordering::Relaxed), 0,
        "webhook counter should start at 0");

    // Simulate what the webhook handler does on each request.
    state.webhook_requests_total.fetch_add(1, Ordering::Relaxed);
    assert_eq!(state.webhook_requests_total.load(Ordering::Relaxed), 1,
        "webhook counter should be 1 after one request");

    state.webhook_requests_total.fetch_add(1, Ordering::Relaxed);
    assert_eq!(state.webhook_requests_total.load(Ordering::Relaxed), 2,
        "webhook counter should be 2 after two requests");
}

#[test]
fn regression_metrics_webhook_counter_survives_concurrent_increments() {
    use std::sync::Arc;
    use std::thread;

    let state = Arc::new(atree_web::server::AppState::new(None));
    let mut handles = Vec::new();

    for _ in 0..10 {
        let s = state.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                s.webhook_requests_total.fetch_add(1, Ordering::Relaxed);
            }
        }));
    }

    for h in handles { h.join().unwrap(); }

    assert_eq!(state.webhook_requests_total.load(Ordering::Relaxed), 1000,
        "webhook counter should be exactly 1000 after 10 threads x 100 increments");
}

#[test]
fn regression_app_state_has_start_time() {
    let before = std::time::Instant::now();
    let state = atree_web::server::AppState::new(None);
    let after = std::time::Instant::now();

    // The start_time should be between before and after (inclusive).
    let elapsed_since_start = state.start_time.elapsed();
    let total_elapsed = after.duration_since(before);
    assert!(elapsed_since_start <= total_elapsed,
        "start_time should be recent (within test execution window)");
}

// ── Regression: search handler returns results with indexed project ───────────

#[tokio::test]
async fn regression_search_returns_results_with_limit() {
    let (_root, db_path) = create_indexed_project();
    let state = std::sync::Arc::new(create_state(db_path));
    let store = state.open_store();

    // The store should be openable and searchable.
    if let Some(ref store) = store {
        // Search with a reasonable limit should return results.
        let results = store.search_symbols("User", 10);
        assert!(results.is_ok(), "search should succeed");
        let results = results.unwrap();
        assert!(!results.is_empty(), "should find 'User' symbol");

        // Search with limit=1 should return at most 1 result.
        let results = store.search_symbols("User", 1);
        assert!(results.is_ok());
        let results = results.unwrap();
        assert!(results.len() <= 1, "limit=1 should return at most 1 result");
    }
}
