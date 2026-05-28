//! Evidence Storage — SQLite persistence layer.
//!
//! Tables:
//! - `evidence` — core evidence records
//! - `evidence_edges` — graph links between evidence units
//!
//! Invariants enforced at the storage level:
//! - I4: Only confidence, stability, and contradiction edges may change post-commit.

use crate::evidence::*;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Database-backed evidence store.
pub struct EvidenceStore<'a> {
    conn: &'a Connection,
}

impl<'a> EvidenceStore<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Initialize evidence tables. Called as part of schema migrations.
    pub fn init_tables(&self) -> rusqlite::Result<()> {
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
                target_type TEXT NOT NULL DEFAULT 'Symbol',
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

    /// Insert a batch of evidence in a single transaction.
    /// Skips duplicates (INSERT OR IGNORE).
    pub fn insert_batch(&self, evidence: &[Evidence]) -> rusqlite::Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut stmt = tx.prepare("
            INSERT OR IGNORE INTO evidence
            (id, kind, file, start_line, start_col, end_line, end_col, language,
             target_type, target_ref, raw, normalized, enclosing_symbol, imports, scope_chain,
             extractor, confidence, stability, entropy, timestamp_ms, git_commit, state, tags)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                    ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23)
        ")?;
        let mut count = 0;
        let mut fts_stmt = tx.prepare(
            "INSERT INTO evidence_fts (kind, raw, normalized, file, language, target_ref, tags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
        )?;
        for ev in evidence {
            let target_type = match ev.target.target_type {
                TargetType::Primitive => "primitive",
                TargetType::Symbol => "symbol",
                TargetType::Pattern => "pattern",
                TargetType::Constraint => "constraint",
            };
            let imports = serde_json::to_string(&ev.context.imports).unwrap_or_default();
            let scope_chain = serde_json::to_string(&ev.context.scope_chain).unwrap_or_default();
            let tags = serde_json::to_string(&ev.tags).unwrap_or_default();

            let result = stmt.execute(params![
                ev.id.0,
                ev.kind.to_string(),
                ev.source.file,
                ev.source.span.start_line as i64,
                ev.source.span.start_col as i64,
                ev.source.span.end_line as i64,
                ev.source.span.end_col as i64,
                ev.source.language,
                target_type,
                ev.target.ref_id,
                ev.content.raw,
                ev.content.normalized,
                ev.context.enclosing_symbol.as_deref().unwrap_or(""),
                imports,
                scope_chain,
                ev.metadata.extractor,
                ev.metadata.confidence,
                ev.metadata.stability,
                ev.metadata.entropy,
                ev.metadata.timestamp_ms,
                ev.metadata.commit.as_deref().unwrap_or(""),
                ev.state.to_string(),
                tags,
            ]);
            if result.is_ok() {
                count += 1;
                // Also index in FTS5 for full-text search.
                let _ = fts_stmt.execute(params![
                    ev.kind.to_string(),
                    ev.content.raw,
                    ev.content.normalized,
                    ev.source.file,
                    ev.source.language,
                    ev.target.ref_id,
                    serde_json::to_string(&ev.tags).unwrap_or_default(),
                ]);
            }
        }
        drop(stmt);
        drop(fts_stmt);
        tx.commit()?;
        Ok(count)
    }

    /// Get evidence by ID.
    pub fn get(&self, id: &EvidenceId) -> rusqlite::Result<Option<EvidenceRecord>> {
        self.conn.query_row(
            "SELECT id, kind, file, start_line, start_col, end_line, end_col, language,
                    target_type, target_ref, raw, normalized, enclosing_symbol, imports, scope_chain,
                    extractor, confidence, stability, entropy, timestamp_ms, git_commit, state, tags
             FROM evidence WHERE id = ?1",
            [&id.0],
            |row| {
                Ok(EvidenceRecord {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    file: row.get(2)?,
                    start_line: row.get::<_, i64>(3)? as usize,
                    start_col: row.get::<_, i64>(4)? as usize,
                    end_line: row.get::<_, i64>(5)? as usize,
                    end_col: row.get::<_, i64>(6)? as usize,
                    language: row.get(7)?,
                    target_type: row.get(8)?,
                    target_ref: row.get(9)?,
                    raw: row.get(10)?,
                    normalized: row.get(11)?,
                    enclosing_symbol: row.get::<_, Option<String>>(12)?,
                    imports: row.get(13)?,
                    scope_chain: row.get(14)?,
                    extractor: row.get(15)?,
                    confidence: row.get(16)?,
                    stability: row.get(17)?,
                    entropy: row.get(18)?,
                    timestamp_ms: row.get(19)?,
                    commit: row.get::<_, Option<String>>(20)?,
                    state: row.get(21)?,
                    tags: row.get(22)?,
                })
            },
        ).optional()
    }

    /// Update confidence and stability (I4: only these fields may change post-commit).
    pub fn update_confidence(
        &self,
        id: &EvidenceId,
        confidence: f64,
        stability: f64,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE evidence SET confidence = ?1, stability = ?2,
             updated_at = strftime('%s','now') * 1000, state = 'UPDATED'
             WHERE id = ?3",
            params![confidence, stability, id.0],
        )?;
        Ok(())
    }

    /// Add a contradiction edge (the only structural change allowed post-commit).
    pub fn add_contradiction(&self, from_id: &EvidenceId, to_id: &EvidenceId) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO evidence_edges (from_id, to_id, edge_type) VALUES (?1, ?2, 'contradicts')",
            params![from_id.0, to_id.0],
        )?;
        Ok(())
    }

    /// Get evidence edges for a given evidence ID.
    pub fn get_edges(&self, id: &EvidenceId) -> rusqlite::Result<Vec<EvidenceEdgeRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT from_id, to_id, edge_type FROM evidence_edges WHERE from_id = ?1 OR to_id = ?1"
        )?;
        let rows = stmt.query_map([id.0.as_str()], |row| {
            Ok(EvidenceEdgeRecord {
                from_id: row.get(0)?,
                to_id: row.get(1)?,
                edge_type: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    /// Query evidence by kind.
    pub fn by_kind(&self, kind: EvidenceKind) -> rusqlite::Result<Vec<EvidenceRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, file, start_line, start_col, end_line, end_col, language,
                    target_type, target_ref, raw, normalized, enclosing_symbol, imports, scope_chain,
                    extractor, confidence, stability, entropy, timestamp_ms, git_commit, state, tags
             FROM evidence WHERE kind = ?1 ORDER BY confidence DESC"
        )?;
        let rows = stmt.query_map([kind.to_string()], |row| {
            Ok(EvidenceRecord {
                id: row.get(0)?,
                kind: row.get(1)?,
                file: row.get(2)?,
                start_line: row.get::<_, i64>(3)? as usize,
                start_col: row.get::<_, i64>(4)? as usize,
                end_line: row.get::<_, i64>(5)? as usize,
                end_col: row.get::<_, i64>(6)? as usize,
                language: row.get(7)?,
                target_type: row.get(8)?,
                target_ref: row.get(9)?,
                raw: row.get(10)?,
                normalized: row.get(11)?,
                enclosing_symbol: row.get::<_, Option<String>>(12)?,
                imports: row.get(13)?,
                scope_chain: row.get(14)?,
                extractor: row.get(15)?,
                confidence: row.get(16)?,
                stability: row.get(17)?,
                entropy: row.get(18)?,
                timestamp_ms: row.get(19)?,
                commit: row.get::<_, Option<String>>(20)?,
                state: row.get(21)?,
                tags: row.get(22)?,
            })
        })?;
        rows.collect()
    }

    /// Query evidence by file.
    pub fn by_file(&self, file: &str) -> rusqlite::Result<Vec<EvidenceRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, file, start_line, start_col, end_line, end_col, language,
                    target_type, target_ref, raw, normalized, enclosing_symbol, imports, scope_chain,
                    extractor, confidence, stability, entropy, timestamp_ms, git_commit, state, tags
             FROM evidence WHERE file = ?1 ORDER BY start_line"
        )?;
        let rows = stmt.query_map([file], |row| {
            Ok(EvidenceRecord {
                id: row.get(0)?,
                kind: row.get(1)?,
                file: row.get(2)?,
                start_line: row.get::<_, i64>(3)? as usize,
                start_col: row.get::<_, i64>(4)? as usize,
                end_line: row.get::<_, i64>(5)? as usize,
                end_col: row.get::<_, i64>(6)? as usize,
                language: row.get(7)?,
                target_type: row.get(8)?,
                target_ref: row.get(9)?,
                raw: row.get(10)?,
                normalized: row.get(11)?,
                enclosing_symbol: row.get::<_, Option<String>>(12)?,
                imports: row.get(13)?,
                scope_chain: row.get(14)?,
                extractor: row.get(15)?,
                confidence: row.get(16)?,
                stability: row.get(17)?,
                entropy: row.get(18)?,
                timestamp_ms: row.get(19)?,
                commit: row.get::<_, Option<String>>(20)?,
                state: row.get(21)?,
                tags: row.get(22)?,
            })
        })?;
        rows.collect()
    }

    /// Count evidence by state.
    pub fn count_by_state(&self) -> rusqlite::Result<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(
            "SELECT state, COUNT(*) FROM evidence GROUP BY state"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect()
    }

    /// Total evidence count.
    pub fn count(&self) -> rusqlite::Result<i64> {
        self.conn.query_row("SELECT COUNT(*) FROM evidence", [], |r| r.get(0))
    }

    /// Full-text search over evidence content (FTS5).
    /// Returns (file, kind, raw, rank) for matching evidence. Full records require
    /// a separate lookup by file + kind + raw from the evidence table.
    pub fn search(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<EvidenceFtsResult>> {
        let mut stmt = self.conn.prepare(
            "SELECT kind, raw, normalized, file, language, target_ref, tags, rank
             FROM evidence_fts
             WHERE evidence_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2"
        )?;
        let rows = stmt.query_map(params![query, limit as i64], |row| {
            Ok(EvidenceFtsResult {
                kind: row.get(0)?,
                raw: row.get(1)?,
                normalized: row.get(2)?,
                file: row.get(3)?,
                language: row.get(4)?,
                target_ref: row.get(5)?,
                tags: row.get(6)?,
                rank: row.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// Remove evidence FTS5 entries for a given file (used during incremental re-index).
    pub fn remove_file_from_fts(&self, file: &str) -> rusqlite::Result<usize> {
        // FTS5 doesn't support DELETE with WHERE on non-FTS columns efficiently.
        // Rebuild the FTS5 table excluding the deleted file's entries.
        self.conn.execute("DELETE FROM evidence_fts WHERE file = ?1", [file])
    }
}

// ── Flat row type for DB I/O ─────────────────────────────────────────────────

/// Flat representation for database operations (avoids storing full `Evidence` graph links).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceRecord {
    pub id: String,
    pub kind: String,
    pub file: String,
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
    pub language: String,
    pub target_type: String,
    pub target_ref: String,
    pub raw: String,
    pub normalized: String,
    pub enclosing_symbol: Option<String>,
    pub imports: String,
    pub scope_chain: String,
    pub extractor: String,
    pub confidence: f64,
    pub stability: f64,
    pub entropy: f64,
    pub timestamp_ms: i64,
    pub commit: Option<String>,
    pub state: String,
    pub tags: String,
}

/// Lightweight result from FTS5 search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceFtsResult {
    pub kind: String,
    pub raw: String,
    pub normalized: String,
    pub file: String,
    pub language: String,
    pub target_ref: String,
    pub tags: String,
    pub rank: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceEdgeRecord {
    pub from_id: String,
    pub to_id: String,
    pub edge_type: String,
}

/// Convert an `Evidence` to an `EvidenceRecord` for storage.
impl From<&Evidence> for EvidenceRecord {
    fn from(ev: &Evidence) -> Self {
        Self {
            id: ev.id.0.clone(),
            kind: ev.kind.to_string(),
            file: ev.source.file.clone(),
            start_line: ev.source.span.start_line,
            start_col: ev.source.span.start_col,
            end_line: ev.source.span.end_line,
            end_col: ev.source.span.end_col,
            language: ev.source.language.clone(),
            target_type: match ev.target.target_type {
                TargetType::Primitive => "primitive",
                TargetType::Symbol => "symbol",
                TargetType::Pattern => "pattern",
                TargetType::Constraint => "constraint",
            }.to_string(),
            target_ref: ev.target.ref_id.clone(),
            raw: ev.content.raw.clone(),
            normalized: ev.content.normalized.clone(),
            enclosing_symbol: ev.context.enclosing_symbol.clone(),
            imports: serde_json::to_string(&ev.context.imports).unwrap_or_default(),
            scope_chain: serde_json::to_string(&ev.context.scope_chain).unwrap_or_default(),
            extractor: ev.metadata.extractor.clone(),
            confidence: ev.metadata.confidence,
            stability: ev.metadata.stability,
            entropy: ev.metadata.entropy,
            timestamp_ms: ev.metadata.timestamp_ms,
            commit: ev.metadata.commit.clone(),
            state: ev.state.to_string(),
            tags: serde_json::to_string(&ev.tags).unwrap_or_default(),
        }
    }
}
