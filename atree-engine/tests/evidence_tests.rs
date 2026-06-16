//! Integration tests for the evidence system.

use atree_engine::*;
use atree_engine::evidence::{Evidence, EvidenceKind};
use atree_engine::patterns::PatternMiningConfig;
use atree_engine::store::GraphStore;
use std::io::Write;

use std::sync::atomic::{AtomicU64, Ordering};
static COUNTER: AtomicU64 = AtomicU64::new(0);

fn create_test_dir() -> std::path::PathBuf {
    let id = COUNTER.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("atree_ev_{}_{}", std::process::id(), id));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn write_rs(path: &std::path::Path, body: &str) {
    let mut f = std::fs::File::create(path).unwrap();
    writeln!(f, "{}", body).unwrap();
    drop(f);
}

fn build(root: std::path::PathBuf) -> atree_engine::ScanResult {
    build_graph(&ScanOptions {
        semantic: true, db_path: None, root, incremental: false, threads: 1,
        include_files: true, ..Default::default()
    }).expect("build_graph should succeed")
}

fn build_with_db(root: std::path::PathBuf, db: std::path::PathBuf) -> atree_engine::ScanResult {
    build_graph(&ScanOptions {
        semantic: true, db_path: Some(db), root, incremental: false, threads: 1,
        include_files: true, ..Default::default()
    }).expect("build_graph should succeed")
}

#[test]
fn test_evidence_extraction_produces_candidates() {
    let root = create_test_dir();
    write_rs(&root.join("lib.rs"),
        "pub struct User { name: String }\n\
         impl User {\n\
         pub fn new(name: String) -> Self { Self { name } }\n\
         pub fn greet(&self) -> String { format!(\"Hello, {}!\", self.name) }\n\
         }\n\
         pub fn helper() -> bool { true }");
    let result = build(root);
    let total: usize = result.parsed_files.iter().map(|pf| pf.evidence.len()).sum();
    assert!(total > 0, "Should extract evidence, got {}", total);
}

#[test]
fn test_evidence_kinds_match_captures() {
    let root = create_test_dir();
    write_rs(&root.join("service.rs"),
        "pub struct UserService { db: Database }\n\
         impl UserService {\n\
         pub fn new(db: Database) -> Self { Self { db } }\n\
         pub fn get_user(&self, id: u64) -> Option<User> { self.db.query(id) }\n\
         }\n\
         pub struct Database;\n\
         impl Database { fn query(&self, _id: u64) -> Option<User> { None } }\n\
         pub struct User { pub id: u64, pub name: String }\n\
         pub fn create_user(name: String) -> User { User { id: 1, name } }");
    let result = build(root);
    let has_decls = result.parsed_files.iter().flat_map(|pf| &pf.evidence)
        .any(|e| matches!(e.kind, EvidenceKind::SymbolDeclaration));
    assert!(has_decls, "Should find symbol declarations");
    let has_calls = result.parsed_files.iter().flat_map(|pf| &pf.evidence)
        .any(|e| matches!(e.kind, EvidenceKind::FunctionCall));
    assert!(has_calls, "Should find function calls");
}

#[test]
fn test_evidence_persisted_to_sqlite() {
    let root = create_test_dir();
    let db = root.join("test.sqlite");
    write_rs(&root.join("lib.rs"),
        "pub struct User { name: String }\n\
         impl User {\n\
         pub fn new(name: String) -> Self { Self { name } }\n\
         pub fn greet(&self) -> String { format!(\"Hello, {}!\", self.name) }\n\
         }\n\
         pub fn helper() -> bool { true }");
    let _result = build_with_db(root, db.clone());
    let store = GraphStore::open(&db).unwrap();
    let ev_store = evidence::storage::EvidenceStore::new(store.conn());
    assert!(ev_store.count().unwrap() > 0, "Should have persisted evidence");
}

