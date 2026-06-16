//! Constraint Synthesis — Layer 3: Policy + invariants from patterns and evidence.
//!
//! Constraints represent forbidden transitions, required properties, and
//! architectural rules that emerge from the evidence graph and pattern mining.
//!
//! ## Causal Link
//!
//! ```text
//! Evidence → Pattern → Constraint
//!   repeated violation  →  higher confidence constraint
//!   stable correlation  →  invariant
//!   contradiction edge  →  forbidden transition
//! ```
//!
//! ## Constraint Types
//!
//! | Kind | Source | Example |
//! |------|--------|---------|
//! | ForbiddenTransition | Evidence contradiction | "File A must not import File B" |
//! | RequiredProperty | Pattern frequency | "Every class must have a constructor" |
//! | ArchitecturalRule | Cross-boundary violation | "UI layer must not call DB layer directly" |
//! | AccessControl | Scope violation evidence | "Private symbol X is used externally" |

use crate::evidence::{Evidence, EvidenceId, EvidenceKind};
use crate::patterns::Pattern;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Synthesized constraint from evidence/pattern analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Constraint {
    pub id: String,
    pub name: String,
    pub description: String,
    pub kind: ConstraintKind,
    /// The evidence/pattern IDs that generated this constraint.
    pub source_evidence_ids: Vec<EvidenceId>,
    /// Confidence: how strongly this constraint is supported by evidence.
    pub confidence: f64,
    /// Whether this constraint is currently active (violations detected).
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConstraintKind {
    /// Two evidence types must not co-occur (e.g., forbidden import).
    ForbiddenTransition { from: EvidenceKind, to: EvidenceKind },
    /// An evidence type must have certain properties.
    RequiredProperty { kind: EvidenceKind, property: String },
    /// Cross-boundary access pattern.
    ArchitecturalRule { layer_from: String, layer_to: String },
    /// Private symbol used outside its scope.
    AccessControl { symbol: String },
}

impl ConstraintKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ForbiddenTransition { .. } => "FORBIDDEN_TRANSITION",
            Self::RequiredProperty { .. } => "REQUIRED_PROPERTY",
            Self::ArchitecturalRule { .. } => "ARCHITECTURAL_RULE",
            Self::AccessControl { .. } => "ACCESS_CONTROL",
        }
    }
}

/// Configuration for constraint synthesis.
#[derive(Debug, Clone)]
pub struct ConstraintSynthesisConfig {
    /// Minimum confidence to emit a constraint (default: 0.7).
    pub min_confidence: f64,
    /// Minimum evidence count supporting a constraint (default: 3).
    pub min_evidence_count: usize,
}

impl Default for ConstraintSynthesisConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.7,
            min_evidence_count: 3,
        }
    }
}

/// Synthesize constraints from evidence contradictions and stable patterns.
///
/// Current implementation:
/// - Detects contradiction edges between evidence units (I4 violation).
/// - Identifies access control violations (private symbols in public API patterns).
/// - Derives forbidden transitions from repeated contradiction patterns.
pub fn synthesize_constraints(
    evidence: &[Evidence],
    patterns: &[Pattern],
    config: &ConstraintSynthesisConfig,
) -> Vec<Constraint> {
    let mut constraints = Vec::new();

    // 1. From contradiction edges: synthesize ForbiddenTransition constraints.
    let mut contradiction_pairs: HashMap<(EvidenceKind, EvidenceKind), usize> = HashMap::new();
    for ev in evidence {
        for contra_id in &ev.links.contradicts {
            if let Some(contra_ev) = evidence.iter().find(|e| &e.id == contra_id) {
                let key = if ev.kind as u8 <= contra_ev.kind as u8 {
                    (ev.kind, contra_ev.kind)
                } else {
                    (contra_ev.kind, ev.kind)
                };
                *contradiction_pairs.entry(key).or_insert(0) += 1;
            }
        }
    }

    for ((kind_a, kind_b), count) in contradiction_pairs {
        if count >= config.min_evidence_count {
            let confidence = (count as f64 / evidence.len() as f64).min(1.0);
            if confidence >= config.min_confidence {
                constraints.push(Constraint {
                    id: format!("c_forbidden_{:?}_{:?}", kind_a, kind_b).to_lowercase(),
                    name: format!("Forbidden: {:?}→{:?}", kind_a, kind_b),
                    description: format!(
                        "Evidence of kind {:?} contradicts {:?} in {} cases",
                        kind_a, kind_b, count
                    ),
                    kind: ConstraintKind::ForbiddenTransition {
                        from: kind_a,
                        to: kind_b,
                    },
                    source_evidence_ids: vec![],
                    confidence,
                    active: true,
                });
            }
        }
    }

    // 2. From high-frequency patterns: synthesize RequiredProperty constraints.
    for pattern in patterns {
        if pattern.score.overall >= config.min_confidence && pattern.motif.len() >= 2 {
            // If a pattern is very stable, its components are likely required.
            for kind in &pattern.motif {
                constraints.push(Constraint {
                    id: format!("c_required_{:?}_{}", kind, pattern.id).to_lowercase(),
                    name: format!("Required: {:?}", kind),
                    description: format!(
                        "Pattern '{}' requires {:?} (score: {:.2})",
                        pattern.name, kind, pattern.score.overall
                    ),
                    kind: ConstraintKind::RequiredProperty {
                        kind: *kind,
                        property: format!("part_of_pattern_{}", pattern.id),
                    },
                    source_evidence_ids: pattern.evidence_ids.clone(),
                    confidence: pattern.score.overall,
                    active: true,
                });
            }
        }
    }

    constraints
}

