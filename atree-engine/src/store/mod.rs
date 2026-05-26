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
use serde::{Serialize, Deserialize};

/// Maximum time to wait for the advisory file lock (seconds).
const LOCK_TIMEOUT_SECS: u64 = 10;

/// Current schema version. Bump when adding migrations.
const SCHEMA_VERSION: i32 = 1;

/// Persistent graph store backed by SQLite.
pub struct GraphStore {
    conn: Connection,
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
        ")?;
        Ok(())
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
        let tx = self.conn.unchecked_transaction()?;
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
                eprintln!("[batch] file insert failed for {}: {}", file.path, e);
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
                        eprintln!("[batch] scope insert failed at line {} (file_id={}): {}", scope.line_start, file_id, e);
                        scope_id_map.push(0);
                    }
                }
            }

            // Insert symbols.
            for sym in &file.symbols {
                let store_scope_id = sym.scope_id
                    .and_then(|sid| scope_id_map.get(scope_id_to_idx[&sid]).copied());
                match symbol_stmt.execute(params![
                    file_id, sym.name, sym.qualified_name,
                    format!("{:?}", sym.kind),
                    sym.line as i64, sym.col as i64,
                    if sym.is_exported { 1 } else { 0 },
                    store_scope_id,
                    sym.owner_id.map(|v| v as i64),
                ]) {
                    Ok(_) => { global_symbol_id_map.insert(sym.id, tx.last_insert_rowid()); }
                    Err(e) => { eprintln!("[batch] symbol insert failed for {}: {}", sym.name, e); }
                }
            }

            // Insert heritage.
            for her in &file.heritage {
                let child_id = file.symbols.iter()
                    .position(|s| s.name == her.class_name || her.class_name.is_empty())
                    .and_then(|idx| file.symbols.get(idx).and_then(|s| global_symbol_id_map.get(&s.id).copied()))
                    .unwrap_or(0);
                let _ = heritage_stmt.execute(params![
                    file_id, child_id, 0i64,
                    her.target_name,
                    format!("{:?}", her.heritage_kind),
                    her.confidence.score(),
                    her.line as i64,
                ]);
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
        let tx = self.conn.unchecked_transaction()?;
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
    pub fn remove_file_data(&self, path: &str) -> rusqlite::Result<()> {
        if let Some(file) = self.get_file(path)? {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute("DELETE FROM edges WHERE file_id = ?1", [file.id])?;
            tx.execute("DELETE FROM calls WHERE file_id = ?1", [file.id])?;
            tx.execute("DELETE FROM heritage WHERE file_id = ?1", [file.id])?;
            tx.execute("DELETE FROM imports WHERE file_id = ?1", [file.id])?;
            tx.execute("DELETE FROM exports WHERE file_id = ?1", [file.id])?;
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
        let rows = stmt.query_map([], |row| Ok(FileRecord {
            id: row.get(0)?,
            path: row.get(1)?,
            hash: row.get::<_, i64>(2)? as u64,
            language: row.get(3)?,
            mtime: row.get(4)?,
            indexed_at: row.get(5)?,
            repo_label: row.get(6)?,
        }))?;
        rows.collect()
    }

    /// Delete a file and all its associated records.
    pub fn delete_file(&self, file_id: i64) -> rusqlite::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM edges WHERE file_id = ?1", [file_id])?;
        tx.execute("DELETE FROM calls WHERE file_id = ?1", [file_id])?;
        tx.execute("DELETE FROM exports WHERE file_id = ?1", [file_id])?;
        tx.execute("DELETE FROM imports WHERE file_id = ?1", [file_id])?;
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

    /// Get all symbols across all files.
    pub fn get_all_symbols(&self) -> rusqlite::Result<Vec<SymbolRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, file_id, name, qualified_name, kind, line, col, is_exported, scope_id, owner_symbol_id
             FROM symbols"
        )?;
        let rows = stmt.query_map([], |row| Ok(SymbolRecord {
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

    /// Get all edges in the graph.
    pub fn get_all_edges(&self) -> rusqlite::Result<Vec<EdgeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, src_id, dst_id, edge_kind, confidence, file_id, line FROM edges"
        )?;
        let rows = stmt.query_map([], |row| Ok(EdgeRecord {
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

    /// Count total edges.
    pub fn count_edges(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))
    }

    // =================================================================
    // Graph traversal queries (recursive CTEs)
    // =================================================================

    /// Find all callers of a symbol (upstream impact).
    /// Uses recursive CTE to walk the call graph backwards.
    pub fn get_callers(&self, symbol_id: i64, max_depth: usize) -> rusqlite::Result<Vec<(i64, String, f64, i64)>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE callers(depth, caller_id, caller_name, confidence, file_id) AS (
                SELECT 0, s.id, s.name, c.confidence, c.file_id
                FROM calls c
                JOIN symbols s ON s.id = c.resolved_symbol_id
                WHERE c.resolved_symbol_id = ?1
                UNION ALL
                SELECT callers.depth + 1, s.id, s.name, c.confidence, c.file_id
                FROM callers
                JOIN calls c ON c.resolved_symbol_id = callers.caller_id
                JOIN symbols s ON s.id = c.resolved_symbol_id
                WHERE callers.depth < ?2
            )
            SELECT caller_id, caller_name, confidence, file_id FROM callers WHERE depth > 0
            ORDER BY depth"
        )?;
        let rows = stmt.query_map(params![symbol_id, max_depth as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    }

    /// Find all callees of a symbol (downstream impact).
    pub fn get_callees(&self, symbol_id: i64, max_depth: usize) -> rusqlite::Result<Vec<(i64, String, f64, i64)>> {
        let mut stmt = self.conn.prepare(
            "WITH RECURSIVE callees(depth, callee_id, callee_name, confidence, file_id) AS (
                SELECT 0, c.resolved_symbol_id, s.name, c.confidence, c.file_id
                FROM calls c
                JOIN symbols s ON s.id = c.resolved_symbol_id
                WHERE c.file_id IN (SELECT file_id FROM symbols WHERE id = ?1)
                AND c.resolved_symbol_id IS NOT NULL
                UNION ALL
                SELECT callees.depth + 1, c.resolved_symbol_id, s.name, c.confidence, c.file_id
                FROM callees
                JOIN symbols sym ON sym.id = callees.callee_id
                JOIN calls c ON c.file_id IN (SELECT file_id FROM symbols WHERE id = callees.callee_id)
                JOIN symbols s ON s.id = c.resolved_symbol_id
                WHERE callees.depth < ?2
                AND c.resolved_symbol_id IS NOT NULL
            )
            SELECT callee_id, callee_name, confidence, file_id FROM callees WHERE depth > 0
            ORDER BY depth"
        )?;
        let rows = stmt.query_map(params![symbol_id, max_depth as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    }

    // =================================================================
    // Stats
    // =================================================================

    pub fn stats(&self) -> rusqlite::Result<StoreStats> {
        let files: i64 = self.conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
        let symbols: i64 = self.conn.query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))?;
        let scopes: i64 = self.conn.query_row("SELECT COUNT(*) FROM scopes", [], |row| row.get(0))?;
        let imports: i64 = self.conn.query_row("SELECT COUNT(*) FROM imports", [], |row| row.get(0))?;
        let calls: i64 = self.conn.query_row("SELECT COUNT(*) FROM calls", [], |row| row.get(0))?;
        let edges: i64 = self.conn.query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))?;
        let resolved_calls: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM edges WHERE edge_kind = 'CALLS'", [], |row| row.get(0)
        )?;
        Ok(StoreStats { files, symbols, scopes, imports, calls, edges, resolved_calls, files_inserted: 0, files_reused: 0 })
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
}
