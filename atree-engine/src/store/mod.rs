//! Persistent graph store using SQLite.
//!
//! Schema mirrors GitNexus's LadybugDB schema but in relational form:
//! - files: path, hash, language, mtime, indexed_at
//! - symbols: id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id
//! - scopes: id, file_id, parent_id, owner_symbol_id, kind, line_start, line_end
//! - imports: id, file_id, source, imported_name, local_name, resolved_file_id, confidence
//! - exports: id, file_id, exported_name, symbol_id, is_default
//! - calls: id, file_id, caller_scope_id, callee_name, receiver, resolved_symbol_id, confidence, line, col
//! - edges: src_id, dst_id, edge_kind, confidence, file_id, line
//!
//! Recursive CTEs enable graph traversal (call chains, impact analysis, etc.).

use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;

/// Maximum recursion depth for call-graph CTEs.
/// SQLite's default expression depth limit is 1000; mutual recursion in real
/// codebases is typically <10 levels. 20 is a safe practical cap.
pub const MAX_CTE_DEPTH: i64 = 20;
use serde::{Serialize, Deserialize};

/// Maximum time to wait for the advisory file lock (seconds).
/// Allowed tables and their permitted columns for cypher queries.
pub const ALLOWED_TABLES: &[(&str, &[&str])] = &[
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
/// - Tables not in the allowlist
pub fn validate_cypher_query(query: &str) -> Result<(), String> {
    let lower = query.to_lowercase();

    // Block dangerous patterns. Multi-word and special-char patterns use
    // substring matching; single-word keywords use word-boundary matching
    // to avoid false positives (e.g. "selection" containing "select").
    let blocked_substr = [
        "sqlite_master", "sqlite_temp_master", "pg_catalog", "information_schema",
        ";", "--", "/*", "*/",
    ];
    for pat in &blocked_substr {
        if lower.contains(pat) {
            return Err(format!("Query contains forbidden pattern: '{}'", pat));
        }
    }

    // Single-word keywords: check with word boundaries (alphanumeric/underscore delimited).
    let blocked_words = [
        "insert", "update", "delete", "drop", "alter", "create",
        "attach", "detach", "replace", "pragma", "union",
    ];
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric() && c != '_').collect();
    for word in &words {
        if blocked_words.contains(word) {
            return Err(format!("Query contains forbidden keyword: '{}'", word));
        }
    }

    // Must start with SELECT or WITH.
    let trimmed = lower.trim();
    if !trimmed.starts_with("select") && !trimmed.starts_with("with") {
        return Err("Only SELECT and WITH queries are allowed".to_string());
    }

    let table_names: std::collections::HashSet<&str> = ALLOWED_TABLES.iter().map(|(t, _)| *t).collect();
    let sql_keywords = ["select", "from", "where", "join", "left", "right", "inner",
        "outer", "on", "and", "or", "not", "in", "is", "null", "as", "group",
        "order", "by", "limit", "offset", "having", "union", "all", "distinct",
        "case", "when", "then", "else", "end", "exists", "between", "like",
        "count", "sum", "avg", "min", "max", "asc", "desc", "using",
        "with", "recursive", "cast", "coalesce"];
    for word in &words {
        if word.is_empty() { continue; }
        if sql_keywords.contains(word) { continue; }
        if table_names.contains(word) { continue; }
        if word.chars().next().map_or(false, |c| c.is_alphabetic()) && word.len() > 1 {
            return Err(format!("Query references unknown identifier: '{}'", word));
        }
    }

    Ok(())
}

/// Persistent graph store backed by SQLite.
pub struct GraphStore {
    conn: Connection,
}

/// Begin a transaction on a raw Connection, returning an error instead of
/// panicking if a transaction is already active. Use this instead of
/// `conn.unchecked_transaction()` to avoid the maintenance trap of
/// unchecked panics on nested calls.
pub fn begin_transaction(conn: &Connection) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    conn.unchecked_transaction()
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to begin transaction (possible nested call)");
            e
        })
}

/// A data flow edge: value flows from src to dst.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataFlowRecord {
    pub id: i64,
    pub file_id: i64,
    pub src_symbol_id: i64,
    pub dst_symbol_id: i64,
    pub flow_kind: String, // 'assignment' | 'param_pass' | 'return' | 'property_read' | 'property_write'
    pub var_name: String,
    pub line: usize,
    pub col: usize,
    pub confidence: f64,
}