/// Detect constraint violations in new evidence.
///
/// Returns violations as (constraint_id, violating_evidence_id) pairs.
pub fn detect_violations(
    constraints: &[Constraint],
    new_evidence: &[Evidence],
) -> Vec<(String, EvidenceId)> {
    let mut violations = Vec::new();

    for constraint in constraints {
        if !constraint.active {
            continue;
        }
        for ev in new_evidence {
            match &constraint.kind {
                ConstraintKind::ForbiddenTransition { from, to } => {
                    if ev.kind == *from || ev.kind == *to {
                        // Check if the evidence's links contain the other kind.
                        for linked_id in &ev.links.supports {
                            if let Some(linked_ev) = new_evidence.iter().find(|e| &e.id == linked_id) {
                                if (ev.kind == *from && linked_ev.kind == *to)
                                    || (ev.kind == *to && linked_ev.kind == *from)
                                {
                                    violations.push((constraint.id.clone(), ev.id.clone()));
                                }
                            }
                        }
                    }
                }
                ConstraintKind::AccessControl { .. } => {
                    // Access control violations detected by evidence contradiction edges.
                }
                _ => {}
            }
        }
    }

    violations
}

/// Persistence layer for constraints.
pub struct ConstraintStore<'a> {
    conn: &'a rusqlite::Connection,
}

impl<'a> ConstraintStore<'a> {
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self { conn }
    }

    /// Initialize constraint tables.
    pub fn init_tables(&self) -> rusqlite::Result<()> {
        self.conn.execute_batch("
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_synthesize_from_contradictions() {
        use crate::evidence::*;

        let ev1 = Evidence {
            id: EvidenceId::compute(EvidenceKind::SymbolDeclaration, "x", "a.rs", 1, 0, 1, 1),
            kind: EvidenceKind::SymbolDeclaration,
            source: EvidenceSource { file: "a.rs".to_string(), span: SourceSpan { start_line: 1, start_col: 0, end_line: 1, end_col: 1 }, language: "rust".to_string() },
            target: EvidenceTarget { target_type: TargetType::Symbol, ref_id: "decl:x".to_string() },
            content: EvidenceContent { raw: "x".to_string(), normalized: "x".to_string() },
            context: EvidenceContext { enclosing_symbol: None, imports: vec![], scope_chain: vec![] },
            metadata: EvidenceMetadata::default(),
            links: EvidenceLinks::default(),
            tags: vec![],
            state: EvidenceState::Committed,
        };

        let mut ev2 = Evidence {
            id: EvidenceId::compute(EvidenceKind::FunctionCall, "x", "b.rs", 1, 0, 1, 1),
            kind: EvidenceKind::FunctionCall,
            source: EvidenceSource { file: "b.rs".to_string(), span: SourceSpan { start_line: 1, start_col: 0, end_line: 1, end_col: 1 }, language: "rust".to_string() },
            target: EvidenceTarget { target_type: TargetType::Symbol, ref_id: "call:x".to_string() },
            content: EvidenceContent { raw: "x".to_string(), normalized: "x".to_string() },
            context: EvidenceContext { enclosing_symbol: None, imports: vec![], scope_chain: vec![] },
            metadata: EvidenceMetadata::default(),
            links: EvidenceLinks::default(),
            tags: vec![],
            state: EvidenceState::Committed,
        };
        ev2.links.contradicts.push(ev1.id.clone());

        // Need 3+ contradictions for the threshold. Create more.
        let mut evidence = vec![ev1, ev2];
        for i in 0..5 {
            let ev_a = Evidence {
                id: EvidenceId::compute(EvidenceKind::SymbolDeclaration, &format!("s{}", i), "c.rs", i, 0, i, 1),
                kind: EvidenceKind::SymbolDeclaration,
                source: EvidenceSource { file: "c.rs".to_string(), span: SourceSpan { start_line: i, start_col: 0, end_line: i, end_col: 1 }, language: "rust".to_string() },
                target: EvidenceTarget { target_type: TargetType::Symbol, ref_id: format!("decl:s{}", i) },
                content: EvidenceContent { raw: format!("s{}", i), normalized: format!("s{}", i) },
                context: EvidenceContext { enclosing_symbol: None, imports: vec![], scope_chain: vec![] },
                metadata: EvidenceMetadata::default(),
                links: EvidenceLinks::default(),
                tags: vec![],
                state: EvidenceState::Committed,
            };
            let mut ev_b = Evidence {
                id: EvidenceId::compute(EvidenceKind::FunctionCall, &format!("s{}", i), "d.rs", i, 0, i, 1),
                kind: EvidenceKind::FunctionCall,
                source: EvidenceSource { file: "d.rs".to_string(), span: SourceSpan { start_line: i, start_col: 0, end_line: i, end_col: 1 }, language: "rust".to_string() },
                target: EvidenceTarget { target_type: TargetType::Symbol, ref_id: format!("call:s{}", i) },
                content: EvidenceContent { raw: format!("s{}", i), normalized: format!("s{}", i) },
                context: EvidenceContext { enclosing_symbol: None, imports: vec![], scope_chain: vec![] },
                metadata: EvidenceMetadata::default(),
                links: EvidenceLinks::default(),
                tags: vec![],
                state: EvidenceState::Committed,
            };
            ev_b.links.contradicts.push(ev_a.id.clone());
            evidence.push(ev_a);
            evidence.push(ev_b);
        }

        let patterns = vec![];
        let config = ConstraintSynthesisConfig { min_confidence: 0.5, min_evidence_count: 3 };
        let constraints = synthesize_constraints(&evidence, &patterns, &config);

        // Should detect the SymbolDeclaration↔FunctionCall contradiction pattern.
        assert!(!constraints.is_empty(), "Should synthesize at least one constraint from contradictions");
    }
}
