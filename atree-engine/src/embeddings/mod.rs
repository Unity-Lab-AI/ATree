//! Embedding generation and semantic vector search.
//!
//! Uses [fastembed](https://docs.rs/fastembed) to generate sentence-transformer
//! embeddings locally via ONNX Runtime. No external API calls needed.
//!
//! The default model is `BGE-small-en-v1.5` (384 dimensions, fast, good quality).
//! Embeddings are stored in a SQLite table and searched via brute-force cosine
//! similarity (exact KNN). For large indexes, consider adding an HNSW index.

use crate::store::GraphStore;
use serde::{Serialize, Deserialize};
use std::sync::OnceLock;

/// Embedding vector dimension for BGE-small-en-v1.5.
pub const EMBEDDING_DIM: usize = 384;

/// A symbol embedding record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolEmbedding {
    pub symbol_id: i64,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file_path: String,
    pub line: usize,
    /// The embedding vector (flattened, row-major).
    pub vector: Vec<f32>,
}

/// A semantic search hit with cosine similarity score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSearchHit {
    pub symbol_id: i64,
    pub name: String,
    pub qualified_name: String,
    pub kind: String,
    pub file_path: String,
    pub line: usize,
    /// Cosine similarity score (higher is better, range [-1, 1]).
    pub similarity: f32,
}

/// Initialize the embeddings table in the graph store.
pub fn init_embeddings_table(store: &GraphStore) -> rusqlite::Result<()> {
    store.conn().execute_batch("
        CREATE TABLE IF NOT EXISTS embeddings (
            symbol_id INTEGER PRIMARY KEY REFERENCES symbols(id),
            vector BLOB NOT NULL,
            updated_at INTEGER NOT NULL
        );
    ")?;
    Ok(())
}

/// Get the global embedder instance (lazy initialization).
///
/// Uses BGE-small-en-v1.5 which is fast, small (~90MB), and produces
/// good quality 384-dimensional embeddings for code symbols.
fn get_embedder() -> &'static Result<fastembed::TextEmbedding, String> {
    static EMBEDDER: OnceLock<Result<fastembed::TextEmbedding, String>> = OnceLock::new();
    EMBEDDER.get_or_init(|| {
        let model = fastembed::EmbeddingModel::BGESmallENV15;
        let opts = fastembed::InitOptions::new(model)
            .with_show_download_progress(true);
        fastembed::TextEmbedding::try_new(opts)
            .map_err(|e| format!("Failed to initialize embedder: {}", e))
    })
}

/// Generate an embedding for a single text string.
pub fn embed_text(text: &str) -> Result<Vec<f32>, String> {
    let embedder = get_embedder().as_ref().map_err(|e| e.clone())?;
    let embeddings = embedder.embed(vec![text.to_string()], None)
        .map_err(|e| format!("Embedding failed: {}", e))?;
    if embeddings.is_empty() {
        return Err("No embeddings generated".to_string());
    }
    Ok(embeddings[0].clone())
}

/// Generate an embedding for a symbol using its name, kind, and qualified name.
fn symbol_to_text(name: &str, kind: &str, qualified_name: &str) -> String {
    // Create a rich text representation for better semantic matching
    format!("{} {} {}", kind, qualified_name, name)
}

/// Generate embeddings for all symbols in the store that don't have one yet.
///
/// Returns the number of embeddings generated.
pub fn generate_embeddings(store: &GraphStore) -> Result<usize, String> {
    init_embeddings_table(store).map_err(|e| e.to_string())?;

    let files = store.get_all_files().map_err(|e| e.to_string())?;
    let mut count = 0;

    for file in &files {
        let symbols = store.get_symbols_by_file(file.id).map_err(|e| e.to_string())?;

        for sym in &symbols {
            // Check if embedding already exists
            let existing: Option<i64> = store.conn()
                .query_row(
                    "SELECT symbol_id FROM embeddings WHERE symbol_id = ?1",
                    [sym.id],
                    |row| row.get(0),
                )
                .ok();

            if existing.is_some() {
                continue; // Already embedded
            }

            let text = symbol_to_text(&sym.name, &sym.kind, &sym.qualified_name);
            match embed_text(&text) {
                Ok(vector) => {
                    let blob = vector_to_blob(&vector);
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;

                    store.conn().execute(
                        "INSERT OR REPLACE INTO embeddings (symbol_id, vector, updated_at)
                         VALUES (?1, ?2, ?3)",
                        rusqlite::params![sym.id, blob, now],
                    ).map_err(|e| e.to_string())?;
                    count += 1;
                }
                Err(e) => {
                    eprintln!("Warning: failed to embed symbol '{}': {}", sym.name, e);
                    continue;
                }
            }
        }
    }

    Ok(count)
}

