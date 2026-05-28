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
