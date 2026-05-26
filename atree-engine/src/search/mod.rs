//! BM25 + Hybrid Search Index for code intelligence queries.
//!
//! Provides full-text search over symbols, files, and code graph nodes
//! using SQLite FTS5 (for BM25) combined with graph-aware ranking.
//!
//! Ported from GitNexus's search/bm25-index.ts and search/hybrid-search.ts.

use crate::store::GraphStore;
use rustc_hash::FxHashMap;
use serde::{Serialize, Deserialize};

/// A search result hit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub node_id: i64,
    pub name: String,
    pub kind: String,
    pub file_path: String,
    pub line: usize,
    pub score: f64,
    pub matched_text: String,
}

/// Search configuration.
pub struct SearchConfig {
    /// Max results to return (default: 10)
    pub limit: usize,
    /// BM25 k1 parameter (default: 1.2)
    pub k1: f64,
    /// BM25 b parameter (default: 0.75)
    pub b: f64,
    /// Boost for exact name matches (default: 2.0)
    pub exact_match_boost: f64,
    /// Boost for exported symbols (default: 1.5)
    pub exported_boost: f64,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            limit: 10,
            k1: 1.2,
            b: 0.75,
            exact_match_boost: 2.0,
            exported_boost: 1.5,
        }
    }
}

/// Initialize the FTS5 search index in the graph store.
/// Creates the FTS5 virtual table if it doesn't exist.
pub fn init_search_index(store: &GraphStore) -> rusqlite::Result<()> {
    // FTS5 table for symbol search
    store.conn().execute_batch("
        CREATE VIRTUAL TABLE IF NOT EXISTS symbol_search USING fts5(
            name,
            qualified_name,
            kind,
            file_path,
            content='',
            content_rowid='rowid'
        );
    ")?;
    Ok(())
}

/// Index all symbols from the graph store into the FTS5 search table.
///
/// Uses a single transaction + prepared statement for batch insert. On a 400K
/// symbol graph this is ~100x faster than individual auto-committed INSERTs.
pub fn index_symbols(store: &GraphStore) -> rusqlite::Result<usize> {
    init_search_index(store)?;

    // Clear existing index — FTS5 contentless tables do not support bare
    // DELETE, so we drop and recreate instead.
    store.conn().execute("DROP TABLE IF EXISTS symbol_search", [])?;
    init_search_index(store)?;

    // Pre-build file_id → path map to avoid N+1 queries.
    let files = store.get_all_files()?;
    let file_map: FxHashMap<i64, String> = files
        .into_iter()
        .map(|f| (f.id, f.path))
        .collect();

    // Batch-insert all symbols in a single transaction.
    let tx = store.conn().unchecked_transaction()?;
    let count: usize;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO symbol_search (rowid, name, qualified_name, kind, file_path)
             VALUES (?1, ?2, ?3, ?4, ?5)"
        )?;

        let symbols = store.get_all_symbols()?;
        count = symbols.len();
        for sym in &symbols {
            let file_path = file_map.get(&sym.file_id).map(|s| s.as_str()).unwrap_or("");
            stmt.execute(rusqlite::params![
                sym.id,
                &sym.name,
                &sym.qualified_name,
                &sym.kind,
                file_path,
            ])?;
        }
        // stmt dropped here, releasing borrow on tx
    }
    tx.commit()?;

    Ok(count)
}

/// Remove a file's symbols from the FTS5 search index.
pub fn remove_file_from_index(store: &GraphStore, file_id: i64) -> rusqlite::Result<usize> {
    // Get all symbol IDs for this file
    let symbol_ids: Vec<i64> = {
        let mut stmt = store.conn().prepare("SELECT id FROM symbols WHERE file_id = ?1")?;
        let rows = stmt.query_map([file_id], |row| row.get::<_, i64>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };

    let mut removed = 0;
    for sym_id in &symbol_ids {
        store.conn().execute("DELETE FROM symbol_search WHERE rowid = ?1", [sym_id])?;
        removed += 1;
    }
    Ok(removed)
}

/// Index symbols for a specific file into the FTS5 search table.
pub fn index_file_symbols(store: &GraphStore, file_id: i64) -> rusqlite::Result<usize> {
    let symbols = store.get_symbols_by_file(file_id)?;
    let mut count = 0;

    for sym in &symbols {
        // Get the file path
        let file_path: String = store.conn().query_row(
            "SELECT path FROM files WHERE id = ?1",
            [file_id],
            |row| row.get(0),
        ).unwrap_or_default();

        store.conn().execute(
            "INSERT OR REPLACE INTO symbol_search (rowid, name, qualified_name, kind, file_path)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                sym.id,
                sym.name,
                sym.qualified_name,
                sym.kind,
                file_path,
            ],
        )?;
        count += 1;
    }

    Ok(count)
}

