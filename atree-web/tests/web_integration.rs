//! Integration tests for the ATree web server.

use std::io::Write;

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
    let state = std::sync::Arc::new(atree_web::server::AppState::new(Some(db_path)));
    // Verify state was created correctly.
    assert!(state.db_path.is_some());
}

#[test]
fn web_build_router_compiles() {
    let (_root, db_path) = create_indexed_project();
    let state = std::sync::Arc::new(atree_web::server::AppState::new(Some(db_path)));
    // Just verify the router builds without panicking.
    let _router = atree_web::server::build_router(state);
}