/// Semantic search: find symbols whose embeddings are most similar to the query.
///
/// Uses exact brute-force cosine similarity. For indexes with >10k symbols,
/// consider adding an approximate nearest neighbor index.
pub fn semantic_search(
    store: &GraphStore,
    query: &str,
    limit: usize,
) -> Result<Vec<SemanticSearchHit>, String> {
    let query_embedding = embed_text(query)?;

    // Load all embeddings and compute cosine similarity
    let mut stmt = store.conn().prepare(
        "SELECT e.symbol_id, e.vector, s.name, s.qualified_name, s.kind, f.path, s.line
         FROM embeddings e
         JOIN symbols s ON s.id = e.symbol_id
         JOIN files f ON f.id = s.file_id"
    ).map_err(|e| e.to_string())?;

    let rows = stmt.query_map([], |row| {
        let blob: Vec<u8> = row.get(1)?;
        let vector = blob_to_vector(&blob);
        Ok((
            row.get::<_, i64>(0)?,
            vector,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, i64>(6)? as usize,
        ))
    }).map_err(|e| e.to_string())?;

    let mut hits: Vec<SemanticSearchHit> = Vec::new();
    for row in rows {
        let (sym_id, vector, name, qualified_name, kind, file_path, line) = row.map_err(|e| e.to_string())?;
        let similarity = cosine_similarity(&query_embedding, &vector);
        hits.push(SemanticSearchHit {
            symbol_id: sym_id,
            name,
            qualified_name,
            kind,
            file_path,
            line,
            similarity,
        });
    }

    // Sort by similarity (highest first)
    hits.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
    hits.truncate(limit);

    Ok(hits)
}

/// Hybrid search: combines BM25 text search with semantic embedding search.
///
/// Results from both methods are merged using Reciprocal Rank Fusion.
pub fn hybrid_semantic_search(
    store: &GraphStore,
    query: &str,
    limit: usize,
) -> Result<Vec<SemanticSearchHit>, String> {
    // Get BM25 results
    let bm25_results = crate::search::search(
        store,
        query,
        &crate::search::SearchConfig { limit: limit * 2, ..Default::default() },
    ).map_err(|e| e.to_string())?;

    // Get semantic results
    let semantic_results = semantic_search(store, query, limit * 2)?;

    // Merge using Reciprocal Rank Fusion (RRF)
    let k = 60.0; // RRF constant
    let mut scores: std::collections::HashMap<i64, f64> = std::collections::HashMap::new();

    for (rank, hit) in bm25_results.iter().enumerate() {
        let score = 1.0 / (k + rank as f64 + 1.0);
        scores.entry(hit.node_id).or_insert(0.0);
        scores.insert(hit.node_id, scores[&hit.node_id] + score * 0.5); // BM25 weight
    }

    for (rank, hit) in semantic_results.iter().enumerate() {
        let score = 1.0 / (k + rank as f64 + 1.0);
        scores.entry(hit.symbol_id).or_insert(0.0);
        scores.insert(hit.symbol_id, scores[&hit.symbol_id] + score * 0.5); // Semantic weight
    }

    // Build final results from semantic results (they have richer data)
    let mut merged: Vec<SemanticSearchHit> = semantic_results.into_iter()
        .filter(|h| scores.contains_key(&h.symbol_id))
        .collect();

    // Add BM25-only results that weren't in semantic results
    for bm25_hit in &bm25_results {
        if !merged.iter().any(|h| h.symbol_id == bm25_hit.node_id) {
            // Fetch full symbol info
            if let Ok(Some(sym)) = store.get_symbols_by_name(&bm25_hit.name).map(|s| s.into_iter().next()) {
                if let Some(file) = store.get_file_by_id(sym.file_id).ok().flatten() {
                    merged.push(SemanticSearchHit {
                        symbol_id: bm25_hit.node_id,
                        name: bm25_hit.name.clone(),
                        qualified_name: sym.qualified_name,
                        kind: bm25_hit.kind.clone(),
                        file_path: file.path,
                        line: bm25_hit.line,
                        similarity: scores.get(&bm25_hit.node_id).copied().unwrap_or(0.0) as f32,
                    });
                }
            }
        }
    }

    merged.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
    merged.truncate(limit);

    Ok(merged)
}

// =================================================================
// Vector utilities
// =================================================================

/// Convert a Vec<f32> to a compact binary blob for SQLite storage.
fn vector_to_blob(vector: &[f32]) -> Vec<u8> {
    vector.iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

/// Convert a binary blob back to Vec<f32>.
fn blob_to_vector(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Compute cosine similarity between two vectors.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < 1e-8 {
        0.0
    } else {
        dot / denom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let v = vec![1.0, 0.0, 0.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 0.001, "Identical vectors should have similarity 1.0");
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 0.001, "Orthogonal vectors should have similarity ~0");
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![-1.0, 0.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < 0.001, "Opposite vectors should have similarity -1.0");
    }

    #[test]
    fn test_vector_blob_roundtrip() {
        let original = vec![1.5, -2.3, 0.0, 0.001, 1000.0];
        let blob = vector_to_blob(&original);
        let recovered = blob_to_vector(&blob);
        assert_eq!(original, recovered, "Vector should survive blob roundtrip");
    }

    #[test]
    fn test_symbol_to_text() {
        let text = symbol_to_text("login", "DefinitionFunction", "auth::login");
        assert!(text.contains("login"));
        assert!(text.contains("DefinitionFunction"));
        assert!(text.contains("auth::login"));
    }
}