/// Search for symbols matching the query.
/// Uses FTS5 for BM25 ranking combined with graph-aware boosting.
pub fn search(
    store: &GraphStore,
    query: &str,
    config: &SearchConfig,
) -> rusqlite::Result<Vec<SearchHit>> {
    // Use FTS5 query for BM25 ranking
    let fts_query = build_fts_query(query);

    let mut stmt = store.conn().prepare(
        "SELECT s.id, s.name, s.qualified_name, s.kind,
                f.path,
                s.line, s.col, s.is_exported,
                rank
         FROM symbol_search ss
         JOIN symbols s ON s.id = ss.rowid
         JOIN files f ON f.id = s.file_id
         WHERE symbol_search MATCH ?1
         ORDER BY rank
         LIMIT ?2"
    )?;

    let rows = stmt.query_map(
        rusqlite::params![fts_query, config.limit as i64],
        |row| {
            let rank: f64 = row.get(8).unwrap_or(0.0);
            let is_exported: i64 = row.get(7).unwrap_or(0);
            let name: String = row.get(1)?;
            let query_lower = query.to_lowercase();

            // Apply boosting
            let mut score = -rank; // FTS5 rank is negative BM25 score

            // Exact name match boost
            if name.to_lowercase() == query_lower {
                score *= config.exact_match_boost;
            } else if name.to_lowercase().contains(&query_lower) {
                score *= 1.2;
            }

            // Exported symbol boost
            if is_exported != 0 {
                score *= config.exported_boost;
            }

            Ok(SearchHit {
                node_id: row.get(0)?,
                name,
                kind: row.get(3)?,
                file_path: row.get(4)?,
                line: row.get::<_, i64>(5)? as usize,
                score,
                matched_text: row.get(2)?,
            })
        },
    )?;

    let mut hits: Vec<SearchHit> = rows.collect::<Result<Vec<_>, _>>()?;

    // Sort by score (highest first)
    hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    Ok(hits)
}