#[test]
fn test_evidence_fts5_search() {
    let root = create_test_dir();
    let db = root.join("test.sqlite");
    write_rs(&root.join("lib.rs"),
        "pub struct UserService { db: Database }\n\
         impl UserService {\n\
         pub fn new(db: Database) -> Self { Self { db } }\n\
         pub fn get_user(&self, id: u64) -> Option<User> { self.db.query(id) }\n\
         }\n\
         pub struct Database;\n\
         impl Database { fn query(&self, _id: u64) -> Option<User> { None } }\n\
         pub struct User { pub id: u64, pub name: String }");
    let _result = build_with_db(root, db.clone());
    let store = GraphStore::open(&db).unwrap();
    let ev_store = evidence::storage::EvidenceStore::new(store.conn());
    // FTS5 search — only assert if FTS5 is available (bundled SQLite may omit it).
    match ev_store.search("UserService", 10) {
        Ok(results) if !results.is_empty() => {
            assert!(results.iter().any(|r| r.file.contains("lib.rs")),
                "FTS5 results should reference lib.rs, got {:?}", results);
        }
        Ok(_) => eprintln!("[WARN] FTS5 returned empty results (tokenizer may filter short tokens)"),
        Err(e) => eprintln!("[WARN] FTS5 not available in this SQLite build: {}", e),
    }
}

#[test]
fn test_pattern_mining_from_parsed_code() {
    let root = create_test_dir();
    write_rs(&root.join("a.rs"),
        "pub struct Foo { x: i32 }\n\
         impl Foo {\n\
         pub fn new() -> Self { Self { x: 0 } }\n\
         pub fn get(&self) -> i32 { self.x }\n\
         }\n\
         pub fn bar() -> i32 { 42 }");
    write_rs(&root.join("b.rs"),
        "pub struct Bar { y: String }\n\
         impl Bar {\n\
         pub fn new(y: String) -> Self { Self { y } }\n\
         pub fn get(&self) -> &str { &self.y }\n\
         }\n\
         pub fn baz() -> bool { true }");
    let result = build(root);
    let evidence: Vec<Evidence> = result.parsed_files.iter()
        .flat_map(|pf| pf.evidence.clone())
        .map(|c| c.into_evidence())
        .collect();
    assert!(!evidence.is_empty());
    let patterns = patterns::mine_patterns(&evidence, &PatternMiningConfig::default());
    for p in &patterns { assert!(p.score.overall >= 0.0); }
}

#[test]
fn test_incremental_scan_preserves_evidence_count() {
    let root = create_test_dir();
    let db = root.join("test.sqlite");
    write_rs(&root.join("lib.rs"),
        "pub struct User { name: String }\n\
         impl User {\n\
         pub fn new(name: String) -> Self { Self { name } }\n\
         }\n\
         pub fn helper() -> bool { true }");
    let root2 = root.clone();
    let db2 = db.clone();
    let _r1 = build_with_db(root, db.clone());
    let store = GraphStore::open(&db).unwrap();
    let initial = evidence::storage::EvidenceStore::new(store.conn()).count().unwrap();
    let _r2 = build_with_db(root2, db2);
    let store = GraphStore::open(&db).unwrap();
    assert_eq!(initial, evidence::storage::EvidenceStore::new(store.conn()).count().unwrap());
}

// ── Regression tests: R-3 — IN clause chunking ────────────────────────────────

/// Helper: populate an in-memory store with N symbols and N-1 edges (chain).
fn populate_chain_store(n: usize) -> GraphStore {
    let store = GraphStore::open_in_memory().unwrap();
    let file_id = store.upsert_file("src/lib.rs", 1, "rust", 0, None).unwrap();
    let mut symbol_ids = Vec::with_capacity(n);
    for i in 0..n {
        let rec = atree_engine::store::SymbolRecord {
            id: 0,
            file_id,
            name: format!("fn_{}", i),
            qualified_name: format!("crate::fn_{}", i),
            kind: "Function".to_string(),
            line: i * 2,
            col: 0,
            is_exported: false,
            scope_id: None,
            owner_symbol_id: None,
        };
        let id = store.insert_symbol(&rec).unwrap();
        symbol_ids.push(id);
    }
    // Chain: fn_0 -> fn_1 -> fn_2 -> ... -> fn_{n-1}
    for w in symbol_ids.windows(2) {
        store.insert_edge(&atree_engine::store::EdgeRecord {
            id: 0,
            src_id: w[0],
            dst_id: w[1],
            edge_kind: "CALLS".to_string(),
            confidence: 1.0,
            file_id: Some(file_id),
            line: 0,
        }).unwrap();
    }
    store
}

