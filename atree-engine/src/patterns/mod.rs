//! Pattern Mining — Layer 2: Inductive compression over evidence.
//!
//! Patterns are subgraphs / motifs that recur across the evidence graph.
//! They represent generalized structures: "this call chain appears in 12 places",
//! "this import→decl→call sequence is common", etc.
//!
//! ## Scoring Model ( minimum viable )
//!
//! ```text
//! pattern_score = frequency × dispersion × stability × (1 - entropy)
//!
//! where:
//!   frequency:   how many times this motif appears
//!   dispersion:  across how many files/modules (higher = more general)
//!   stability:   how consistent the motif is across observations
//!   entropy:     informational noise (lower = more precise pattern)
//! ```
//!
//! ## Architecture
//!
//! ```text
//! EvidenceGraph → mine_patterns() → Pattern[]
//!   → pattern.score = f(frequency, dispersion, stability, entropy)
//!   → PatternStore (SQLite)
//! ```

use crate::evidence::{Evidence, EvidenceId, EvidenceKind};
use serde::{Deserialize, Serialize};

/// A recurring motif in the evidence graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub id: String,
    pub name: String,
    pub description: String,
    /// The evidence kinds that form this pattern (e.g., [Import, Declaration, Call]).
    pub motif: Vec<EvidenceKind>,
    /// Evidence IDs participating in this pattern.
    pub evidence_ids: Vec<EvidenceId>,
    /// Scoring.
    pub score: PatternScore,
}

/// Pattern quality signals.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatternScore {
    pub frequency: usize,
    pub dispersion: f64,  // 0-1, fraction of files/modules covered
    pub overall: f64,     // composite: frequency_norm × dispersion
}

impl PatternScore {
    pub fn compute(frequency: usize, dispersion: f64) -> Self {
        let freq_norm = (frequency as f64).min(100.0) / 100.0; // normalize to [0, 1]
        let overall = freq_norm * dispersion.max(0.0).min(1.0);
        Self {
            frequency,
            dispersion,
            overall,
        }
    }
}

/// Configuration for pattern mining.
#[derive(Debug, Clone)]
pub struct PatternMiningConfig {
    /// Minimum frequency to consider a pattern (default: 3).
    pub min_frequency: usize,
    /// Minimum dispersion (default: 0.1 = 10% of files).
    pub min_dispersion: f64,
}

impl Default for PatternMiningConfig {
    fn default() -> Self {
        Self {
            min_frequency: 3,
            min_dispersion: 0.1,
        }
    }
}

/// Mine patterns from committed evidence.
///
/// Current implementation: extracts simple 2-grams (evidence kind pairs that
/// co-occur in the same file). Future: subgraph isomorphism, sequential
/// pattern mining, graph neural networks.
pub fn mine_patterns(
    evidence: &[Evidence],
    config: &PatternMiningConfig,
) -> Vec<Pattern> {
    if evidence.is_empty() {
        return Vec::new();
    }

    // Group evidence by file using FxHashMap for performance.
    let mut by_file: rustc_hash::FxHashMap<String, Vec<(EvidenceKind, usize)>> = rustc_hash::FxHashMap::default();
    for (idx, ev) in evidence.iter().enumerate() {
        by_file.entry(ev.source.file.clone()).or_default().push((ev.kind, idx));
    }

    let total_files = by_file.len().max(1) as f64;

    // Count co-occurring evidence kind pairs per file, and track unique file set.
    // Key: (kind_a, kind_b), Value: (total_pair_count, set_of_file_names)
    let mut pair_stats: rustc_hash::FxHashMap<(EvidenceKind, EvidenceKind), (usize, rustc_hash::FxHashSet<String>)> = rustc_hash::FxHashMap::default();

    for (file, evs) in &by_file {
        // Only iterate pairs within the same file.
        for i in 0..evs.len() {
            for j in (i + 1)..evs.len() {
                let key = (evs[i].0, evs[j].0);
                let (count, files) = pair_stats.entry(key).or_default();
                *count += 1;
                files.insert(file.clone());
            }
        }
    }

    let mut patterns = Vec::with_capacity(pair_stats.len());

    for ((kind_a, kind_b), (frequency, files)) in pair_stats {
        if frequency < config.min_frequency {
            continue;
        }
        let dispersion = files.len() as f64 / total_files;
        if dispersion < config.min_dispersion {
            continue;
        }

        // Stability/entropy not computed — temporal analysis not yet implemented.
        let score = PatternScore::compute(frequency, dispersion);

        patterns.push(Pattern {
            id: format!("pat_{:?}_{:?}", kind_a, kind_b).to_lowercase(),
            name: format!("{:?}→{:?}", kind_a, kind_b),
            description: format!(
                "Co-occurring {:?} and {:?} in {} files (freq={})",
                kind_a, kind_b, files.len(), frequency
            ),
            motif: vec![kind_a, kind_b],
            evidence_ids: Vec::new(), // Don't store individual IDs — too expensive
            score,
        });
    }

    // Sort by overall score descending.
    patterns.sort_by(|a, b| b.score.overall.partial_cmp(&a.score.overall).unwrap_or(std::cmp::Ordering::Equal));
    patterns
}