/// Build an FTS5 query string from a user query.
///
/// Uses OR semantics with prefix matching so that any matching term
/// ranks a result. FTS5's BM25 ranking naturally boosts documents
/// matching more terms, so OR gives us the best of both worlds:
/// - Recall: symbols matching any query term are found
/// - Precision: symbols matching more terms rank higher
///
/// For single-word queries, uses prefix matching (e.g. "auth*" matches
/// "authentication", "authorize", etc.).
fn build_fts_query(query: &str) -> String {
    let words: Vec<&str> = query.split_whitespace().collect();
    if words.is_empty() {
        return query.to_string();
    }

    // All words get prefix matching, joined with OR.
    // FTS5 BM25 ranking will naturally prefer documents matching more terms.
    words.iter()
        .map(|w| {
            // Strip FTS5 special characters that could cause parse errors.
            // FTS5 operators: " ( ) * ^ - NOT AND OR NEAR
            let sanitized: String = w.chars()
                .filter(|c| c.is_alphanumeric() || *c == '_' || *c == ':' || *c == '.')
                .collect();
            if sanitized.is_empty() {
                String::new()
            } else {
                format!("{}*", sanitized)
            }
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Hybrid search: combines BM25 text search with graph proximity scoring.
/// Symbols that are closely connected in the call graph get a proximity boost.
pub fn hybrid_search(
    store: &GraphStore,
    query: &str,
    config: &SearchConfig,
) -> rusqlite::Result<Vec<SearchHit>> {
    let mut text_hits = search(store, query, config)?;

    if text_hits.is_empty() {
        return Ok(text_hits);
    }

    // Calculate graph proximity scores
    let mut proximity_scores: FxHashMap<i64, f64> = FxHashMap::default();

    for hit in &text_hits {
        // Find how many other search results are reachable from this hit
        let reachable = count_reachable_in_set(store, hit.node_id, &text_hits)?;
        if reachable > 0 {
            proximity_scores.insert(hit.node_id, reachable as f64);
        }
    }

    // Apply proximity boost
    let max_proximity = proximity_scores.values().copied().fold(0.0, f64::max);
    if max_proximity > 0.0 {
        for hit in &mut text_hits {
            if let Some(&prox) = proximity_scores.get(&hit.node_id) {
                let normalized = prox / max_proximity;
                hit.score *= 1.0 + normalized * 0.3; // 30% proximity boost max
            }
        }
    }

    // Re-sort
    text_hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    Ok(text_hits)
}

/// Count how many nodes in the hit set are reachable from the given node via CALLS edges.
fn count_reachable_in_set(
    store: &GraphStore,
    node_id: i64,
    hit_set: &[SearchHit],
) -> rusqlite::Result<usize> {
    let hit_ids: rustc_hash::FxHashSet<i64> = hit_set.iter().map(|h| h.node_id).collect();
    let mut visited = rustc_hash::FxHashSet::default();
    let mut count = 0;

    // BFS up to depth 3
    let mut queue: Vec<(i64, usize)> = vec![(node_id, 0)];
    visited.insert(node_id);

    while let Some((current, depth)) = queue.pop() {
        if depth >= 3 {
            continue;
        }

        let edges = store.get_edges_for_node(current)?;
        for edge in &edges {
            if edge.edge_kind == "CALLS" && !visited.contains(&edge.dst_id) {
                visited.insert(edge.dst_id);
                if hit_ids.contains(&edge.dst_id) {
                    count += 1;
                }
                queue.push((edge.dst_id, depth + 1));
            }
        }
    }

    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_finds_symbols() {
        let store = crate::store::GraphStore::open_in_memory().unwrap();
        init_search_index(&store).unwrap();

        let file_id = store.upsert_file("src/auth.rs", 1, "rust", 0, None).unwrap();

        let login_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "login".into(), qualified_name: "auth::login".into(),
            kind: "DefinitionFunction".into(), line: 1, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        let logout_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "logout".into(), qualified_name: "auth::logout".into(),
            kind: "DefinitionFunction".into(), line: 10, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "validate_token".into(), qualified_name: "auth::validate_token".into(),
            kind: "DefinitionFunction".into(), line: 20, col: 0,
            is_exported: false, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        // Index symbols
        let count = index_symbols(&store).unwrap();
        assert_eq!(count, 3);

        // Search for "login"
        let config = SearchConfig::default();
        let results = search(&store, "login", &config).unwrap();
        assert!(!results.is_empty(), "Should find 'login'");
        assert_eq!(results[0].name, "login");

        // Search for "auth" (matches qualified names)
        let results = search(&store, "auth", &config).unwrap();
        assert!(!results.is_empty(), "Should find symbols with 'auth' in qualified name");

        // Search for "logout"
        let results = search(&store, "logout", &config).unwrap();
        assert!(!results.is_empty(), "Should find 'logout'");
    }

    #[test]
    fn test_empty_search() {
        let store = crate::store::GraphStore::open_in_memory().unwrap();
        init_search_index(&store).unwrap();

        let config = SearchConfig::default();
        let results = search(&store, "nonexistent", &config).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_hybrid_search() {
        let store = crate::store::GraphStore::open_in_memory().unwrap();
        init_search_index(&store).unwrap();

        let file_id = store.upsert_file("src/lib.rs", 1, "rust", 0, None).unwrap();

        let a_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "handle_request".into(), qualified_name: "handle_request".into(),
            kind: "DefinitionFunction".into(), line: 1, col: 0,
            is_exported: true, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        let b_id = store.insert_symbol(&crate::store::SymbolRecord {
            id: 0, file_id, name: "parse_body".into(), qualified_name: "parse_body".into(),
            kind: "DefinitionFunction".into(), line: 10, col: 0,
            is_exported: false, scope_id: None, owner_symbol_id: None,
        }).unwrap();

        // a → b
        store.insert_edge(&crate::store::EdgeRecord {
            id: 0, src_id: a_id, dst_id: b_id,
            edge_kind: "CALLS".into(), confidence: 1.0,
            file_id: Some(file_id), line: 0,
        }).unwrap();

        index_symbols(&store).unwrap();

        let config = SearchConfig::default();
        let results = hybrid_search(&store, "handle", &config).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn test_build_fts_query_sanitization() {
        // Normal query
        assert_eq!(build_fts_query("auth login"), "auth* OR login*");

        // Query with FTS5 special characters should be sanitized
        let q = build_fts_query("auth \"login\"");
        assert!(!q.contains('"'), "Should strip double quotes: {}", q);
        assert!(q.contains("auth*"), "Should keep 'auth': {}", q);
        assert!(q.contains("login*"), "Should keep 'login': {}", q);

        // Query with parentheses
        let q = build_fts_query("auth (login)");
        assert!(!q.contains('('), "Should strip parentheses: {}", q);
        assert!(!q.contains(')'), "Should strip parentheses: {}", q);

        // Query with only special chars should produce empty string
        let q = build_fts_query("\"()^*");
        assert!(q.is_empty() || q == " OR  OR  OR ", "Should handle all-special-char query: {}", q);

        // Empty query
        assert_eq!(build_fts_query(""), "");
        // Whitespace-only query returns the original (empty after trim, but returns as-is)
        assert_eq!(build_fts_query("   ").trim(), "");
    }
}