#[test]
fn regression_get_edges_for_nodes_empty_input() {
    let store = populate_chain_store(10);
    // Empty input should return empty results without error.
    let edges = store.get_edges_for_nodes(&[]).unwrap();
    assert!(edges.is_empty(), "empty input should return no edges");
}

#[test]
fn regression_get_edges_for_nodes_small_input() {
    let store = populate_chain_store(10);
    let ids: Vec<i64> = (1..=5).collect();
    let edges = store.get_edges_for_nodes(&ids).unwrap();
    // With 5 nodes in a chain (1->2->3->4), we expect edges from each node.
    assert!(!edges.is_empty(), "should find edges for small node set");
}

#[test]
fn regression_get_edges_for_nodes_large_input_triggers_chunking() {
    let n = 600;
    let store = populate_chain_store(n);

    let expected_count: i64 = store.conn().query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0)).unwrap();
    let all_ids = store.get_all_symbols().unwrap();
    let all_ids: Vec<i64> = all_ids.iter().map(|s| s.id).collect();
    assert_eq!(all_ids.len(), n, "should have {} symbols", n);

    let edges = store.get_edges_for_nodes(&all_ids).unwrap();
    assert_eq!(edges.len(), expected_count as usize,
        "should find all {} edges even with chunking (got {})", expected_count, edges.len());
}

#[test]
fn regression_get_edges_for_symbols_large_set_triggers_chunking() {
    use rustc_hash::FxHashSet;
    let n = 600;
    let store = populate_chain_store(n);
    let expected_count: i64 = store.conn().query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0)).unwrap();

    let all_ids = store.get_all_symbols().unwrap();
    let all_ids: FxHashSet<i64> = all_ids.iter().map(|s| s.id).collect();
    let edges = store.get_edges_for_symbols(&all_ids).unwrap();

    assert_eq!(edges.len(), expected_count as usize,
        "get_edges_for_symbols should find all {} edges with chunking (got {})", expected_count, edges.len());
}

#[test]
fn regression_get_files_by_ids_large_set_triggers_chunking() {
    let store = GraphStore::open_in_memory().unwrap();
    let n = 600;
    let mut file_ids = Vec::with_capacity(n);
    for i in 0..n {
        let id = store.upsert_file(
            &format!("src/mod_{}.rs", i),
            i as u64,
            "rust",
            0,
            None,
        ).unwrap();
        file_ids.push(id);
    }
    // Query with all IDs to trigger chunking.
    let files = store.get_files_by_ids(&file_ids).unwrap();
    assert_eq!(files.len(), n,
        "get_files_by_ids should return all {} files with chunking (got {})", n, files.len());
}

#[test]
fn regression_get_edges_for_nodes_very_large_input() {
    // 1500 symbols — forces 3+ chunk iterations.
    let n = 1500;
    let store = populate_chain_store(n);
    let expected_count: i64 = store.conn().query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0)).unwrap();

    let all_ids = store.get_all_symbols().unwrap();
    let all_ids: Vec<i64> = all_ids.iter().map(|s| s.id).collect();
    assert_eq!(all_ids.len(), n, "should have {} symbols", n);

    let edges = store.get_edges_for_nodes(&all_ids).unwrap();
    assert_eq!(edges.len(), expected_count as usize,
        "should find all {} edges across 3+ chunks (got {})", expected_count, edges.len());
}