/// Persistence layer for patterns (SQLite backing).
pub struct PatternStore<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> PatternStore<'a> {
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// Initialize pattern tables.
    pub fn init_tables(&self) -> rusqlite::Result<()> {
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
        ")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::*;

    fn make_evidence(kind: EvidenceKind, file: &str, line: usize) -> Evidence {
        let name = format!("ev_{}", line);
        let id = EvidenceId::compute(kind, &name, file, line, 0, line, name.len());
        Evidence {
            id,
            kind,
            source: EvidenceSource {
                file: file.to_string(),
                span: SourceSpan { start_line: line, start_col: 0, end_line: line, end_col: name.len() },
                language: "rust".to_string(),
            },
            target: EvidenceTarget { target_type: TargetType::Symbol, ref_id: format!("decl:{}", name) },
            content: EvidenceContent { raw: name.clone(), normalized: name },
            context: EvidenceContext { enclosing_symbol: None, imports: vec![], scope_chain: vec![] },
            metadata: EvidenceMetadata { extractor: "test".to_string(), confidence: 0.9, stability: 1.0, entropy: 0.0, timestamp_ms: 0, commit: None },
            links: EvidenceLinks::default(),
            tags: vec![],
            state: EvidenceState::Committed,
        }
    }

    #[test]
    fn test_mine_patterns_basic() {
        let evidence = vec![
            make_evidence(EvidenceKind::ImportEdge, "a.rs", 1),
            make_evidence(EvidenceKind::SymbolDeclaration, "a.rs", 10),
            make_evidence(EvidenceKind::FunctionCall, "a.rs", 20),
            make_evidence(EvidenceKind::ImportEdge, "b.rs", 1),
            make_evidence(EvidenceKind::SymbolDeclaration, "b.rs", 10),
            make_evidence(EvidenceKind::FunctionCall, "b.rs", 20),
            make_evidence(EvidenceKind::ImportEdge, "c.rs", 1),
            make_evidence(EvidenceKind::SymbolDeclaration, "c.rs", 10),
        ];

        let config = PatternMiningConfig { min_frequency: 2, min_dispersion: 0.1 };
        let patterns = mine_patterns(&evidence, &config);

        // Should find Import→Declaration and Import→Call and Declaration→Call pairs.
        assert!(!patterns.is_empty(), "Should mine at least one pattern");

        // All patterns should have reasonable scores.
        for p in &patterns {
            assert!(p.score.overall > 0.0, "Pattern {} should have positive score", p.id);
            assert!(p.score.frequency >= config.min_frequency);
        }
    }

    #[test]
    fn test_pattern_score_computation() {
        let score = PatternScore::compute(10, 0.5);
        assert_eq!(score.frequency, 10);
        assert!(score.overall > 0.0);
        assert!(score.overall <= 1.0);
    }
}