impl GraphStore {
    /// Open or create a graph store at the given path.
    ///
    /// Uses SQLite's built-in locking via WAL mode + a busy timeout to prevent
    /// concurrent scans from corrupting the index. Combined with `synchronous = NORMAL`,
    /// this provides crash-safe persistence without data loss on power failure.
    pub fn open<P: AsRef<Path>>(path: P) -> rusqlite::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(1), Some(format!("Failed to create DB directory: {}", e))
                )
            })?;
        }
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.init_pragmas()?;
        store.run_migrations()?;
        Ok(store)
    }

    /// Create an in-memory graph store (for testing).
    ///
    /// No file lock is needed for in-memory databases.
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.init_pragmas()?;
        store.run_migrations()?;
        Ok(store)
    }

    /// Get a reference to the underlying SQLite connection.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Begin a transaction, returning an error if one is already active.
    /// Begin a transaction, returning an error if one is already active.
    /// Safe wrapper — logs and returns an error instead of panicking on nested calls.
    /// Use this instead of `conn().unchecked_transaction()` directly.
    pub fn begin_transaction(&self) -> rusqlite::Result<rusqlite::Transaction<'_>> {
        begin_transaction(&self.conn)
    }

    /// Run WAL checkpoint and ANALYZE to keep the database performant.
    ///
    /// Call this periodically (e.g. after large batch writes or on a timer).
    /// WAL checkpoint truncates the write-ahead log; ANALYZE updates table
    /// statistics so the query planner chooses good indexes.
    pub fn maintenance(&self) -> rusqlite::Result<()> {
        tracing::debug!("Running DB maintenance: WAL checkpoint + ANALYZE");
        // PASSIVE checkpoint: moves data from WAL to main DB without blocking readers.
        self.conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE);")?;
        // ANALYZE: gather statistics on all tables for the query planner.
        self.conn.execute_batch("ANALYZE;")?;
        tracing::info!("DB maintenance complete");
        Ok(())
    }

    /// Get the current database file size in bytes.
    pub fn db_size_bytes(&self) -> u64 {
        self.conn
            .query_row("SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u64
    }

    /// Get a summary of the index size and row counts.
    pub fn stats(&self) -> rusqlite::Result<StoreStats> {
        let files: i64 = self.conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap_or(0);
        let symbols: i64 = self.conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0)).unwrap_or(0);
        let scopes: i64 = self.conn.query_row("SELECT COUNT(*) FROM scopes", [], |r| r.get(0)).unwrap_or(0);
        let imports: i64 = self.conn.query_row("SELECT COUNT(*) FROM imports", [], |r| r.get(0)).unwrap_or(0);
        let calls: i64 = self.conn.query_row("SELECT COUNT(*) FROM calls", [], |r| r.get(0)).unwrap_or(0);
        let edges: i64 = self.conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0)).unwrap_or(0);
        let resolved_calls: i64 = self.conn.query_row("SELECT COUNT(*) FROM calls WHERE resolved_symbol_id IS NOT NULL", [], |r| r.get(0)).unwrap_or(0);
        let evidence: i64 = self.conn.query_row("SELECT COUNT(*) FROM evidence", [], |r| r.get(0)).unwrap_or(0);
        Ok(StoreStats {
            files,
            symbols,
            scopes,
            imports,
            calls,
            edges,
            resolved_calls,
            files_inserted: 0,
            files_reused: 0,
            evidence,
            db_size_bytes: self.db_size_bytes(),
        })
    }

    /// Try running maintenance, logging errors instead of failing.
    pub fn maintenance_or_warn(&self) {
        if let Err(e) = self.maintenance() {
            tracing::warn!(error = %e, "DB maintenance failed");
        }
    }

    /// Initialize SQLite PRAGMAs for safe, performant operation.
    ///
    /// Uses `synchronous = NORMAL` (not OFF) to prevent data corruption on
    /// power loss. Uses WAL mode for concurrent read performance. Disables
    /// mmap_size to prevent torn reads under concurrent access.
    fn init_pragmas(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        self.conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
        self.conn.execute_batch("PRAGMA cache_size = -20000;")?;
        self.conn.execute_batch("PRAGMA temp_store = MEMORY;")?;
        // mmap_size = 0: disable memory-mapped I/O to prevent torn reads
        // when another process writes concurrently.
        self.conn.execute_batch("PRAGMA mmap_size = 0;")?;
        // busy_timeout: wait up to 10 seconds for locks before failing,
        // preventing "database is locked" errors under concurrent access.
        self.conn.execute_batch("PRAGMA busy_timeout = 10000;")?;
        // foreign_keys: enforce REFERENCES constraints. Without this,
        // deletions of parent rows silently leave orphaned child rows.
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        // soft_heap_limit: abort queries that would use more than 512MB of heap.
        // This prevents accidental OOM from large JOINs or unbounded queries.
        self.conn.execute_batch("PRAGMA soft_heap_limit = 536870912;")?;

        // NOTE: per-query timeout via progress_handler is deferred.
        // rusqlite 0.31's progress_handler method on Connection is defined in
        // the hooks module but not accessible as an inherent method from
        // external crates. The soft_heap_limit PRAGMA (512MB) provides OOM
        // protection as an alternative safety net.

        Ok(())
    }

    /// Run schema migrations based on `PRAGMA user_version`.
    fn run_migrations(&self) -> rusqlite::Result<()> {
        let current_version: i32 = self.conn.query_row(
            "PRAGMA user_version", [], |r| r.get(0)
        ).unwrap_or(0);
        if current_version < 1 {
            log::info!("Running schema migration: v0 -> v1 (initial schema)");
            self.init_schema_tables()?;
            self.conn.execute_batch("PRAGMA user_version = 1;")?;
        }
        if current_version < 2 {
            log::info!("Running schema migration: v1 -> v2 (abstraction layers)");
            self.init_schema_v2()?;
            self.conn.execute_batch("PRAGMA user_version = 2;")?;
        }
        if current_version < 3 {
            log::info!("Running schema migration: v2 -> v3 (community persistence)");
            self.init_schema_v3()?;
            self.conn.execute_batch("PRAGMA user_version = 3;")?;
        }
        if current_version < 4 {
            log::info!("Running schema migration: v3 -> v4 (evidence storage)");
            self.init_schema_v4()?;
            self.conn.execute_batch("PRAGMA user_version = 4;")?;
        }
        if current_version < 5 {
            log::info!("Running schema migration: v4 -> v5 (patterns + constraints)");
            self.init_schema_v5()?;
            self.conn.execute_batch("PRAGMA user_version = 5;")?;
        }
        if current_version < 6 {
            log::info!("Running schema migration: v5 -> v6 (routes table)");
            self.init_schema_v6()?;
            self.conn.execute_batch("PRAGMA user_version = 6;")?;
        }
        if current_version < 7 {
            log::info!("Running schema migration: v6 -> v7 (missing indexes)");
            self.init_schema_v7()?;
            self.conn.execute_batch("PRAGMA user_version = 7;")?;
        }
        if current_version < 8 {
            log::info!("Running schema migration: v7 -> v8 (data flow tracking)");
            self.init_schema_v8()?;
            self.conn.execute_batch("PRAGMA user_version = 8;")?;
        }
        if current_version < 9 {
            log::info!("Running schema migration: v8 -> v9 (boundary violations)");
            self.init_schema_v9()?;
            self.conn.execute_batch("PRAGMA user_version = 9;")?;
        }
        Ok(())
    }

    /// Create the initial schema tables. Called by migration v0 -> v1.
    fn init_schema_tables(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                hash INTEGER NOT NULL,
                language TEXT NOT NULL,
                mtime INTEGER NOT NULL,
                indexed_at INTEGER NOT NULL,
                repo_label TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_files_hash ON files(hash);
            CREATE INDEX IF NOT EXISTS idx_files_path ON files(path);
            CREATE INDEX IF NOT EXISTS idx_files_repo ON files(repo_label);

            CREATE TABLE IF NOT EXISTS scopes (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id),
                parent_id INTEGER REFERENCES scopes(id),
                owner_symbol_id INTEGER,
                kind TEXT NOT NULL,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_scopes_file ON scopes(file_id);
            CREATE INDEX IF NOT EXISTS idx_scopes_parent ON scopes(parent_id);

            CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id),
                name TEXT NOT NULL,
                qualified_name TEXT NOT NULL,
                kind TEXT NOT NULL,
                line INTEGER NOT NULL,
                col INTEGER NOT NULL,
                is_exported INTEGER NOT NULL DEFAULT 0,
                scope_id INTEGER REFERENCES scopes(id),
                owner_symbol_id INTEGER
            );
            CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_id);
            CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name);
            CREATE INDEX IF NOT EXISTS idx_symbols_qname ON symbols(qualified_name);
            CREATE INDEX IF NOT EXISTS idx_symbols_scope ON symbols(scope_id);

            CREATE TABLE IF NOT EXISTS imports (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id),
                source TEXT NOT NULL,
                imported_name TEXT NOT NULL,
                local_name TEXT NOT NULL,
                resolved_file_id INTEGER REFERENCES files(id),
                confidence REAL NOT NULL DEFAULT 0.0
            );
            CREATE INDEX IF NOT EXISTS idx_imports_file ON imports(file_id);
            CREATE INDEX IF NOT EXISTS idx_imports_source ON imports(source);

            CREATE TABLE IF NOT EXISTS exports (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id),
                exported_name TEXT NOT NULL,
                symbol_id INTEGER NOT NULL REFERENCES symbols(id),
                is_default INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_exports_file ON exports(file_id);
            CREATE INDEX IF NOT EXISTS idx_exports_name ON exports(exported_name);

            CREATE TABLE IF NOT EXISTS heritage (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id),
                child_symbol_id INTEGER NOT NULL REFERENCES symbols(id),
                parent_symbol_id INTEGER REFERENCES symbols(id),
                parent_name TEXT NOT NULL,
                heritage_kind TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.0,
                line INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_heritage_child ON heritage(child_symbol_id);
            CREATE INDEX IF NOT EXISTS idx_heritage_parent ON heritage(parent_symbol_id);

            CREATE TABLE IF NOT EXISTS calls (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id),
                caller_scope_id INTEGER REFERENCES scopes(id),
                callee_name TEXT NOT NULL,
                receiver TEXT,
                resolved_symbol_id INTEGER REFERENCES symbols(id),
                confidence REAL NOT NULL DEFAULT 0.0,
                line INTEGER NOT NULL,
                col INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_calls_file ON calls(file_id);
            CREATE INDEX IF NOT EXISTS idx_calls_callee ON calls(callee_name);
            CREATE INDEX IF NOT EXISTS idx_calls_resolved ON calls(resolved_symbol_id);

            CREATE TABLE IF NOT EXISTS edges (
                id INTEGER PRIMARY KEY,
                src_id INTEGER NOT NULL,
                dst_id INTEGER NOT NULL,
                edge_kind TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.0,
                file_id INTEGER REFERENCES files(id),
                line INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_edges_src ON edges(src_id);
            CREATE INDEX IF NOT EXISTS idx_edges_dst ON edges(dst_id);
            CREATE INDEX IF NOT EXISTS idx_edges_kind ON edges(edge_kind);

            -- ── Abstraction layer: file-level graph ──────────────────────────
            -- Stores pre-computed file→file edges for fast large-repo navigation.
            CREATE TABLE IF NOT EXISTS file_graph_edges (
                id INTEGER PRIMARY KEY,
                src_file_id INTEGER NOT NULL REFERENCES files(id),
                dst_file_id INTEGER NOT NULL REFERENCES files(id),
                edge_kind TEXT NOT NULL DEFAULT 'CALLS',
                weight INTEGER NOT NULL DEFAULT 1,
                UNIQUE(src_file_id, dst_file_id, edge_kind)
            );
            CREATE INDEX IF NOT EXISTS idx_file_graph_src ON file_graph_edges(src_file_id);
            CREATE INDEX IF NOT EXISTS idx_file_graph_dst ON file_graph_edges(dst_file_id);

            -- ── Abstraction layer: module-level graph ────────────────────────
            -- Stores pre-computed module/package→package edges.
            CREATE TABLE IF NOT EXISTS module_graph_edges (
                id INTEGER PRIMARY KEY,
                src_module TEXT NOT NULL,
                dst_module TEXT NOT NULL,
                edge_kind TEXT NOT NULL DEFAULT 'CALLS',
                weight INTEGER NOT NULL DEFAULT 1,
                UNIQUE(src_module, dst_module, edge_kind)
            );
            CREATE INDEX IF NOT EXISTS idx_module_graph_src ON module_graph_edges(src_module);
            CREATE INDEX IF NOT EXISTS idx_module_graph_dst ON module_graph_edges(dst_module);

            -- ── Graph metadata ──────────────────────────────────────────────
            -- Stores repo-size classification and recommended view settings.
            CREATE TABLE IF NOT EXISTS graph_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
        ")?;
        Ok(())
    }

    /// Add abstraction layer tables (migration v1 -> v2).
    fn init_schema_v2(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS file_graph_edges (
                id INTEGER PRIMARY KEY,
                src_file_id INTEGER NOT NULL REFERENCES files(id),
                dst_file_id INTEGER NOT NULL REFERENCES files(id),
                edge_kind TEXT NOT NULL DEFAULT 'CALLS',
                weight INTEGER NOT NULL DEFAULT 1,
                UNIQUE(src_file_id, dst_file_id, edge_kind)
            );
            CREATE INDEX IF NOT EXISTS idx_file_graph_src ON file_graph_edges(src_file_id);
            CREATE INDEX IF NOT EXISTS idx_file_graph_dst ON file_graph_edges(dst_file_id);

            CREATE TABLE IF NOT EXISTS module_graph_edges (
                id INTEGER PRIMARY KEY,
                src_module TEXT NOT NULL,
                dst_module TEXT NOT NULL,
                edge_kind TEXT NOT NULL DEFAULT 'CALLS',
                weight INTEGER NOT NULL DEFAULT 1,
                UNIQUE(src_module, dst_module, edge_kind)
            );
            CREATE INDEX IF NOT EXISTS idx_module_graph_src ON module_graph_edges(src_module);
            CREATE INDEX IF NOT EXISTS idx_module_graph_dst ON module_graph_edges(dst_module);

            CREATE TABLE IF NOT EXISTS graph_metadata (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
        ")?;
        Ok(())
    }

    /// Add evidence storage tables (migration v3 -> v4).
    fn init_schema_v4(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS evidence (
                id TEXT PRIMARY KEY,
                kind TEXT NOT NULL,
                file TEXT NOT NULL,
                start_line INTEGER NOT NULL,
                start_col INTEGER NOT NULL,
                end_line INTEGER NOT NULL,
                end_col INTEGER NOT NULL,
                language TEXT NOT NULL DEFAULT '',
                target_type TEXT NOT NULL DEFAULT 'symbol',
                target_ref TEXT NOT NULL DEFAULT '',
                raw TEXT NOT NULL DEFAULT '',
                normalized TEXT NOT NULL DEFAULT '',
                enclosing_symbol TEXT,
                imports TEXT NOT NULL DEFAULT '[]',
                scope_chain TEXT NOT NULL DEFAULT '[]',
                extractor TEXT NOT NULL DEFAULT '',
                confidence REAL NOT NULL DEFAULT 0.0,
                stability REAL NOT NULL DEFAULT 1.0,
                entropy REAL NOT NULL DEFAULT 0.0,
                timestamp_ms INTEGER NOT NULL DEFAULT 0,
                git_commit TEXT,
                state TEXT NOT NULL DEFAULT 'EXTRACTED',
                tags TEXT NOT NULL DEFAULT '[]',
                created_at INTEGER NOT NULL DEFAULT (strftime('%s','now') * 1000),
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s','now') * 1000)
            );
            CREATE INDEX IF NOT EXISTS idx_evidence_kind ON evidence(kind);
            CREATE INDEX IF NOT EXISTS idx_evidence_file ON evidence(file);
            CREATE INDEX IF NOT EXISTS idx_evidence_state ON evidence(state);
            CREATE INDEX IF NOT EXISTS idx_evidence_confidence ON evidence(confidence);

            -- FTS5 virtual table for full-text search over evidence content.
            -- Shadow table: fts5 manages its own storage; VIRTUAL TABLE is the query interface.
            CREATE VIRTUAL TABLE IF NOT EXISTS evidence_fts USING fts5(
                kind,
                raw,
                normalized,
                file,
                language,
                target_ref,
                tags
            );

            CREATE TABLE IF NOT EXISTS evidence_edges (
                from_id TEXT NOT NULL,
                to_id TEXT NOT NULL,
                edge_type TEXT NOT NULL,
                FOREIGN KEY (from_id) REFERENCES evidence(id),
                FOREIGN KEY (to_id) REFERENCES evidence(id),
                PRIMARY KEY (from_id, to_id, edge_type)
            );
            CREATE INDEX IF NOT EXISTS idx_ev_edges_from ON evidence_edges(from_id);
            CREATE INDEX IF NOT EXISTS idx_ev_edges_to ON evidence_edges(to_id);

        ")?;
        Ok(())
    }

    /// Add pattern and constraint tables (migration v4 -> v5).
    fn init_schema_v5(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS patterns (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT NOT NULL,
                motif TEXT NOT NULL,
                frequency INTEGER NOT NULL DEFAULT 0,
                dispersion REAL NOT NULL DEFAULT 0.0,
                stability REAL NOT NULL DEFAULT 1.0,
                entropy REAL NOT NULL DEFAULT 0.0,
                overall_score REAL NOT NULL DEFAULT 0.0,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s','now') * 1000)
            );

            CREATE TABLE IF NOT EXISTS pattern_evidence (
                pattern_id TEXT NOT NULL,
                evidence_id TEXT NOT NULL,
                PRIMARY KEY (pattern_id, evidence_id),
                FOREIGN KEY (pattern_id) REFERENCES patterns(id),
                FOREIGN KEY (evidence_id) REFERENCES evidence(id)
            );
            CREATE INDEX IF NOT EXISTS idx_pattern_evidence_pid ON pattern_evidence(pattern_id);

            CREATE TABLE IF NOT EXISTS constraints (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                description TEXT NOT NULL,
                kind TEXT NOT NULL,
                confidence REAL NOT NULL DEFAULT 0.7,
                active INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL DEFAULT (strftime('%s','now') * 1000)
            );

            CREATE TABLE IF NOT EXISTS constraint_violations (
                constraint_id TEXT NOT NULL,
                evidence_id TEXT NOT NULL,
                detected_at INTEGER NOT NULL DEFAULT (strftime('%s','now') * 1000),
                PRIMARY KEY (constraint_id, evidence_id),
                FOREIGN KEY (constraint_id) REFERENCES constraints(id),
                FOREIGN KEY (evidence_id) REFERENCES evidence(id)
            );
            CREATE INDEX IF NOT EXISTS idx_cv_cid ON constraint_violations(constraint_id);
        ")?;
        Ok(())
    }

    /// Add routes table (migration v5 -> v6).
    fn init_schema_v6(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS routes (
                id INTEGER PRIMARY KEY,
                method TEXT NOT NULL,
                path TEXT NOT NULL,
                file_path TEXT NOT NULL,
                framework TEXT NOT NULL,
                line INTEGER NOT NULL,
                handler_symbol_id INTEGER REFERENCES symbols(id)
            );
            CREATE INDEX IF NOT EXISTS idx_routes_file ON routes(file_path);
            CREATE INDEX IF NOT EXISTS idx_routes_path ON routes(path);
        ")?;
        Ok(())
    }

    /// Add missing indexes for performance (migration v6 -> v7).
    fn init_schema_v7(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("
            CREATE INDEX IF NOT EXISTS idx_symbols_owner ON symbols(owner_symbol_id);
            CREATE INDEX IF NOT EXISTS idx_edges_src_kind ON edges(src_id, edge_kind);
            CREATE INDEX IF NOT EXISTS idx_edges_dst_kind ON edges(dst_id, edge_kind);
            CREATE INDEX IF NOT EXISTS idx_exports_symbol ON exports(symbol_id);
            CREATE INDEX IF NOT EXISTS idx_heritage_parent ON heritage(parent_name);
            CREATE INDEX IF NOT EXISTS idx_calls_scope ON calls(caller_scope_id);
            CREATE INDEX IF NOT EXISTS idx_evidence_target ON evidence(target_ref);
            CREATE INDEX IF NOT EXISTS idx_evidence_enclosing ON evidence(enclosing_symbol);
        ")?;
        Ok(())
    }

    /// Add community persistence tables (migration v2 -> v3).
    fn init_schema_v3(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS communities (
                id INTEGER PRIMARY KEY,
                community_id TEXT NOT NULL UNIQUE,
                label TEXT NOT NULL DEFAULT '',
                cohesion REAL NOT NULL DEFAULT 0.0,
                symbol_count INTEGER NOT NULL DEFAULT 0,
                keywords TEXT NOT NULL DEFAULT '[]',
                modularity REAL NOT NULL DEFAULT 0.0
            );
            CREATE INDEX IF NOT EXISTS idx_communities_count ON communities(symbol_count DESC);

            CREATE TABLE IF NOT EXISTS community_memberships (
                symbol_id INTEGER NOT NULL REFERENCES symbols(id),
                community_id TEXT NOT NULL,
                PRIMARY KEY (symbol_id, community_id)
            );
            CREATE INDEX IF NOT EXISTS idx_comm_memberships_cid ON community_memberships(community_id);
        ")?;
        Ok(())
    }

    /// Add data flow tracking tables (migration v7 -> v8).
    fn init_schema_v8(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS data_flows (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id),
                src_symbol_id INTEGER NOT NULL REFERENCES symbols(id),
                dst_symbol_id INTEGER NOT NULL REFERENCES symbols(id),
                flow_kind TEXT NOT NULL,  -- 'assignment', 'param_pass', 'return', 'property_read', 'property_write'
                var_name TEXT NOT NULL DEFAULT '',
                line INTEGER NOT NULL,
                col INTEGER NOT NULL,
                confidence REAL NOT NULL DEFAULT 1.0
            );
            CREATE INDEX IF NOT EXISTS idx_data_flows_src ON data_flows(src_symbol_id);
            CREATE INDEX IF NOT EXISTS idx_data_flows_dst ON data_flows(dst_symbol_id);
            CREATE INDEX IF NOT EXISTS idx_data_flows_file ON data_flows(file_id);
        ")?;
        Ok(())
    }

    /// Add boundary violation tracking (migration v8 -> v9).
    fn init_schema_v9(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("
            CREATE TABLE IF NOT EXISTS boundary_violations (
                id INTEGER PRIMARY KEY,
                rule_name TEXT NOT NULL,
                from_file TEXT NOT NULL,
                to_file TEXT NOT NULL,
                from_layer TEXT NOT NULL,
                to_layer TEXT NOT NULL,
                violation_kind TEXT NOT NULL,  -- 'import' or 'call'
                line INTEGER NOT NULL,
                symbol_name TEXT NOT NULL,
                detected_at INTEGER NOT NULL DEFAULT (strftime('%s','now') * 1000)
            );
            CREATE INDEX IF NOT EXISTS idx_bv_rule ON boundary_violations(rule_name);
            CREATE INDEX IF NOT EXISTS idx_bv_from ON boundary_violations(from_file);
        ")?;
        Ok(())
    }

    // =================================================================
    // Data flow (ACCESSES / data-flow tracking)
    // =================================================================

    /// Insert a data flow record.
    pub fn insert_data_flow(&self, rec: &DataFlowRecord) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO data_flows (file_id, src_symbol_id, dst_symbol_id, flow_kind, var_name, line, col, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                rec.file_id, rec.src_symbol_id, rec.dst_symbol_id,
                &rec.flow_kind, &rec.var_name, rec.line as i64, rec.col as i64,
                rec.confidence,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Batch insert data flow records.
    pub fn insert_data_flows_batch(&self, records: &[DataFlowRecord]) -> rusqlite::Result<usize> {
        let tx = self.begin_transaction()?;
        let mut count = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO data_flows (file_id, src_symbol_id, dst_symbol_id, flow_kind, var_name, line, col, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            )?;
            for rec in records {
                if stmt.execute(params![
                    rec.file_id, rec.src_symbol_id, rec.dst_symbol_id,
                    &rec.flow_kind, &rec.var_name, rec.line as i64, rec.col as i64,
                    rec.confidence,
                ]).is_ok() {
                    count += 1;
                }
            }
        }
        tx.commit()?;
        Ok(count)
    }

    /// Get all data flows for a symbol (as source).
    pub fn get_data_flows_from(&self, symbol_id: i64) -> rusqlite::Result<Vec<DataFlowRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, src_symbol_id, dst_symbol_id, flow_kind, var_name, line, col, confidence
             FROM data_flows WHERE src_symbol_id = ?1"
        )?;
        let rows = stmt.query_map([symbol_id], |row| {
            Ok(DataFlowRecord {
                id: row.get(0)?, file_id: row.get(1)?,
                src_symbol_id: row.get(2)?, dst_symbol_id: row.get(3)?,
                flow_kind: row.get(4)?, var_name: row.get(5)?,
                line: row.get::<_, i64>(6)? as usize,
                col: row.get::<_, i64>(7)? as usize,
                confidence: row.get(8)?,
            })
        })?;
        rows.collect()
    }

    /// Get all data flows for a symbol (as destination).
    pub fn get_data_flows_to(&self, symbol_id: i64) -> rusqlite::Result<Vec<DataFlowRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, src_symbol_id, dst_symbol_id, flow_kind, var_name, line, col, confidence
             FROM data_flows WHERE dst_symbol_id = ?1"
        )?;
        let rows = stmt.query_map([symbol_id], |row| {
            Ok(DataFlowRecord {
                id: row.get(0)?, file_id: row.get(1)?,
                src_symbol_id: row.get(2)?, dst_symbol_id: row.get(3)?,
                flow_kind: row.get(4)?, var_name: row.get(5)?,
                line: row.get::<_, i64>(6)? as usize,
                col: row.get::<_, i64>(7)? as usize,
                confidence: row.get(8)?,
            })
        })?;
        rows.collect()
    }

    /// Trace the full data flow chain from a symbol (forward traversal).
    /// Uses recursive CTE for efficient graph walk.
    pub fn trace_data_flow_forward(&self, symbol_id: i64, max_depth: i64) -> rusqlite::Result<Vec<(i64, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE flow_chain(dst_id, flow_kind, depth) AS (
                 SELECT dst_symbol_id, flow_kind, 1
                 FROM data_flows WHERE src_symbol_id = ?1
                 UNION ALL
                 SELECT df.dst_symbol_id, df.flow_kind, fc.depth + 1
                 FROM data_flows df
                 JOIN flow_chain fc ON df.src_symbol_id = fc.dst_id
                 WHERE fc.depth < ?2
             )
             SELECT dst_id, flow_kind, depth FROM flow_chain ORDER BY depth"
        )?;
        let rows = stmt.query_map(params![symbol_id, max_depth], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })?;
        rows.collect()
    }

    /// Trace the full data flow chain to a symbol (backward traversal).
    /// Uses recursive CTE for efficient graph walk.
    pub fn trace_data_flow_backward(&self, symbol_id: i64, max_depth: i64) -> rusqlite::Result<Vec<(i64, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE flow_chain(src_id, flow_kind, depth) AS (
                 SELECT src_symbol_id, flow_kind, 1
                 FROM data_flows WHERE dst_symbol_id = ?1
                 UNION ALL
                 SELECT df.src_symbol_id, df.flow_kind, fc.depth + 1
                 FROM data_flows df
                 JOIN flow_chain fc ON df.dst_symbol_id = fc.src_id
                 WHERE fc.depth < ?2
             )
             SELECT src_id, flow_kind, depth FROM flow_chain ORDER BY depth"
        )?;
        let rows = stmt.query_map(params![symbol_id, max_depth], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?))
        })?;
        rows.collect()
    }

    /// Get symbols with no incoming call edges and no incoming import edges (dead code candidates).
    pub fn get_dead_code_candidates(&self) -> rusqlite::Result<Vec<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT s.id, s.file_id, s.name, s.qualified_name, s.kind, s.line, s.col,
                    s.is_exported, s.scope_id, s.owner_symbol_id
             FROM symbols s
             WHERE s.kind IN ('Function', 'Method', 'Class', 'Struct', 'Interface', 'Enum', 'Trait')
             AND s.is_exported = 0
             AND s.id NOT IN (SELECT DISTINCT dst_id FROM edges WHERE edge_kind = 'CALLS')
             AND s.id NOT IN (SELECT DISTINCT symbol_id FROM exports)
             ORDER BY s.name"
        )?;
        let rows = stmt.query_map([], Self::map_symbol_row)?;
        rows.collect()
    }

    /// Detect call graph cycles using recursive CTE.
    /// Returns cycles as vectors of symbol IDs forming the cycle.
    pub fn detect_call_cycles(&self, max_depth: i64) -> rusqlite::Result<Vec<(i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE call_chain(src_id, dst_id, depth) AS (
                 SELECT e.src_id, e.dst_id, 1
                 FROM edges e
                 WHERE e.edge_kind = 'CALLS' AND e.src_id != e.dst_id
                 UNION ALL
                 SELECT cc.src_id, e.dst_id, cc.depth + 1
                 FROM call_chain cc
                 JOIN edges e ON e.src_id = cc.dst_id
                 WHERE e.edge_kind = 'CALLS' AND e.src_id != e.dst_id AND cc.depth < ?1
             )
             SELECT DISTINCT src_id, dst_id FROM call_chain WHERE src_id = dst_id"
        )?;
        let rows = stmt.query_map([max_depth], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect()
    }

    /// Find strongly connected components in the call graph (actual cycles with members).
    /// Find pairwise cycles in the call graph: pairs (a, b) where a→b and b→a
    /// can be reached within depth 32 via CALLS edges.
    ///
    /// NOTE: This detects only 2-node cycles. Larger SCCs (e.g., a→b→c→a)
    /// require Tarjan's algorithm, which is not implemented here.
    pub fn detect_pairwise_cycles(&self) -> rusqlite::Result<Vec<Vec<i64>>> {
        // Find cycle-participating pairs via recursive CTE
        let cycle_edges: Vec<(i64, i64)> = {
            let mut stmt = self.conn.prepare(
                "WITH RECURSIVE call_chain(src_id, dst_id, depth) AS (
                     SELECT e.src_id, e.dst_id, 1
                     FROM edges e
                     WHERE e.edge_kind = 'CALLS' AND e.src_id != e.dst_id
                     UNION ALL
                     SELECT cc.src_id, e.dst_id, cc.depth + 1
                     FROM call_chain cc
                     JOIN edges e ON e.src_id = cc.dst_id
                     WHERE e.edge_kind = 'CALLS' AND e.src_id != e.dst_id AND cc.depth < ?1
                 )
                 SELECT DISTINCT LEAST(src_id, dst_id), GREATEST(src_id, dst_id)
                 FROM call_chain WHERE src_id = dst_id"
            )?;
            let rows = stmt.query_map([MAX_CTE_DEPTH], |row| Ok((row.get(0)?, row.get(1)?)))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        // For simplicity, return pairs — full Tarjan's would need application-level algo
        Ok(cycle_edges.into_iter().map(|(a, b)| vec![a, b]).collect())
    }

    /// Add ACCESSES edges (field/property read/write tracking) to the edges table.
    /// reason: 'read' | 'write'
    pub fn insert_access_edge(&self, src_id: i64, dst_id: i64, reason: &str, confidence: f64, file_id: i64, line: i64) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT OR IGNORE INTO edges (src_id, dst_id, edge_kind, confidence, file_id, line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![src_id, dst_id, format!("ACCESSES_{}", reason), confidence, file_id, line],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get the module-level impact: which files are affected if this symbol changes.
    /// Returns (source_file, target_file, edge_kind, weight) tuples.
    pub fn get_module_impact(&self, symbol_id: i64, max_depth: i64) -> rusqlite::Result<Vec<(String, String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE impact(fid, depth) AS (
                 SELECT DISTINCT s2.file_id, 1
                 FROM edges e
                 JOIN symbols s2 ON s2.id = e.src_id OR s2.id = e.dst_id
                 WHERE (e.src_id = ?1 OR e.dst_id = ?1)
                 AND s2.file_id != (SELECT file_id FROM symbols WHERE id = ?1)
                 UNION ALL
                 SELECT DISTINCT s3.file_id, i.depth + 1
                 FROM edges e2
                 JOIN symbols s3 ON s3.id = e2.src_id OR s3.id = e2.dst_id
                 JOIN impact i ON s3.file_id = i.fid
                 WHERE i.depth < ?2
             )
             SELECT f1.path, f2.path, e.edge_kind, COUNT(*) as weight
             FROM edges e
             JOIN symbols s_src ON s_src.id = e.src_id
             JOIN symbols s_dst ON s_dst.id = e.dst_id
             JOIN files f1 ON f1.id = s_src.file_id
             JOIN files f2 ON f2.id = s_dst.file_id
             WHERE s_src.file_id != s_dst.file_id
             AND (s_src.file_id IN (SELECT fid FROM impact) OR s_src.file_id = ?3)
             GROUP BY f1.path, f2.path, e.edge_kind
             ORDER BY weight DESC
             LIMIT 50"
        )?;
        let rows = stmt.query_map(params![symbol_id, max_depth, symbol_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    }

    /// Get per-symbol git blame: maps a symbol's line range to the most recent
    /// commit that touched each line. Returns (line, commit_hash, author, timestamp).
    /// Only works if blame_lines has been populated (via git-blame command or full indexing).
    pub fn get_symbol_blame(&self, symbol_id: i64) -> rusqlite::Result<Vec<(i64, String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT bl.line_number, bl.commit_hash, bl.author_name, bl.last_changed_at
             FROM blame_lines bl
             JOIN symbols s ON s.file_id = bl.file_id
             WHERE s.id = ?1
             AND bl.line_number BETWEEN s.line AND s.line + 100
             ORDER BY bl.line_number"
        )?;
        let rows = stmt.query_map([symbol_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    }

    /// Get the primary author for a symbol (who touched it most recently).
    pub fn get_symbol_primary_author(&self, symbol_id: i64) -> rusqlite::Result<Option<(String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT bl.author_name, bl.author_email, bl.last_changed_at
             FROM blame_lines bl
             JOIN symbols s ON s.file_id = bl.file_id
             WHERE s.id = ?1
             AND bl.line_number = s.line
             ORDER BY bl.last_changed_at DESC
             LIMIT 1"
        )?;
        let mut rows = stmt.query_map([symbol_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.next().transpose()
    }

    /// Get all ACCESSES edges for a symbol (both reads and writes).
    pub fn get_accesses(&self, symbol_id: i64) -> rusqlite::Result<Vec<(i64, String, f64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT src_id, edge_kind, confidence FROM edges
             WHERE dst_id = ?1 AND edge_kind LIKE 'ACCESSES%'"
        )?;
        let rows = stmt.query_map([symbol_id], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, f64>(2)?))
        })?;
        rows.collect()
    }

    // =================================================================
    // Boundary violations
    // =================================================================

    /// Store detected boundary violations.
    pub fn store_boundary_violations(&self, violations: &[crate::architecture::BoundaryViolation]) -> rusqlite::Result<usize> {
        let tx = self.begin_transaction()?;
        let mut count = 0;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO boundary_violations (rule_name, from_file, to_file, from_layer, to_layer, violation_kind, line, symbol_name)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            )?;
            for v in violations {
                if stmt.execute(params![
                    &v.rule_name, &v.from_file, &v.to_file,
                    &v.from_layer, &v.to_layer, &v.violation_kind,
                    v.line as i64, &v.symbol_name,
                ]).is_ok() {
                    count += 1;
                }
            }
        }
        tx.commit()?;
        Ok(count)
    }

    /// Get all boundary violations, optionally filtered by rule or file.
    pub fn get_boundary_violations(&self, rule_filter: Option<&str>, file_filter: Option<&str>) -> rusqlite::Result<Vec<(String, String, String, String, String, String, usize, String)>> {
        let sql = match (rule_filter, file_filter) {
            (Some(_), Some(_)) => "SELECT rule_name, from_file, to_file, from_layer, to_layer, violation_kind, line, symbol_name FROM boundary_violations WHERE rule_name = ?1 AND (from_file = ?2 OR to_file = ?2) ORDER BY from_file, line",
            (Some(_), None) => "SELECT rule_name, from_file, to_file, from_layer, to_layer, violation_kind, line, symbol_name FROM boundary_violations WHERE rule_name = ?1 ORDER BY from_file, line",
            (None, Some(_)) => "SELECT rule_name, from_file, to_file, from_layer, to_layer, violation_kind, line, symbol_name FROM boundary_violations WHERE from_file = ?1 OR to_file = ?1 ORDER BY from_file, line",
            (None, None) => "SELECT rule_name, from_file, to_file, from_layer, to_layer, violation_kind, line, symbol_name FROM boundary_violations ORDER BY from_file, line",
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows: rusqlite::Result<Vec<_>> = match (rule_filter, file_filter) {
            (Some(r), Some(f)) => stmt.query_map(params![r, f], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get::<_, i64>(6)? as usize, row.get(7)?)))?.collect(),
            (Some(r), None) => stmt.query_map(params![r], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get::<_, i64>(6)? as usize, row.get(7)?)))?.collect(),
            (None, Some(f)) => stmt.query_map(params![f], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get::<_, i64>(6)? as usize, row.get(7)?)))?.collect(),
            (None, None) => stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get::<_, i64>(6)? as usize, row.get(7)?)))?.collect(),
        };
        rows
    }

    // =================================================================
    // Community persistence
    // =================================================================

    /// Persist community detection results to the store.
    /// Clears existing memberships first so incremental re-indexing doesn't violate FK constraints.
    pub fn store_communities(&self, result: &crate::community::CommunityDetectionResult) -> rusqlite::Result<usize> {
        // Clear old data to avoid FK violations during re-indexing
        self.conn.execute("DELETE FROM community_memberships", [])?;
        self.conn.execute("DELETE FROM communities", [])?;

        let mut count = 0;
        for community in &result.communities {
            let keywords_json = serde_json::to_string(&community.keywords).unwrap_or_default();
            self.conn.execute(
                "INSERT INTO communities (community_id, label, cohesion, symbol_count, keywords, modularity)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    &community.id,
                    &community.label,
                    community.cohesion,
                    community.symbol_count as i64,
                    keywords_json,
                    result.stats.modularity,
                ],
            )?;
            count += 1;
        }
        // Store memberships
        for (symbol_id, community_id) in &result.memberships {
            self.conn.execute(
                "INSERT INTO community_memberships (symbol_id, community_id) VALUES (?1, ?2)",
                rusqlite::params![symbol_id, community_id],
            )?;
        }
        Ok(count)
    }

    /// Get all communities sorted by size (largest first).
    pub fn get_communities(&self) -> rusqlite::Result<Vec<(String, String, f64, usize, Vec<String>)>> {
        let mut stmt = self.conn.prepare(
            "SELECT community_id, label, cohesion, symbol_count, keywords
             FROM communities ORDER BY symbol_count DESC"
        )?;
        let rows = stmt.query_map([], |row| {
            let keywords_json: String = row.get(4)?;
            let keywords: Vec<String> = serde_json::from_str(&keywords_json).unwrap_or_default();
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, i64>(3)? as usize,
                keywords,
            ))
        })?;
        rows.collect()
    }

    /// Get community membership for a symbol.
    pub fn get_symbol_community(&self, symbol_id: i64) -> rusqlite::Result<Option<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT community_id FROM community_memberships WHERE symbol_id = ?1"
        )?;
        let mut rows = stmt.query_map([symbol_id], |row| row.get(0))?;
        rows.next().transpose()
    }

    /// Get all symbols in a community.
    pub fn get_community_symbols(&self, community_id: &str) -> rusqlite::Result<Vec<i64>> {
        let mut stmt = self.conn.prepare(
            "SELECT symbol_id FROM community_memberships WHERE community_id = ?1"
        )?;
        let rows = stmt.query_map([community_id], |row| row.get(0))?;
        rows.collect()
    }

    /// Get inter-community edges (community→community) with aggregated weights.
    pub fn get_community_graph_edges(&self) -> rusqlite::Result<Vec<(String, String, String, i64)>> {
        let mut stmt = self.conn.prepare("
            SELECT cm1.community_id, cm2.community_id, e.edge_kind, COUNT(*) as weight
            FROM edges e
            JOIN community_memberships cm1 ON cm1.symbol_id = e.src_id
            JOIN community_memberships cm2 ON cm2.symbol_id = e.dst_id
            WHERE cm1.community_id != cm2.community_id
            GROUP BY cm1.community_id, cm2.community_id, e.edge_kind
            ORDER BY weight DESC
        ")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    }

    /// Get enriched cluster metadata including API boundaries, dominant imports, and coupling.
    /// Returns: (community_id, label, cohesion, symbol_count, keywords,
    ///           api_boundary_count, dominant_imports_json, coupling_score, internal_edge_count, external_edge_count)
    pub fn get_community_details(&self, community_id: &str) -> rusqlite::Result<Option<(String, String, f64, usize, Vec<String>, usize, Vec<String>, f64, i64, i64)>> {
        // Get basic community info
        let (comm_id, label, cohesion, symbol_count, keywords) = {
            let mut stmt = self.conn.prepare(
                "SELECT community_id, label, cohesion, symbol_count, keywords FROM communities WHERE community_id = ?1"
            )?;
            let mut rows = stmt.query_map([community_id], |row| {
                let kw_json: String = row.get(4)?;
                let keywords: Vec<String> = serde_json::from_str(&kw_json).unwrap_or_default();
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, f64>(2)?,
                    row.get::<_, i64>(3)? as usize,
                    keywords,
                ))
            })?;
            match rows.next() {
                Some(Ok(r)) => r,
                _ => return Ok(None),
            }
        };

        let member_ids = self.get_community_symbols(community_id)?;
        if member_ids.is_empty() {
            return Ok(Some((community_id.to_string(), label, cohesion, 0, keywords, 0, vec![], 0.0, 0, 0)));
        }

        let member_set: rustc_hash::FxHashSet<i64> = member_ids.iter().copied().collect();

        // API boundary: exported symbols that are called from outside the community
        let mut api_boundary_count = 0usize;
        let mut dominant_imports: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut internal_edges = 0i64;
        let mut external_edges = 0i64;

        for sym_id in &member_ids {
            // Check if symbol is exported and has external callers
            if let Ok(Some(sym)) = self.get_symbol_by_id(*sym_id) {
                if sym.is_exported {
                    if let Ok(edges) = self.get_edges_for_node(*sym_id) {
                        let has_external_caller = edges.iter().any(|e| {
                            let other = if e.src_id == *sym_id { e.dst_id } else { e.src_id };
                            !member_set.contains(&other) && e.edge_kind == "CALLS"
                        });
                        if has_external_caller {
                            api_boundary_count += 1;
                        }
                    }
                }

                // Count imports from this symbol's file
                if let Ok(Some(file_id)) = self.get_file_id_for_symbol(*sym_id) {
                    if let Ok(imports) = self.get_imports_by_file(file_id) {
                        for imp in &imports {
                            *dominant_imports.entry(imp.source.clone()).or_insert(0) += 1;
                        }
                    }
                }
            }

            // Count internal vs external edges
            if let Ok(edges) = self.get_edges_for_node(*sym_id) {
                for e in &edges {
                    let other = if e.src_id == *sym_id { e.dst_id } else { e.src_id };
                    if member_set.contains(&other) {
                        internal_edges += 1;
                    } else {
                        external_edges += 1;
                    }
                }
            }
        }

        // Top imports
        let mut imports_vec: Vec<(String, usize)> = dominant_imports.into_iter().collect();
        imports_vec.sort_by(|a, b| b.1.cmp(&a.1));
        let top_imports: Vec<String> = imports_vec.into_iter().take(5).map(|(s, _)| s).collect();

        // Coupling score: ratio of external edges to total edges
        let total_edges = internal_edges + external_edges;
        let coupling = if total_edges > 0 {
            external_edges as f64 / total_edges as f64
        } else {
            0.0
        };

        Ok(Some((
            comm_id,
            label,
            cohesion,
            symbol_count,
            keywords,
            api_boundary_count,
            top_imports,
            coupling,
            internal_edges / 2, // each internal edge counted twice
            external_edges,
        )))
    }

    // =================================================================
    // Abstraction layers — pre-computed for large-repo navigation
    // =================================================================

    /// Build file-level and module-level abstraction graphs from the raw symbol graph.
    ///
    /// This collapses thousands of symbol-level edges into a manageable number of
    /// file→file and module→module edges. Called once after the pipeline finishes.
    ///
    /// For a 10K-file repo with 200K symbol edges, this typically produces:
    /// - ~5K file→file edges (10x reduction)
    /// - ~200 module→module edges (1000x reduction)
    pub fn build_abstraction_layers(&self) -> rusqlite::Result<()> {
        // ── File-level graph ──────────────────────────────────────────────
        // Aggregate symbol edges into file→file edges, counting weight.
        self.conn.execute_batch("
            DELETE FROM file_graph_edges;
            INSERT INTO file_graph_edges (src_file_id, dst_file_id, edge_kind, weight)
            SELECT s1.file_id, s2.file_id, e.edge_kind, COUNT(*)
            FROM edges e
            JOIN symbols s1 ON s1.id = e.src_id
            JOIN symbols s2 ON s2.id = e.dst_id
            WHERE s1.file_id != s2.file_id
            GROUP BY s1.file_id, s2.file_id, e.edge_kind;
        ")?;

        // ── Module-level graph ────────────────────────────────────────────
        // Derive module from the file path (parent directory of the file).
        // For src/foo/bar.rs, module = "src/foo".
        // SQLite has no REVERSE, so we use a recursive CTE to find the last '/'.
        self.conn.execute_batch("
            DELETE FROM module_graph_edges;
            WITH RECURSIVE
            last_slash(path, pos) AS (
                SELECT path, 0 FROM files
                UNION ALL
                SELECT path, INSTR(SUBSTR(path, pos + 1), '/') + pos
                FROM last_slash
                WHERE INSTR(SUBSTR(path, pos + 1), '/') > 0
            ),
            modules(path, module) AS (
                SELECT path,
                    CASE
                        WHEN pos > 0 THEN SUBSTR(path, 1, pos - 1)
                        ELSE '.'
                    END
                FROM last_slash
                WHERE pos = (SELECT MAX(ls.pos) FROM last_slash ls WHERE ls.path = last_slash.path)
            )
            INSERT INTO module_graph_edges (src_module, dst_module, edge_kind, weight)
            SELECT m1.module, m2.module, e.edge_kind, COUNT(*)
            FROM edges e
            JOIN symbols s1 ON s1.id = e.src_id
            JOIN symbols s2 ON s2.id = e.dst_id
            JOIN files f1 ON f1.id = s1.file_id
            JOIN files f2 ON f2.id = s2.file_id
            JOIN modules m1 ON m1.path = f1.path
            JOIN modules m2 ON m2.path = f2.path
            WHERE m1.module != m2.module
            GROUP BY m1.module, m2.module, e.edge_kind;
        ")?;

        // ── Graph metadata ────────────────────────────────────────────────
        let file_count: i64 = self.conn.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap_or(0);
        let symbol_count: i64 = self.conn.query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0)).unwrap_or(0);
        let edge_count: i64 = self.conn.query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0)).unwrap_or(0);
        let file_edge_count: i64 = self.conn.query_row("SELECT COUNT(*) FROM file_graph_edges", [], |r| r.get(0)).unwrap_or(0);
        let module_edge_count: i64 = self.conn.query_row("SELECT COUNT(*) FROM module_graph_edges", [], |r| r.get(0)).unwrap_or(0);

        let size_class = if file_count < 100 {
            "small"
        } else if file_count < 1000 {
            "medium"
        } else if file_count < 10000 {
            "large"
        } else {
            "xlarge"
        };

        let default_view = match size_class {
            "small" => "full",
            "medium" => "full",
            "large" => "file",
            _ => "module",
        };

        let max_layout_nodes = match size_class {
            "small" => 5000,
            "medium" => 2000,
            "large" => 1000,
            _ => 500,
        };

        let insert_meta = |key: &str, value: &str| -> rusqlite::Result<()> {
            self.conn.execute(
                "INSERT OR REPLACE INTO graph_metadata (key, value) VALUES (?1, ?2)",
                params![key, value],
            )?;
            Ok(())
        };
        insert_meta("size_class", size_class)?;
        insert_meta("default_view", default_view)?;
        insert_meta("max_layout_nodes", &max_layout_nodes.to_string())?;
        insert_meta("file_count", &file_count.to_string())?;
        insert_meta("symbol_count", &symbol_count.to_string())?;
        insert_meta("edge_count", &edge_count.to_string())?;
        insert_meta("file_edge_count", &file_edge_count.to_string())?;
        insert_meta("module_edge_count", &module_edge_count.to_string())?;

        log::info!(
            "Abstraction layers built: {} files, {} symbols, {} edges → {} file-edges, {} module-edges (size_class={}, default_view={})",
            file_count, symbol_count, edge_count, file_edge_count, module_edge_count, size_class, default_view
        );

        Ok(())
    }

    /// Get graph metadata value by key.
    pub fn get_graph_metadata(&self, key: &str) -> rusqlite::Result<Option<String>> {
        let mut stmt = self.conn.prepare("SELECT value FROM graph_metadata WHERE key = ?1")?;
        let mut rows = stmt.query_map([key], |row| row.get(0))?;
        rows.next().transpose()
    }

    /// Get symbol counts by kind (for schema resource).
    pub fn get_symbol_kind_counts(&self) -> rusqlite::Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, COUNT(*) FROM symbols GROUP BY kind ORDER BY COUNT(*) DESC"
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?;
        rows.collect()
    }

    /// Get all graph metadata as a key-value map.
    pub fn get_all_graph_metadata(&self) -> rusqlite::Result<Option<rustc_hash::FxHashMap<String, String>>> {
        let mut stmt = self.conn.prepare("SELECT key, value FROM graph_metadata")?;
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
        let mut map = rustc_hash::FxHashMap::default();
        for row in rows {
            let (k, v) = row?;
            map.insert(k, v);
        }
        if map.is_empty() { Ok(None) } else { Ok(Some(map)) }
    }

    /// Get all file-level graph edges (for file-level view).
    pub fn get_file_graph_edges(&self) -> rusqlite::Result<Vec<(i64, i64, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT src_file_id, dst_file_id, edge_kind, weight FROM file_graph_edges ORDER BY weight DESC"
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?;
        rows.collect()
    }

    /// Get all module-level graph edges (for module-level view).
    pub fn get_module_graph_edges(&self) -> rusqlite::Result<Vec<(String, String, String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT src_module, dst_module, edge_kind, weight FROM module_graph_edges ORDER BY weight DESC"
        )?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?;
        rows.collect()
    }

    /// Get a scoped subgraph: all symbols and edges within N hops of a starting symbol.
    /// Returns (nodes, edges) as JSON-serializable values.
    pub fn get_symbol_neighborhood(&self, symbol_id: i64, max_depth: usize) -> rusqlite::Result<(Vec<SymbolRecord>, Vec<EdgeRecord>)> {
        // Verify the symbol exists; file_id not needed for BFS.
        match self.conn.query_row(
            "SELECT 1 FROM symbols WHERE id = ?1", [symbol_id], |r| r.get::<_, i64>(0)
        ) {
            Ok(_) => {}
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok((vec![], vec![])),
            Err(e) => return Err(e),
        };

        // BFS to find all symbol IDs within N hops.
        // Prepare statements once outside the loop to avoid per-symbol prepare/execute overhead.
        let mut visited = rustc_hash::FxHashSet::default();
        let mut current_level = vec![symbol_id];
        visited.insert(symbol_id);

        // BFS with batched edge lookups: O(depth) queries instead of O(nodes).
        let outgoing_sql = "SELECT src_id, dst_id FROM edges WHERE src_id IN (SELECT value FROM json_each(?1))";
        let incoming_sql = "SELECT src_id, dst_id FROM edges WHERE dst_id IN (SELECT value FROM json_each(?1))";

        for _ in 0..max_depth {
            if current_level.is_empty() { break; }
            let mut next_level = Vec::new();
            // Process in batches to keep query size reasonable
            for chunk in current_level.chunks(500) {
                let json = serde_json::json!(chunk);
                let mut stmt = self.conn.prepare(outgoing_sql)?;
                let rows = stmt.query_map([json.to_string()], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
                for row in rows {
                    let (_src, dst) = row?;
                    if visited.insert(dst) {
                        next_level.push(dst);
                    }
                }
            }
            current_level = next_level;
        }

        // Also get incoming edges (callers) — same batched approach
        let mut current_level = vec![symbol_id];
        // Reset visited for the incoming traversal (we want both directions)
        let mut visited_incoming = rustc_hash::FxHashSet::default();
        visited_incoming.insert(symbol_id);
        for _ in 0..max_depth {
            if current_level.is_empty() { break; }
            let mut next_level = Vec::new();
            for chunk in current_level.chunks(500) {
                let json = serde_json::json!(chunk);
                let mut stmt = self.conn.prepare(incoming_sql)?;
                let rows = stmt.query_map([json.to_string()], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
                for row in rows {
                    let (src, _dst) = row?;
                    if visited_incoming.insert(src) {
                        next_level.push(src);
                    }
                }
            }
            current_level = next_level;
        }
        // Merge incoming visited into the main visited set
        visited.extend(visited_incoming);

        // Fetch all visited symbols
        let all_ids: Vec<i64> = visited.into_iter().collect();
        let mut symbols = Vec::new();
        for chunk in all_ids.chunks(500) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let query = format!(
                "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id FROM symbols WHERE ID IN ({})",
                placeholders
            );
            let mut stmt = self.conn.prepare(&query)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok(SymbolRecord {
                id: row.get(0)?, file_id: row.get(1)?, name: row.get(2)?,
                qualified_name: row.get(3)?, kind: row.get(4)?,
                line: row.get::<_, i64>(5)? as usize, col: row.get::<_, i64>(6)? as usize,
                is_exported: row.get::<_, i64>(7)? != 0, scope_id: row.get(8)?, owner_symbol_id: row.get(9)?,
            }))?;
            for row in rows { symbols.push(row?); }
        }

        // Fetch edges between visited symbols — batch query to avoid N+1.
        let id_set: rustc_hash::FxHashSet<i64> = all_ids.iter().cloned().collect();
        let mut edges = Vec::new();
        if !all_ids.is_empty() {
            let placeholders = all_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("SELECT id, src_id, dst_id, edge_kind, confidence, file_id, line FROM edges WHERE src_id IN ({})", placeholders);
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(all_ids.iter()), |row| Ok(EdgeRecord {
                id: row.get(0)?, src_id: row.get(1)?, dst_id: row.get(2)?,
                edge_kind: row.get(3)?, confidence: row.get(4)?,
                file_id: row.get(5)?, line: row.get::<_, i64>(6)? as usize,
            }))?;
            for row in rows {
                let e = row?;
                if id_set.contains(&e.dst_id) {
                    edges.push(e);
                }
            }
        }

        Ok((symbols, edges))
    }

    /// Get all symbols and edges for a specific file (file-scoped view).
    pub fn get_file_subgraph(&self, file_id: i64) -> rusqlite::Result<(Vec<SymbolRecord>, Vec<EdgeRecord>)> {
        let symbols = self.get_symbols_by_file(file_id)?;

        let symbol_ids: Vec<i64> = symbols.iter().map(|s| s.id).collect();
        let mut edges = Vec::new();

        if !symbol_ids.is_empty() {
            const IN_CHUNK_SIZE: usize = 500;
            for chunk in symbol_ids.chunks(IN_CHUNK_SIZE) {
                let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let sql = format!(
                    "SELECT id, src_id, dst_id, edge_kind, confidence, file_id, line
                     FROM edges WHERE src_id IN ({0}) OR dst_id IN ({0})",
                    placeholders
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok(EdgeRecord {
                    id: row.get(0)?, src_id: row.get(1)?, dst_id: row.get(2)?,
                    edge_kind: row.get(3)?, confidence: row.get(4)?,
                    file_id: row.get(5)?, line: row.get::<_, i64>(6)? as usize,
                }))?;
                for row in rows { edges.push(row?); }
            }
        }

        Ok((symbols, edges))
    }

    // =================================================================
    // Batch insert — all records in a single transaction
    // =================================================================

    /// Insert all parsed-file data in a single SQLite transaction.
    ///
    /// This is the high-performance path for initial indexing. Instead of
    /// auto-committing each INSERT (which is O(fsync) per statement), we
    /// wrap everything in one `BEGIN/COMMIT` block. For a 10K-file repo
    /// this turns ~50K individual transactions into 1.
    ///
    /// Returns the in-memory symbol ID → DB ID mapping so downstream
    /// phases can resolve references without re-querying.
    pub fn insert_all_files_batch(
        &self,
        parsed_files: &[crate::semantic::ParsedFile],
        repo_label: Option<&str>,
    ) -> rusqlite::Result<rustc_hash::FxHashMap<u64, i64>> {
        let _t0 = std::time::Instant::now();
        let tx = self.begin_transaction()?;
        let mut global_symbol_id_map: rustc_hash::FxHashMap<u64, i64> =
            rustc_hash::FxHashMap::default();

        // Pre-allocate prepared statements for the hot loop.
        let mut file_stmt = tx.prepare(
            "INSERT INTO files (path, hash, language, mtime, indexed_at, repo_label)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
                hash = excluded.hash,
                language = excluded.language,
                mtime = excluded.mtime,
                indexed_at = excluded.indexed_at,
                repo_label = excluded.repo_label"
        )?;
        let mut scope_stmt = tx.prepare(
            "INSERT INTO scopes (file_id, parent_id, owner_symbol_id, kind, line_start, line_end)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        )?;
        let mut symbol_stmt = tx.prepare(
            "INSERT INTO symbols (file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
        )?;
        let mut heritage_stmt = tx.prepare(
            "INSERT INTO heritage (file_id, child_symbol_id, parent_symbol_id, parent_name, heritage_kind, confidence, line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
        )?;
        let mut import_stmt = tx.prepare(
            "INSERT INTO imports (file_id, source, imported_name, local_name, resolved_file_id, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        )?;
        let mut call_stmt = tx.prepare(
            "INSERT INTO calls (file_id, caller_scope_id, callee_name, receiver, resolved_symbol_id, confidence, line, col)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        )?;
        let mut export_stmt = tx.prepare(
            "INSERT INTO exports (file_id, exported_name, symbol_id, is_default)
             VALUES (?1, ?2, ?3, ?4)"
        )?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        for file in parsed_files {

            // Insert file record.
            if let Err(e) = file_stmt.execute(params![
                file.path, file.hash as i64,
                format!("{:?}", file.language),
                0i64, now, repo_label,
            ]) {
                tracing::warn!(file = %file.path, error = %e, "Batch file insert failed");
                continue;
            };
            // Get the file rowid. last_insert_rowid() works for INSERT,
            // but for ON CONFLICT DO UPDATE we need to query it.
            // Use the rowid column directly (alias for INTEGER PRIMARY KEY).
            let file_id: i64 = tx.query_row(
                "SELECT rowid FROM files WHERE path = ?1",
                [file.path.as_str()],
                |r| r.get(0),
            ).unwrap_or(0);
            if file_id <= 0 {
                continue;
            }

            // Insert scopes.
            // Build a map from globally unique scope ID → index within this file.
            let scope_id_to_idx: rustc_hash::FxHashMap<u64, usize> = file.scopes
                .iter()
                .enumerate()
                .map(|(idx, s)| (s.id, idx))
                .collect();
            let mut scope_id_map: Vec<i64> = Vec::with_capacity(file.scopes.len());
            for scope in &file.scopes {
                let parent_store_id = scope.parent_id
                    .and_then(|pid| scope_id_map.get(scope_id_to_idx[&pid]).copied());
                match scope_stmt.execute(params![
                    file_id, parent_store_id,
                    scope.owner_symbol_id.map(|v| v as i64),
                    format!("{:?}", scope.kind),
                    scope.line_start as i64, scope.line_end as i64,
                ]) {
                    Ok(_) => scope_id_map.push(tx.last_insert_rowid()),
                    Err(e) => {
                        tracing::warn!(file_id, line = scope.line_start, error = %e, "Batch scope insert failed");
                        scope_id_map.push(0);
                    }
                }
            }

            // Insert symbols.
            for sym in &file.symbols {
                let store_scope_id = sym.scope_id
                    .and_then(|sid| scope_id_map.get(scope_id_to_idx[&sid]).copied());
                let sym_params = rusqlite::params![
                    file_id, &sym.name, &sym.qualified_name,
                    format!("{:?}", sym.kind),
                    sym.line as i64, sym.col as i64,
                    if sym.is_exported { 1i64 } else { 0i64 },
                    store_scope_id,
                    sym.owner_id.map(|v| v as i64),
                ];
                match symbol_stmt.execute(sym_params) {
                    Ok(_) => { global_symbol_id_map.insert(sym.id, tx.last_insert_rowid()); }
                    Err(e) => { tracing::warn!(symbol = %sym.name, error = %e, "Batch symbol insert failed"); }
                }
            }

            // Insert exports — from both explicit Export records and symbols with is_exported=true.
            for exp in &file.exports {
                if let Some(&sym_id) = global_symbol_id_map.get(&exp.symbol_id) {
                    let _ = export_stmt.execute(params![
                        file_id, exp.exported_name, sym_id,
                        if exp.is_default { 1 } else { 0 },
                    ]);
                }
            }
            // Also populate exports from symbols marked is_exported=true.
            for sym in &file.symbols {
                if sym.is_exported {
                    if let Some(&sym_id) = global_symbol_id_map.get(&sym.id) {
                        let _ = export_stmt.execute(params![
                            file_id, &sym.name, sym_id, 0i64,
                        ]);
                    }
                }
            }

            // Insert heritage.
            for her in &file.heritage {
                let child_idx = if !her.class_name.is_empty() {
                    file.symbols.iter().position(|s| s.name == her.class_name)
                } else {
                    file.symbols.iter()
                        .enumerate()
                        .filter(|(_, s)| {
                            matches!(s.kind,
                                crate::lang::CaptureTag::DefinitionClass |
                                crate::lang::CaptureTag::DefinitionStruct |
                                crate::lang::CaptureTag::DefinitionInterface |
                                crate::lang::CaptureTag::DefinitionEnum |
                                crate::lang::CaptureTag::DefinitionTrait)
                        })
                        .filter(|(_, s)| s.line <= her.line)
                        .max_by_key(|(_, s)| s.line)
                        .map(|(idx, _)| idx)
                };
                let child_id = child_idx
                    .and_then(|idx| file.symbols.get(idx).and_then(|s| global_symbol_id_map.get(&s.id).copied()))
                    .unwrap_or(0);
                let parent_id: Option<i64> = if child_id > 0 {
                    let same_file = file.symbols.iter()
                        .find(|s| s.name == her.target_name && s.id as i64 != child_id)
                        .and_then(|s| global_symbol_id_map.get(&s.id).copied());
                    if same_file.is_some() { same_file } else {
                        self.get_symbols_by_name(&her.target_name).ok()
                            .and_then(|syms| syms.first().map(|s| s.id))
                    }
                } else { None };
                heritage_stmt.execute(params![
                    file_id, child_id, parent_id,
                    her.target_name,
                    format!("{:?}", her.heritage_kind),
                    her.confidence.score(),
                    her.line as i64,
                ])?;
            }

            // Insert imports.
            for imp in &file.imports {
                let _ = import_stmt.execute(params![
                    file_id, imp.source, imp.imported_name, imp.local_name,
                    imp.resolved_file_id.map(|v| v as i64),
                    imp.confidence.score(),
                ]);
            }

            // Insert calls.
            for call in &file.calls {
                let caller_store_scope_id = call.caller_scope_id
                    .and_then(|sid| scope_id_map.get(scope_id_to_idx[&sid]).copied());
                let _ = call_stmt.execute(params![
                    file_id, caller_store_scope_id,
                    call.callee_name, call.receiver,
                    call.resolved_symbol_id.map(|v| v as i64),
                    call.confidence.score(),
                    call.line as i64, call.col as i64,
                ]);
            }
        }

        // Drop statements before commit (required by rusqlite).
        drop(file_stmt);
        drop(scope_stmt);
        drop(symbol_stmt);
        drop(heritage_stmt);
        drop(import_stmt);
        drop(call_stmt);
        drop(export_stmt);

        tx.commit()?;

        // Post-commit verification: ensure data was persisted.
        let expected_files = parsed_files.len() as i64;
        let actual_files: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM files", [], |r| r.get(0)
        ).unwrap_or(0);
        log::info!("[batch] committed {} files (total in DB: {})", expected_files, actual_files);

        Ok(global_symbol_id_map)
    }

    /// Insert scope-resolution edges in a single transaction.
    pub fn insert_edges_batch(
        &self,
        edges: &[crate::store::EdgeRecord],
    ) -> rusqlite::Result<usize> {
        let _t0 = std::time::Instant::now();
        let tx = self.begin_transaction()?;
        let mut stmt = tx.prepare(
            "INSERT INTO edges (src_id, dst_id, edge_kind, confidence, file_id, line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        )?;
        let mut count = 0;
        for edge in edges {
            if stmt.execute(params![
                edge.src_id, edge.dst_id, edge.edge_kind,
                edge.confidence, edge.file_id, edge.line as i64,
            ]).is_ok() {
                count += 1;
            }
        }
        drop(stmt);
        tx.commit()?;

        // Post-commit verification.
        let expected = edges.len() as i64;
        if count as i64 != expected {
            log::warn!("[batch] edge insert: {}/{} persisted ({} failed)", count, expected, expected - count as i64);
        }
        Ok(count)
    }

    // =================================================================
    // File operations
    // =================================================================

    /// Insert or update a file record. Returns the file ID.
    pub fn upsert_file(&self, path: &str, hash: u64, language: &str, mtime: i64, repo_label: Option<&str>) -> rusqlite::Result<i64> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        self.conn.execute(
            "INSERT INTO files (path, hash, language, mtime, indexed_at, repo_label)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET
                hash = excluded.hash,
                language = excluded.language,
                mtime = excluded.mtime,
                indexed_at = excluded.indexed_at,
                repo_label = excluded.repo_label",
            params![path, hash as i64, language, mtime, now, repo_label],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get file record by path.
    pub fn get_file(&self, path: &str) -> rusqlite::Result<Option<FileRecord>> {
        self.conn.query_row(
            "SELECT id, path, hash, language, mtime, indexed_at, repo_label FROM files WHERE path = ?1",
            [path],
            |row| Ok(FileRecord {
                id: row.get(0)?,
                path: row.get(1)?,
                hash: row.get::<_, i64>(2)? as u64,
                language: row.get(3)?,
                mtime: row.get(4)?,
                indexed_at: row.get(5)?,
                repo_label: row.get(6)?,
            }),
        ).optional()
    }

    /// Get a file record by its ID.
    pub fn get_file_by_id(&self, file_id: i64) -> rusqlite::Result<Option<FileRecord>> {
        self.conn.query_row(
            "SELECT id, path, hash, language, mtime, indexed_at, repo_label FROM files WHERE id = ?1",
            [file_id],
            |row| Ok(FileRecord {
                id: row.get(0)?,
                path: row.get(1)?,
                hash: row.get::<_, i64>(2)? as u64,
                language: row.get(3)?,
                mtime: row.get(4)?,
                indexed_at: row.get(5)?,
                repo_label: row.get(6)?,
            }),
        ).optional()
    }

    /// Check if a file needs re-parsing (hash changed or not in store).
    /// Returns Some(file_id) if the file is unchanged, None if it needs re-parsing.
    pub fn check_file_unchanged(&self, path: &str, hash: u64) -> rusqlite::Result<Option<i64>> {
        if let Some(file) = self.get_file(path)? {
            if file.hash == hash {
                return Ok(Some(file.id));
            }
        }
        Ok(None)
    }

    /// Get all indexed file paths and their hashes.
    pub fn get_all_file_hashes(&self) -> rusqlite::Result<Vec<(String, u64)>> {
        let mut stmt = self.conn.prepare("SELECT path, hash FROM files")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        rows.collect()
    }

    /// Get all distinct repo labels in the index.
    pub fn get_repos(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT repo_label FROM files WHERE repo_label IS NOT NULL ORDER BY repo_label"
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect()
    }

    /// Get symbol count per repo.
    pub fn get_repo_stats(&self) -> rusqlite::Result<Vec<(String, i64, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT f.repo_label, COUNT(DISTINCT f.id), COUNT(DISTINCT s.id)
             FROM files f
             LEFT JOIN symbols s ON s.file_id = f.id
             WHERE f.repo_label IS NOT NULL
             GROUP BY f.repo_label
             ORDER BY f.repo_label"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
        })?;
        rows.collect()
    }

    /// Remove a file and ALL its associated data (symbols, scopes, imports, calls, edges, heritage).
    /// Used during incremental scanning when a file has changed.
    ///
    /// Deletes edges by symbol ID (not just file_id) because cross-file edges
    /// may reference this file's symbols but carry a different file_id.
    pub fn remove_file_data(&self, path: &str) -> rusqlite::Result<()> {
        if let Some(file) = self.get_file(path)? {
            let tx = self.begin_transaction()?;
            // Delete edges where THIS file's symbols are either source or destination.
            // The per-file file_id filter misses cross-file edges; the symbol-based
            // deletions catch all edges involving this file's symbols.
            tx.execute("DELETE FROM edges WHERE src_id IN (SELECT id FROM symbols WHERE file_id = ?1)", [file.id])?;
            tx.execute("DELETE FROM edges WHERE dst_id IN (SELECT id FROM symbols WHERE file_id = ?1)", [file.id])?;
            tx.execute("DELETE FROM calls WHERE file_id = ?1", [file.id])?;
            tx.execute("DELETE FROM heritage WHERE file_id = ?1", [file.id])?;
            tx.execute("DELETE FROM imports WHERE file_id = ?1", [file.id])?;
            tx.execute("DELETE FROM exports WHERE file_id = ?1", [file.id])?;
            // Delete evidence edges and evidence linked to this file.
            tx.execute("DELETE FROM evidence_edges WHERE from_id IN (SELECT id FROM evidence WHERE file = ?1)", [file.path.as_str()])?;
            tx.execute("DELETE FROM evidence_edges WHERE to_id IN (SELECT id FROM evidence WHERE file = ?1)", [file.path.as_str()])?;
            tx.execute("DELETE FROM evidence WHERE file = ?1", [file.path.as_str()])?;
            // Clean up FTS5 index for this file (evidence_fts is a manually-populated
            // virtual table — deleting from evidence does NOT auto-delete FTS5 rows).
            tx.execute("DELETE FROM evidence_fts WHERE file = ?1", [file.path.as_str()])?;
            // Delete routes for this file.
            tx.execute("DELETE FROM routes WHERE file_path = ?1", [file.path.as_str()])?;
            // Delete community memberships before symbols (FK constraint)
            tx.execute("DELETE FROM community_memberships WHERE symbol_id IN (SELECT id FROM symbols WHERE file_id = ?1)", [file.id])?;
            tx.execute("DELETE FROM symbols WHERE file_id = ?1", [file.id])?;
            tx.execute("DELETE FROM scopes WHERE file_id = ?1", [file.id])?;
            tx.execute("DELETE FROM files WHERE id = ?1", [file.id])?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Get all indexed files.
    pub fn get_all_files(&self) -> rusqlite::Result<Vec<FileRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, hash, language, mtime, indexed_at, repo_label FROM files"
        )?;
        let rows = stmt.query_map([], Self::map_file_row)?;
        rows.collect()
    }

    /// Delete a file and all its associated records.
    pub fn delete_file(&self, file_id: i64) -> rusqlite::Result<()> {
        let tx = self.begin_transaction()?;
        // Delete edges by symbol_id (not file_id) to catch cross-file edges.
        tx.execute("DELETE FROM edges WHERE src_id IN (SELECT id FROM symbols WHERE file_id = ?1) OR dst_id IN (SELECT id FROM symbols WHERE file_id = ?1)", [file_id])?;
        tx.execute("DELETE FROM calls WHERE file_id = ?1", [file_id])?;
        tx.execute("DELETE FROM exports WHERE file_id = ?1", [file_id])?;
        tx.execute("DELETE FROM imports WHERE file_id = ?1", [file_id])?;
        tx.execute("DELETE FROM heritage WHERE file_id = ?1", [file_id])?;
        tx.execute("DELETE FROM community_memberships WHERE symbol_id IN (SELECT id FROM symbols WHERE file_id = ?1)", [file_id])?;
        // Delete evidence edges and evidence linked to this file.
        // evidence_edges uses from_id/to_id (both FK to evidence.id).
        tx.execute("DELETE FROM evidence_edges WHERE from_id IN (SELECT id FROM evidence WHERE file = (SELECT path FROM files WHERE id = ?1))", [file_id])?;
        tx.execute("DELETE FROM evidence_edges WHERE to_id IN (SELECT id FROM evidence WHERE file = (SELECT path FROM files WHERE id = ?1))", [file_id])?;
        tx.execute("DELETE FROM evidence WHERE file = (SELECT path FROM files WHERE id = ?1)", [file_id])?;
        // Clean up FTS5 index (manually-populated virtual table).
        tx.execute("DELETE FROM evidence_fts WHERE file = (SELECT path FROM files WHERE id = ?1)", [file_id])?;
        // Clean up orphaned pattern/constraint evidence links.
        tx.execute("DELETE FROM pattern_evidence WHERE evidence_id NOT IN (SELECT id FROM evidence)", [])?;
        tx.execute("DELETE FROM constraint_violations WHERE evidence_id NOT IN (SELECT id FROM evidence)", [])?;
        // Delete routes by file_path.
        tx.execute("DELETE FROM routes WHERE file_path IN (SELECT path FROM files WHERE id = ?1)", [file_id])?;
        tx.execute("DELETE FROM symbols WHERE file_id = ?1", [file_id])?;
        tx.execute("DELETE FROM scopes WHERE file_id = ?1", [file_id])?;
        tx.execute("DELETE FROM files WHERE id = ?1", [file_id])?;
        tx.commit()
    }

    // =================================================================
    // Symbol operations
    // =================================================================

    /// Insert a symbol and return its ID.
    pub fn insert_symbol(&self, rec: &SymbolRecord) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO symbols (file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                rec.file_id, rec.name, rec.qualified_name, rec.kind,
                rec.line as i64, rec.col as i64,
                if rec.is_exported { 1 } else { 0 },
                rec.scope_id, rec.owner_symbol_id,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Insert a symbol with an explicit ID (for process/community nodes).
    pub fn insert_symbol_with_id(&self, rec: &SymbolRecord) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT OR REPLACE INTO symbols (id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                rec.id, rec.file_id, rec.name, rec.qualified_name, rec.kind,
                rec.line as i64, rec.col as i64,
                if rec.is_exported { 1 } else { 0 },
                rec.scope_id, rec.owner_symbol_id,
            ],
        )?;
        Ok(rec.id)
    }

    /// Get symbols by name (across all files).
    pub fn get_symbols_by_name(&self, name: &str) -> rusqlite::Result<Vec<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
             FROM symbols WHERE name = ?1"
        )?;
        let rows = stmt.query_map([name], |row| Ok(SymbolRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            qualified_name: row.get(3)?,
            kind: row.get(4)?,
            line: row.get::<_, i64>(5)? as usize,
            col: row.get::<_, i64>(6)? as usize,
            is_exported: row.get::<_, i64>(7)? != 0,
            scope_id: row.get(8)?,
            owner_symbol_id: row.get(9)?,
        }))?;
        rows.collect()
    }

    /// Get all symbols, paginated. Use for bulk operations where you need
    /// to control memory usage via the limit parameter.
    pub fn get_all_symbols_paginated(&self, limit: usize) -> rusqlite::Result<Vec<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
             FROM symbols ORDER BY id LIMIT ?1"
        )?;
        let rows = stmt.query_map([limit as i64], |row| Ok(SymbolRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            qualified_name: row.get(3)?,
            kind: row.get(4)?,
            line: row.get::<_, i64>(5)? as usize,
            col: row.get::<_, i64>(6)? as usize,
            is_exported: row.get::<_, i64>(7)? != 0,
            scope_id: row.get(8)?,
            owner_symbol_id: row.get(9)?,
        }))?;
        rows.collect()
    }

    /// Search symbols by name pattern (substring match, case-insensitive).
    /// Returns matching symbols ordered by relevance (exact > prefix > substring).
    pub fn search_symbols(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<SymbolRecord>> {
        // Try FTS5 first for O(log n) indexed search; fall back to LIKE for
        // stores that haven't had the FTS5 index populated.
        if let Some(results) = self.search_symbols_fts(query, limit).ok() {
            if !results.is_empty() { return Ok(results); }
        }
        let pattern = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
             FROM symbols
             WHERE name LIKE ?1 OR qualified_name LIKE ?1
             ORDER BY
               CASE WHEN name = ?2 THEN 0
                    WHEN name LIKE ?3 THEN 1
                    ELSE 2 END,
               name
             LIMIT ?4"
        )?;
        let exact = query.to_string();
        let prefix = format!("{}%", query);
        let rows = stmt.query_map(
            params![pattern, exact, prefix, limit as i64],
            |row| Ok(SymbolRecord {
                id: row.get(0)?,
                file_id: row.get(1)?,
                name: row.get(2)?,
                qualified_name: row.get(3)?,
                kind: row.get(4)?,
                line: row.get::<_, i64>(5)? as usize,
                col: row.get::<_, i64>(6)? as usize,
                is_exported: row.get::<_, i64>(7)? != 0,
                scope_id: row.get(8)?,
                owner_symbol_id: row.get(9)?,
            }),
        )?;
        rows.collect()
    }

    /// Search symbols using the FTS5 index. Returns Ok(None) if the FTS5 table
    /// doesn't exist or the query fails; caller should fall back to LIKE search.
    fn search_symbols_fts(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<SymbolRecord>> {
        // Build FTS5 query: each word gets prefix matching, joined with OR.
        let fts_query = query.split_whitespace()
            .map(|w| {
                let sanitized: String = w.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect();
                if sanitized.is_empty() { String::new() } else { format!("{}*", sanitized) }
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" OR ");
        if fts_query.is_empty() { return Ok(vec![]); }
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
             FROM symbol_search ss
             JOIN symbols s ON s.id = ss.rowid
             WHERE symbol_search MATCH ?1
             ORDER BY rank
             LIMIT ?2"
        )?;
        let rows = stmt.query_map(
            params![fts_query, limit as i64],
            |row| Ok(SymbolRecord {
                id: row.get(0)?,
                file_id: row.get(1)?,
                name: row.get(2)?,
                qualified_name: row.get(3)?,
                kind: row.get(4)?,
                line: row.get::<_, i64>(5)? as usize,
                col: row.get::<_, i64>(6)? as usize,
                is_exported: row.get::<_, i64>(7)? != 0,
                scope_id: row.get(8)?,
                owner_symbol_id: row.get(9)?,
            }),
        )?;
        rows.collect()
    }

    /// Get a single symbol by its ID.
    pub fn get_symbol_by_id(&self, symbol_id: i64) -> rusqlite::Result<Option<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
             FROM symbols WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map([symbol_id], |row| Ok(SymbolRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            qualified_name: row.get(3)?,
            kind: row.get(4)?,
            line: row.get::<_, i64>(5)? as usize,
            col: row.get::<_, i64>(6)? as usize,
            is_exported: row.get::<_, i64>(7)? != 0,
            scope_id: row.get(8)?,
            owner_symbol_id: row.get(9)?,
        }))?;
        rows.next().transpose()
    }

    /// Batch-load symbol records by IDs. Avoids N+1 queries when rendering graph layouts.
    pub fn get_symbols_by_ids(&self, ids: &[i64]) -> rusqlite::Result<Vec<SymbolRecord>> {
        if ids.is_empty() { return Ok(Vec::new()); }
        const IN_CHUNK_SIZE: usize = 500;
        let mut all_symbols = Vec::new();
        for chunk in ids.chunks(IN_CHUNK_SIZE) {
            let placeholders: String = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
                 FROM symbols WHERE id IN ({})",
                placeholders
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok(SymbolRecord {
                id: row.get(0)?, file_id: row.get(1)?, name: row.get(2)?,
                qualified_name: row.get(3)?, kind: row.get(4)?,
                line: row.get::<_, i64>(5)? as usize, col: row.get::<_, i64>(6)? as usize,
                is_exported: row.get::<_, i64>(7)? != 0, scope_id: row.get(8)?, owner_symbol_id: row.get(9)?,
            }))?;
            for row in rows { all_symbols.push(row?); }
        }
        Ok(all_symbols)
    }

    /// Batch-load file records by IDs. Avoids N+1 queries when rendering graph layouts.
    pub fn get_files_by_ids(&self, ids: &[i64]) -> rusqlite::Result<Vec<FileRecord>> {
        if ids.is_empty() { return Ok(Vec::new()); }
        const IN_CHUNK_SIZE: usize = 500;
        let mut all_files = Vec::new();
        for chunk in ids.chunks(IN_CHUNK_SIZE) {
            let placeholders: String = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("SELECT id, path, hash, language, mtime, indexed_at, repo_label FROM files WHERE id IN ({})", placeholders);
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| Ok(FileRecord {
                id: row.get(0)?,
                path: row.get(1)?,
                hash: row.get::<_, i64>(2)? as u64,
                language: row.get(3)?,
                mtime: row.get::<_, i64>(4)?,
                indexed_at: row.get::<_, i64>(5)?,
                repo_label: row.get(6)?,
            }))?;
            for row in rows { all_files.push(row?); }
        }
        Ok(all_files)
    }

    /// Batch-load edges for multiple symbol IDs. Returns all edges where either
    /// src_id or dst_id is in the given set. Avoids N+1 queries in graph rendering.
    pub fn get_edges_for_symbols(&self, symbol_ids: &rustc_hash::FxHashSet<i64>) -> rusqlite::Result<Vec<EdgeRecord>> {
        if symbol_ids.is_empty() { return Ok(Vec::new()); }
        let ids: Vec<i64> = symbol_ids.iter().copied().collect();
        const IN_CHUNK_SIZE: usize = 500;
        let mut seen = rustc_hash::FxHashSet::default();
        let mut all_edges = Vec::new();
        for chunk in ids.chunks(IN_CHUNK_SIZE) {
            let placeholders: String = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("SELECT id, src_id, dst_id, edge_kind, confidence, file_id, line FROM edges WHERE src_id IN ({}) OR dst_id IN ({})", placeholders, placeholders);
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter().chain(chunk.iter())), |row| Ok(EdgeRecord {
                id: row.get(0)?,
                src_id: row.get(1)?,
                dst_id: row.get(2)?,
                edge_kind: row.get(3)?,
                confidence: row.get(4)?,
                file_id: row.get(5)?,
                line: row.get::<_, i64>(6)? as usize,
            }))?;
            for row in rows {
                let edge = row?;
                if seen.insert(edge.id) {
                    all_edges.push(edge);
                }
            }
        }
        Ok(all_edges)
    }

    /// Get all symbols across all files.
    ///
    /// **Memory warning:** For large indexes (>100K symbols), prefer
    /// `get_all_symbols_chunked` or `get_symbols_paginated` to bound memory.
    pub fn get_all_symbols(&self) -> rusqlite::Result<Vec<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
             FROM symbols"
        )?;
        let rows = stmt.query_map([], Self::map_symbol_row)?;
        rows.collect()
    }

    /// Stream all symbols in chunks to bound memory usage.
    ///
    /// Calls `chunk_fn` with each chunk of up to `chunk_size` symbols.
    /// Returns the total symbol count. If `chunk_fn` returns `Err`, streaming stops.
    ///
    /// Use this instead of `get_all_symbols` for large indexes where loading
    /// all symbols into a single `Vec` would exhaust memory.
    pub fn get_all_symbols_chunked<F>(&self, chunk_size: usize, mut chunk_fn: F) -> rusqlite::Result<usize>
    where
        F: FnMut(&[SymbolRecord]) -> rusqlite::Result<()>,
    {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
             FROM symbols"
        )?;
        let mut rows = stmt.query_map([], Self::map_symbol_row)?;
        let mut chunk = Vec::with_capacity(chunk_size);
        let mut total = 0;
        while let Some(row) = rows.next() {
            chunk.push(row?);
            total += 1;
            if chunk.len() >= chunk_size {
                chunk_fn(&chunk)?;
                chunk.clear();
            }
        }
        if !chunk.is_empty() {
            chunk_fn(&chunk)?;
        }
        Ok(total)
    }

    /// Get all files in chunks to bound memory usage.
    pub fn get_all_files_chunked<F>(&self, chunk_size: usize, mut chunk_fn: F) -> rusqlite::Result<usize>
    where
        F: FnMut(&[FileRecord]) -> rusqlite::Result<()>,
    {
        let mut stmt = self.conn.prepare(
            "SELECT id, path, hash, language, mtime, indexed_at, repo_label FROM files"
        )?;
        let mut rows = stmt.query_map([], Self::map_file_row)?;
        let mut chunk = Vec::with_capacity(chunk_size);
        let mut total = 0;
        while let Some(row) = rows.next() {
            chunk.push(row?);
            total += 1;
            if chunk.len() >= chunk_size {
                chunk_fn(&chunk)?;
                chunk.clear();
            }
        }
        if !chunk.is_empty() {
            chunk_fn(&chunk)?;
        }
        Ok(total)
    }

    #[inline]
    fn map_symbol_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SymbolRecord> {
        Ok(SymbolRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            qualified_name: row.get(3)?,
            kind: row.get(4)?,
            line: row.get::<_, i64>(5)? as usize,
            col: row.get::<_, i64>(6)? as usize,
            is_exported: row.get::<_, i64>(7)? != 0,
            scope_id: row.get(8)?,
            owner_symbol_id: row.get(9)?,
        })
    }

    #[inline]
    fn map_file_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
        Ok(FileRecord {
            id: row.get(0)?,
            path: row.get(1)?,
            hash: row.get::<_, i64>(2)? as u64,
            language: row.get(3)?,
            mtime: row.get(4)?,
            indexed_at: row.get(5)?,
            repo_label: row.get(6)?,
        })
    }

    /// Get symbols with pagination (for large indexes). Returns (symbols, has_more).
    pub fn get_symbols_paginated(&self, offset: usize, limit: usize) -> rusqlite::Result<(Vec<SymbolRecord>, bool)> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
             FROM symbols ORDER BY id LIMIT ?1 OFFSET ?2"
        )?;
        let limit_plus_one = (limit + 1) as i64;
        let rows = stmt.query_map(params![limit_plus_one, offset as i64], |row| Ok(SymbolRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            qualified_name: row.get(3)?,
            kind: row.get(4)?,
            line: row.get::<_, i64>(5)? as usize,
            col: row.get::<_, i64>(6)? as usize,
            is_exported: row.get::<_, i64>(7)? != 0,
            scope_id: row.get(8)?,
            owner_symbol_id: row.get(9)?,
        }))?;
        let all: Vec<SymbolRecord> = rows.collect::<Result<Vec<_>, _>>()?;
        let has_more = all.len() > limit;
        let symbols = all.into_iter().take(limit).collect();
        Ok((symbols, has_more))
    }

    /// Get all symbols for a file.
    pub fn get_symbols_by_file(&self, file_id: i64) -> rusqlite::Result<Vec<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
             FROM symbols WHERE file_id = ?1"
        )?;
        let rows = stmt.query_map([file_id], |row| Ok(SymbolRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            name: row.get(2)?,
            qualified_name: row.get(3)?,
            kind: row.get(4)?,
            line: row.get::<_, i64>(5)? as usize,
            col: row.get::<_, i64>(6)? as usize,
            is_exported: row.get::<_, i64>(7)? != 0,
            scope_id: row.get(8)?,
            owner_symbol_id: row.get(9)?,
        }))?;
        rows.collect()
    }

    /// Count total symbols.
    pub fn count_symbols(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
    }

    /// Count indexed files.
    pub fn count_files(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("SELECT COUNT(*) FROM files WHERE path != '__process_placeholder__'", [], |row| row.get(0))
    }

    // =================================================================
    // Scope operations
    // =================================================================

    pub fn insert_scope(&self, rec: &ScopeRecord) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO scopes (file_id, parent_id, owner_symbol_id, kind, line_start, line_end)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                rec.file_id, rec.parent_id, rec.owner_symbol_id,
                rec.kind, rec.line_start as i64, rec.line_end as i64,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_scopes_by_file(&self, file_id: i64) -> rusqlite::Result<Vec<ScopeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, parent_id, owner_symbol_id, kind, line_start, line_end
             FROM scopes WHERE file_id = ?1 ORDER BY line_start"
        )?;
        let rows = stmt.query_map([file_id], |row| Ok(ScopeRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            parent_id: row.get(2)?,
            owner_symbol_id: row.get(3)?,
            kind: row.get(4)?,
            line_start: row.get::<_, i64>(5)? as usize,
            line_end: row.get::<_, i64>(6)? as usize,
        }))?;
        rows.collect()
    }

    // =================================================================
    // Import operations
    // =================================================================

    pub fn insert_import(&self, rec: &ImportRecord) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO imports (file_id, source, imported_name, local_name, resolved_file_id, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                rec.file_id, rec.source, rec.imported_name, rec.local_name,
                rec.resolved_file_id, rec.confidence,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_imports_by_file(&self, file_id: i64) -> rusqlite::Result<Vec<ImportRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, source, imported_name, local_name, resolved_file_id, confidence
             FROM imports WHERE file_id = ?1"
        )?;
        let rows = stmt.query_map([file_id], |row| Ok(ImportRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            source: row.get(2)?,
            imported_name: row.get(3)?,
            local_name: row.get(4)?,
            resolved_file_id: row.get(5)?,
            confidence: row.get(6)?,
        }))?;
        rows.collect()
    }
    pub fn update_import_resolution(&self, import_id: i64, resolved_file_id: Option<i64>, confidence: f64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE imports SET resolved_file_id = ?1, confidence = ?2 WHERE id = ?3",
            params![resolved_file_id, confidence, import_id],
        )?;
        Ok(())
    }

    // =================================================================
    // Heritage operations
    // =================================================================

    pub fn insert_heritage(&self, rec: &HeritageRecord) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO heritage (file_id, child_symbol_id, parent_symbol_id, parent_name, heritage_kind, confidence, line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                rec.file_id, rec.child_symbol_id, rec.parent_symbol_id,
                rec.parent_name, rec.heritage_kind, rec.confidence,
                rec.line as i64,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update heritage records with resolved parent symbol IDs from scope-resolution.
    /// Matches on (child_symbol_id, parent_name) to set parent_symbol_id.
    pub fn update_heritage_parent(
        &self,
        child_symbol_id: i64,
        parent_name: &str,
        parent_symbol_id: i64,
        confidence: f64,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE heritage SET parent_symbol_id = ?1, confidence = ?2
             WHERE child_symbol_id = ?3 AND parent_name = ?4 AND parent_symbol_id IS NULL",
            params![parent_symbol_id, confidence, child_symbol_id, parent_name],
        )?;
        Ok(())
    }

    pub fn get_heritage_by_child(&self, child_symbol_id: i64) -> rusqlite::Result<Vec<HeritageRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, child_symbol_id, parent_symbol_id, parent_name, heritage_kind, confidence, line
             FROM heritage WHERE child_symbol_id = ?1"
        )?;
        let rows = stmt.query_map([child_symbol_id], |row| Ok(HeritageRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            child_symbol_id: row.get(2)?,
            parent_symbol_id: row.get(3)?,
            parent_name: row.get(4)?,
            heritage_kind: row.get(5)?,
            confidence: row.get(6)?,
            line: row.get::<_, i64>(7)? as usize,
        }))?;
        rows.collect()
    }

    pub fn get_all_heritage(&self) -> rusqlite::Result<Vec<HeritageRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, child_symbol_id, parent_symbol_id, parent_name, heritage_kind, confidence, line
             FROM heritage"
        )?;
        let rows = stmt.query_map([], |row| Ok(HeritageRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            child_symbol_id: row.get(2)?,
            parent_symbol_id: row.get(3)?,
            parent_name: row.get(4)?,
            heritage_kind: row.get(5)?,
            confidence: row.get(6)?,
            line: row.get::<_, i64>(7)? as usize,
        }))?;
        rows.collect()
    }

    // =================================================================
    // Call operations=================================================================
    // Call operations
    // =================================================================

    pub fn insert_call(&self, rec: &CallRecord) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO calls (file_id, caller_scope_id, callee_name, receiver, resolved_symbol_id, confidence, line, col)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                rec.file_id, rec.caller_scope_id, rec.callee_name, rec.receiver,
                rec.resolved_symbol_id, rec.confidence,
                rec.line as i64, rec.col as i64,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn update_call_resolution(&self, call_id: i64, resolved_symbol_id: Option<i64>, confidence: f64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE calls SET resolved_symbol_id = ?1, confidence = ?2 WHERE id = ?3",
            params![resolved_symbol_id, confidence, call_id],
        )?;
        Ok(())
    }

    /// Resolve all unresolved calls matching a callee_name to a specific symbol.
    /// Returns the number of calls updated.
    pub fn resolve_calls_by_name(&self, callee_name: &str, symbol_id: i64) -> rusqlite::Result<usize> {
        // First verify the symbol exists and get its info
        let sym: Option<SymbolRecord> = self.conn.query_row(
            "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
             FROM symbols WHERE id = ?1",
            [symbol_id],
            |row| Ok(SymbolRecord {
                id: row.get(0)?, file_id: row.get(1)?, name: row.get(2)?,
                qualified_name: row.get(3)?, kind: row.get(4)?,
                line: row.get::<_, i64>(5)? as usize, col: row.get::<_, i64>(6)? as usize,
                is_exported: row.get::<_, i64>(7)? != 0, scope_id: row.get(8)?,
                owner_symbol_id: row.get(9)?,
            }),
        ).ok();

        let Some(sym) = sym else { return Ok(0); };

        // Find all unresolved calls with matching callee_name (same file or cross-file)
        let call_ids: Vec<i64> = {
            let mut stmt = self.conn.prepare(
                "SELECT id FROM calls WHERE callee_name = ?1 AND resolved_symbol_id IS NULL"
            )?;
            let rows = stmt.query_map([callee_name], |row| row.get::<_, i64>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        let mut updated = 0;
        let conf = if sym.is_exported { 1.0 } else { 0.5 };
        for call_id in &call_ids {
            self.conn.execute(
                "UPDATE calls SET resolved_symbol_id = ?1, confidence = ?2 WHERE id = ?3",
                params![symbol_id, conf, call_id],
            )?;
            updated += 1;
        }

        Ok(updated)
    }

    pub fn get_calls_by_file(&self, file_id: i64) -> rusqlite::Result<Vec<CallRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, caller_scope_id, callee_name, receiver, resolved_symbol_id, confidence, line, col
             FROM calls WHERE file_id = ?1"
        )?;
        let rows = stmt.query_map([file_id], |row| Ok(CallRecord {
            id: row.get(0)?,
            file_id: row.get(1)?,
            caller_scope_id: row.get(2)?,
            callee_name: row.get(3)?,
            receiver: row.get(4)?,
            resolved_symbol_id: row.get(5)?,
            confidence: row.get(6)?,
            line: row.get::<_, i64>(7)? as usize,
            col: row.get::<_, i64>(8)? as usize,
        }))?;
        rows.collect()
    }

    pub fn count_calls(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("SELECT COUNT(*) FROM calls", [], |row| row.get(0))
    }

    // =================================================================
    // Edge operations
    // =================================================================

    pub fn insert_edge(&self, rec: &EdgeRecord) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO edges (src_id, dst_id, edge_kind, confidence, file_id, line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                rec.src_id, rec.dst_id, rec.edge_kind, rec.confidence,
                rec.file_id, rec.line as i64,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Get the file_id for a given symbol ID.
    pub fn get_file_id_for_symbol(&self, symbol_id: i64) -> rusqlite::Result<Option<i64>> {
        let mut stmt = self.conn.prepare("SELECT file_id FROM symbols WHERE id = ?1")?;
        let result: rusqlite::Result<i64> = stmt.query_row([symbol_id], |row| row.get(0));
        match result {
            Ok(id) => Ok(Some(id)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Get all edges for a node (incoming + outgoing).
    pub fn get_edges_for_node(&self, node_id: i64) -> rusqlite::Result<Vec<EdgeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, src_id, dst_id, edge_kind, confidence, file_id, line
             FROM edges WHERE src_id = ?1 OR dst_id = ?1"
        )?;
        let rows = stmt.query_map([node_id], |row| Ok(EdgeRecord {
            id: row.get(0)?,
            src_id: row.get(1)?,
            dst_id: row.get(2)?,
            edge_kind: row.get(3)?,
            confidence: row.get(4)?,
            file_id: row.get(5)?,
            line: row.get::<_, i64>(6)? as usize,
        }))?;
        rows.collect()
    }

    /// Get edges for multiple nodes in a single query. O(1) queries instead of O(n).
    pub fn get_edges_for_nodes(&self, node_ids: &[i64]) -> rusqlite::Result<Vec<EdgeRecord>> {
        if node_ids.is_empty() {
            return Ok(Vec::new());
        }
        // Chunk IN clauses to avoid excessive SQLite variable counts.
        // Deduplicate by edge ID since OR queries may return the same edge
        // from overlapping chunk boundaries (e.g., edge (500,501) matches
        // both chunk [1..500] via src_id and chunk [501..1000] via dst_id).
        const IN_CHUNK_SIZE: usize = 500;
        let mut seen = rustc_hash::FxHashSet::default();
        let mut all_edges = Vec::new();
        for chunk in node_ids.chunks(IN_CHUNK_SIZE) {
            let placeholders: Vec<String> = chunk.iter().map(|_| "?".to_string()).collect();
            let sql = format!(
                "SELECT id, src_id, dst_id, edge_kind, confidence, file_id, line
                 FROM edges WHERE src_id IN ({}) OR dst_id IN ({})",
                placeholders.join(","),
                placeholders.join(","),
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(chunk.iter().copied().chain(chunk.iter().copied())), |row| {
                Ok(EdgeRecord {
                    id: row.get(0)?,
                    src_id: row.get(1)?,
                    dst_id: row.get(2)?,
                    edge_kind: row.get(3)?,
                    confidence: row.get(4)?,
                    file_id: row.get(5)?,
                    line: row.get::<_, i64>(6)? as usize,
                })
            })?;
            for row in rows {
                let edge = row?;
                if seen.insert(edge.id) {
                    all_edges.push(edge);
                }
            }
        }
        Ok(all_edges)
    }

    /// Get all edges in the graph.
    ///
    /// **Memory warning:** For large indexes (>1M edges), prefer
    /// `get_all_edges_chunked` to bound memory.
    pub fn get_all_edges(&self) -> rusqlite::Result<Vec<EdgeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, src_id, dst_id, edge_kind, confidence, file_id, line FROM edges"
        )?;
        let rows = stmt.query_map([], Self::map_edge_row)?;
        rows.collect()
    }

    /// Stream all edges in chunks to bound memory usage.
    pub fn get_all_edges_chunked<F>(&self, chunk_size: usize, mut chunk_fn: F) -> rusqlite::Result<usize>
    where
        F: FnMut(&[EdgeRecord]) -> rusqlite::Result<()>,
    {
        let mut stmt = self.conn.prepare(
            "SELECT id, src_id, dst_id, edge_kind, confidence, file_id, line FROM edges"
        )?;
        let mut rows = stmt.query_map([], Self::map_edge_row)?;
        let mut chunk = Vec::with_capacity(chunk_size);
        let mut total = 0;
        while let Some(row) = rows.next() {
            chunk.push(row?);
            total += 1;
            if chunk.len() >= chunk_size {
                chunk_fn(&chunk)?;
                chunk.clear();
            }
        }
        if !chunk.is_empty() {
            chunk_fn(&chunk)?;
        }
        Ok(total)
    }

    #[inline]
    fn map_edge_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EdgeRecord> {
        Ok(EdgeRecord {
            id: row.get(0)?,
            src_id: row.get(1)?,
            dst_id: row.get(2)?,
            edge_kind: row.get(3)?,
            confidence: row.get(4)?,
            file_id: row.get(5)?,
            line: row.get::<_, i64>(6)? as usize,
        })
    }

    /// Count total edges.
    pub fn count_edges(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
    }

    // =================================================================
    // Graph traversal queries (recursive CTEs)
    // =================================================================

    /// Find all callers of a symbol (upstream impact).
    /// Uses recursive CTE to walk the call graph backwards.
    /// `max_depth` is hard-capped at 10 to prevent pathological queries.
    pub fn get_callers(&self, symbol_id: i64, max_depth: usize) -> rusqlite::Result<Vec<(i64, String, f64, i64)>> {
        let depth = max_depth.min(10);
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE callers(depth, caller_id, caller_name, confidence, sym_file_id) AS (
                SELECT 0, e.src_id, s.name, e.confidence, s.file_id
                FROM edges e
                JOIN symbols s ON s.id = e.src_id
                WHERE e.dst_id = ?1 AND e.edge_kind = 'CALLS'
                UNION ALL
                SELECT callers.depth + 1, e.src_id, s.name, e.confidence, s.file_id
                FROM callers
                JOIN edges e ON e.dst_id = callers.caller_id
                JOIN symbols s ON s.id = e.src_id
                WHERE e.edge_kind = 'CALLS' AND callers.depth < ?2
            )
            SELECT DISTINCT caller_id, caller_name, confidence, sym_file_id FROM callers WHERE depth > 0
            ORDER BY depth
            LIMIT 10000"
        )?;
        let rows = stmt.query_map(params![symbol_id, depth as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    }

    /// Find all callees of a symbol (downstream impact).
    /// `max_depth` is hard-capped at 10 to prevent pathological queries.
    pub fn get_callees(&self, symbol_id: i64, max_depth: usize) -> rusqlite::Result<Vec<(i64, String, f64, i64)>> {
        let depth = max_depth.min(10);
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE callees(depth, callee_id, callee_name, confidence, sym_file_id) AS (
                SELECT 0, e.dst_id, s.name, e.confidence, s.file_id
                FROM edges e
                JOIN symbols s ON s.id = e.dst_id
                WHERE e.src_id = ?1 AND e.edge_kind = 'CALLS'
                UNION ALL
                SELECT callees.depth + 1, e.dst_id, s.name, e.confidence, s.file_id
                FROM callees
                JOIN edges e ON e.src_id = callees.callee_id
                JOIN symbols s ON s.id = e.dst_id
                WHERE e.edge_kind = 'CALLS' AND callees.depth < ?2
            )
            SELECT DISTINCT callee_id, callee_name, confidence, sym_file_id FROM callees WHERE depth > 0
            ORDER BY depth
            LIMIT 10000"
        )?;
        let rows = stmt.query_map(params![symbol_id, depth as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    }

    /// Set incremental indexing stats.
    pub fn set_incremental_stats(&self, _inserted: i64, _reused: i64) {
        // Store in a separate metadata table or just track in memory
        // For now, these are tracked externally and passed through ScanResult
    }
}

// =================================================================
// Record types
// =================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: i64,
    pub path: String,
    pub hash: u64,
    pub language: String,
    pub mtime: i64,
    pub indexed_at: i64,
    pub repo_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRecord {
    pub id: i64,
    pub file_id: i64,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub line: usize,
    pub col: usize,
    pub is_exported: bool,
    pub scope_id: Option<i64>,
    pub owner_symbol_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeritageRecord {
    pub id: i64,
    pub file_id: i64,
    pub child_symbol_id: i64,
    pub parent_symbol_id: Option<i64>,
    pub parent_name: String,
    pub heritage_kind: String,
    pub confidence: f64,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeRecord {
    pub id: i64,
    pub file_id: i64,
    pub parent_id: Option<i64>,
    pub owner_symbol_id: Option<i64>,
    pub kind: String,
    pub line_start: usize,
    pub line_end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportRecord {
    pub id: i64,
    pub file_id: i64,
    pub source: String,
    pub imported_name: String,
    pub local_name: String,
    pub resolved_file_id: Option<i64>,
    pub confidence: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRecord {
    pub id: i64,
    pub file_id: i64,
    pub caller_scope_id: Option<i64>,
    pub callee_name: String,
    pub receiver: Option<String>,
    pub resolved_symbol_id: Option<i64>,
    pub confidence: f64,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EdgeRecord {
    pub id: i64,
    pub src_id: i64,
    pub dst_id: i64,
    pub edge_kind: String,
    pub confidence: f64,
    pub file_id: Option<i64>,
    pub line: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreStats {
    pub files: i64,
    pub symbols: i64,
    pub scopes: i64,
    pub imports: i64,
    pub calls: i64,
    pub edges: i64,
    pub resolved_calls: i64,
    pub files_inserted: i64,
    pub files_reused: i64,
    pub evidence: i64,
    pub db_size_bytes: u64,
}

// =================================================================
// Route persistence
// =================================================================

impl GraphStore {
    /// Persist detected routes to the store, replacing old routes for the given files.
    /// `file_ids_to_replace`: set of file IDs whose routes should be cleared before inserting.
    pub fn persist_routes(
        &self,
        routes: &[(i64, crate::routes::Route)], // (handler_symbol_id, route)
        file_ids_to_replace: &[i64],
    ) -> rusqlite::Result<usize> {
        let tx = self.begin_transaction()?;
        // Clear old routes for the files being re-indexed
        if !file_ids_to_replace.is_empty() {
            let placeholders = file_ids_to_replace.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("DELETE FROM routes WHERE file_path IN (SELECT path FROM files WHERE id IN ({}))", placeholders);
            let params: Vec<&dyn rusqlite::ToSql> = file_ids_to_replace.iter().map(|id| id as &dyn rusqlite::ToSql).collect();
            tx.execute(&sql, params.as_slice())?;
        }
        let mut count = 0;
        let mut stmt = tx.prepare(
            "INSERT INTO routes (method, path, file_path, framework, line, handler_symbol_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
        )?;
        for (handler_db_id, route) in routes {
            stmt.execute(params![
                &route.method,
                &route.path,
                &route.file_path,
                &route.framework,
                route.line as i64,
                *handler_db_id,
            ])?;
            count += 1;
        }
        drop(stmt);
        tx.commit()?;
        Ok(count)
    }

    /// Query all routes, optionally filtered by path substring.
    pub fn get_routes(&self, path_filter: Option<&str>) -> rusqlite::Result<Vec<(String, String, String, i64, String, String)>> {
        let sql = match path_filter {
            Some(_) => "SELECT r.method, r.path, r.file_path, r.line, r.framework, COALESCE(s.name, '') as handler_name
                         FROM routes r LEFT JOIN symbols s ON s.id = r.handler_symbol_id
                         WHERE r.path LIKE ?1
                         ORDER BY r.file_path, r.line",
            None => "SELECT r.method, r.path, r.file_path, r.line, r.framework, COALESCE(s.name, '') as handler_name
                     FROM routes r LEFT JOIN symbols s ON s.id = r.handler_symbol_id
                     ORDER BY r.file_path, r.line",
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows: rusqlite::Result<Vec<(String, String, String, i64, String, String)>> = if let Some(filter) = path_filter {
            let pattern = format!("%{}%", filter);
            let mapped = stmt.query_map([pattern], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?;
            mapped.collect()
        } else {
            let mapped = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?;
            mapped.collect()
        };
        rows
    }

    /// Remove duplicate symbols caused by re-indexing. Keeps the symbol with the
    /// lowest id for each (file_id, name, line) tuple. Also cleans up orphaned
    /// calls, exports, imports, and heritage entries.
    pub fn deduplicate_symbols(&self) -> rusqlite::Result<usize> {
        let deleted: i64 = self.conn.query_row(
            "WITH dups AS (
                SELECT id, ROW_NUMBER() OVER (PARTITION BY file_id, name, line ORDER BY id) as rn
                FROM symbols
            )
            SELECT COUNT(*) FROM dups WHERE rn > 1",
            [],
            |r| r.get(0),
        )?;
        if deleted > 0 {
            self.conn.execute(
                "DELETE FROM symbols WHERE id IN (
                    SELECT id FROM (
                        SELECT id, ROW_NUMBER() OVER (PARTITION BY file_id, name, line ORDER BY id) as rn
                        FROM symbols
                    ) WHERE rn > 1
                )",
                [],
            )?;
            log::info!("Deduplicated {} symbols", deleted);
        }
        Ok(deleted as usize)
    }

    /// Remove duplicate calls caused by re-indexing.
    pub fn deduplicate_calls(&self) -> rusqlite::Result<usize> {
        let deleted: i64 = self.conn.query_row(
            "WITH dups AS (
                SELECT id, ROW_NUMBER() OVER (PARTITION BY file_id, caller_scope_id, callee_name, line ORDER BY id) as rn
                FROM calls
            )
            SELECT COUNT(*) FROM dups WHERE rn > 1",
            [],
            |r| r.get(0),
        )?;
        if deleted > 0 {
            self.conn.execute(
                "DELETE FROM calls WHERE id IN (
                    SELECT id FROM (
                        SELECT id, ROW_NUMBER() OVER (PARTITION BY file_id, caller_scope_id, callee_name, line ORDER BY id) as rn
                        FROM calls
                    ) WHERE rn > 1
                )",
                [],
            )?;
            log::info!("Deduplicated {} calls", deleted);
        }
        Ok(deleted as usize)
    }
}