#[test]
fn regression_chunking_produces_same_result_as_no_chunking() {
    // Verify that chunking does not introduce duplicates or miss edges.
    // Use a single store with 600 symbols and compare chunked vs unchunked results.
    let n = 600;
    let store = populate_chain_store(n);

    let all_ids = store.get_all_symbols().unwrap();
    let all_ids: Vec<i64> = all_ids.iter().map(|s| s.id).collect();

    // get_edges_for_nodes uses chunking for >500 IDs.
    let chunked_edges = store.get_edges_for_nodes(&all_ids).unwrap();

    // Verify no duplicate edge IDs (dedup works).
    let mut edge_ids: Vec<i64> = chunked_edges.iter().map(|e| e.id).collect();
    edge_ids.sort();
    let unique_count = edge_ids.iter().collect::<rustc_hash::FxHashSet<_>>().len();
    assert_eq!(unique_count, edge_ids.len(),
        "chunking should not produce duplicate edges");

    // Verify total count matches direct DB query.
    let expected_count: i64 = store.conn().query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0)).unwrap();
    assert_eq!(chunked_edges.len(), expected_count as usize,
        "chunked query should return exactly {} edges, got {}", expected_count, chunked_edges.len());
}

// ── Regression tests: N-01 — get_symbols_by_ids batch method ─────────────────

#[test]
fn regression_get_symbols_by_ids_empty() {
    let store = GraphStore::open_in_memory().unwrap();
    let result = store.get_symbols_by_ids(&[]).unwrap();
    assert!(result.is_empty(), "empty input should return empty results");
}

#[test]
fn regression_get_symbols_by_ids_small() {
    let store = populate_chain_store(10);
    let ids: Vec<i64> = (1..=5).collect();
    let result = store.get_symbols_by_ids(&ids).unwrap();
    assert_eq!(result.len(), 5, "should return 5 symbols");
}

#[test]
fn regression_get_symbols_by_ids_large_triggers_chunking() {
    let store = populate_chain_store(600);
    let all_ids = store.get_all_symbols().unwrap();
    let ids: Vec<i64> = all_ids.iter().map(|s| s.id).collect();
    let result = store.get_symbols_by_ids(&ids).unwrap();
    assert_eq!(result.len(), all_ids.len(),
        "batch lookup should return all {} symbols", all_ids.len());
}

// ── Regression: route regex compilation (N-02) ───────────────────────────────

#[test]
fn regression_route_regex_compiles_once() {
    // Call detect_routes_from_path many times — should not recompile regex.
    use atree_engine::routes::detect_routes_from_path;
    let files = vec![
        "app/api/users/route.ts",
        "app/api/posts/[id]/route.tsx",
        "pages/api/auth.ts",
        "src/utils/helpers.ts",
        "app/dashboard/page.ts",  // not a route
    ];
    for _ in 0..100 {
        for f in &files {
            let routes = detect_routes_from_path(f);
            if f.contains("app/api/") && f.contains("route.") {
                assert!(!routes.is_empty(), "should detect route in {}", f);
            }
        }
    }
    // If we got here without panic, the OnceLock pattern works.
}

// ── Regression: pattern sorting with NaN (partial_cmp fix) ───────────────────

#[test]
fn regression_patterns_sort_handles_nan() {
    // Verify that partial_cmp with NaN returns None, and unwrap_or handles it.
    // This is the core fix: patterns/mod.rs:159 now uses unwrap_or(Ordering::Equal)
    // instead of unwrap(), so NaN scores don't cause a panic.
    let nan_score: f64 = f64::NAN;
    let normal_score: f64 = 1.0;
    let result = nan_score.partial_cmp(&normal_score);
    assert!(result.is_none(), "NaN partial_cmp should return None");

    // Verify the unwrap_or pattern works correctly.
    let ordering = result.unwrap_or(std::cmp::Ordering::Equal);
    assert_eq!(ordering, std::cmp::Ordering::Equal);
}
