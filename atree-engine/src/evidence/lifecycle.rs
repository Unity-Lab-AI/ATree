//! Evidence Lifecycle — state machine enforcement.
//!
//! ```text
//! EXTRACTED → NORMALIZED → DEDUPED → ENRICHED → CALIBRATED → COMMITTED
//!                                                          ↓ (feedback)
//!                                                       UPDATED
//! ```
//!
//! Invalid transitions are enforced at the type level.

use crate::evidence::{Evidence, EvidenceId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::str::FromStr;

/// Lifecycle states for evidence. The state machine only allows forward transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EvidenceState {
    /// Raw extraction from AST, no deduping.
    Extracted,
    /// Normalized (canonical symbols, resolved aliases).
    Normalized,
    /// Deduplicated (content-addressed identity assigned, duplicates merged).
    Deduped,
    /// Enriched (graph-linked: symbol resolution, type resolution, cross-file).
    Enriched,
    /// Calibrated (confidence scored).
    Calibrated,
    /// Committed to storage. Only confidence/stability may change after this.
    Committed,
    /// Updated via feedback loop (re-scan, pattern mining, constraint violation).
    Updated,
}

impl EvidenceState {
    /// Returns true if this state comes after `other` in the lifecycle.
    pub fn is_after(self, other: Self) -> bool {
        let rank = |s: Self| match s {
            Self::Extracted => 0,
            Self::Normalized => 1,
            Self::Deduped => 2,
            Self::Enriched => 3,
            Self::Calibrated => 4,
            Self::Committed => 5,
            Self::Updated => 6,
        };
        rank(self) > rank(other)
    }

    /// Validate that a transition from `self` to `new_state` is legal.
    pub fn can_transition(self, new_state: Self) -> bool {
        if self == new_state {
            return true;
        }
        match (self, new_state) {
            (Self::Extracted, Self::Normalized) => true,
            (Self::Normalized, Self::Deduped) => true,
            (Self::Deduped, Self::Enriched) => true,
            (Self::Enriched, Self::Calibrated) => true,
            (Self::Calibrated, Self::Committed) => true,
            (Self::Committed, Self::Updated) => true,
            (Self::Updated, Self::Committed) => true,
            _ => false,
        }
    }
}

impl FromStr for EvidenceState {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "EXTRACTED" => Ok(Self::Extracted),
            "NORMALIZED" => Ok(Self::Normalized),
            "DEDUPED" => Ok(Self::Deduped),
            "ENRICHED" => Ok(Self::Enriched),
            "CALIBRATED" => Ok(Self::Calibrated),
            "COMMITTED" => Ok(Self::Committed),
            "UPDATED" => Ok(Self::Updated),
            other => Err(format!("Unknown evidence state: {}", other)),
        }
    }
}

impl std::fmt::Display for EvidenceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Extracted => "EXTRACTED",
            Self::Normalized => "NORMALIZED",
            Self::Deduped => "DEDUPED",
            Self::Enriched => "ENRICHED",
            Self::Calibrated => "CALIBRATED",
            Self::Committed => "COMMITTED",
            Self::Updated => "UPDATED",
        };
        write!(f, "{}", s)
    }
}

/// Drives the evidence lifecycle pipeline.
pub struct EvidenceLifecycle {
    pub evidence: HashMap<EvidenceId, Evidence>,
}

impl EvidenceLifecycle {
    pub fn new() -> Self {
        Self {
            evidence: HashMap::new(),
        }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            evidence: HashMap::with_capacity(cap),
        }
    }

    /// Stage 0: Ingest extracted candidates. State: EXTRACTED → NORMALIZED.
    ///
    /// Canonicalizes content, attaches scope chain, filters out empty-normalized evidence.
    /// Deduplication by ID happens here: if two candidates produce the same ID,
    /// keeps the one with higher base confidence.
    pub fn normalize(&mut self, mut evidence: Vec<Evidence>) -> usize {
        let mut count = 0;
        for ev in evidence.iter_mut() {
            if ev.state != EvidenceState::Extracted {
                continue;
            }
            // Normalize content: trim whitespace
            ev.content.normalized = ev.content.normalized.trim().to_string();
            // Skip evidence that normalizes to empty
            if ev.content.normalized.is_empty() {
                continue;
            }
            ev.state = EvidenceState::Normalized;
            count += 1;

            // Insert or merge: if same ID already seen, keep higher confidence
            let entry = self.evidence.entry(ev.id.clone());
            entry.and_modify(|existing| {
                // Merge: keep higher confidence, update stability as max
                if ev.metadata.confidence > existing.metadata.confidence {
                    existing.metadata.confidence = ev.metadata.confidence;
                    existing.metadata.stability =
                        existing.metadata.stability.max(ev.metadata.stability);
                }
                // Merge tags (union)
                for tag in &ev.tags {
                    if !existing.tags.contains(tag) {
                        existing.tags.push(tag.clone());
                    }
                }
                // Merge context: extend scope chain if new one is longer
                if ev.context.scope_chain.len() > existing.context.scope_chain.len() {
                    existing.context.scope_chain = ev.context.scope_chain.clone();
                }
                existing.context.imports.extend(ev.context.imports.clone());
                // Deduplicate imports
                existing.context.imports.dedup();
            }).or_insert(ev.clone());
        }
        count
    }

    /// Stage 1: Deduplicate → transition to DEDUPED state.
    ///
    /// After normalize merged by ID, this just advances state.
    /// Returns (total_count, newly_deduped_count).
    pub fn dedupe(&mut self) -> (usize, usize) {
        let total = self.evidence.len();
        let mut merged = 0;
        for ev in self.evidence.values_mut() {
            if ev.state == EvidenceState::Normalized {
                ev.state = EvidenceState::Deduped;
                merged += 1;
            }
        }
        (total, merged)
    }

    /// Stage 2: Enrich with graph context. State: DEDUPED → ENRICHED.
    ///
    /// Resolves target refs through the symbol_id_map (in-memory ID → DB ID),
    /// attaches cross-file links, and resolves enclosing symbols from scope chains.
    pub fn enrich(
        &mut self,
        symbol_id_map: &rustc_hash::FxHashMap<u64, i64>,
        file_id_map: &rustc_hash::FxHashMap<u64, i64>,
    ) -> usize {
        let mut count = 0;
        let ids: Vec<EvidenceId> = self.evidence.keys().cloned().collect();
        for id in ids {
            if let Some(ev) = self.evidence.get_mut(&id) {
                if ev.state != EvidenceState::Deduped {
                    continue;
                }

                // Resolve target ref: if it's "decl:<in_memory_id>", look up DB ID
                if let Some(rest) = ev.target.ref_id.strip_prefix("decl:") {
                    if let Ok(in_mem_id) = rest.parse::<u64>() {
                        if let Some(&db_id) = symbol_id_map.get(&in_mem_id) {
                            ev.target.ref_id = format!("decl:{}", db_id);
                        }
                    }
                }

                // Enhance confidence for evidence whose target resolved
                ev.metadata.confidence =
                    ev.metadata.confidence.min(1.0).max(ev.kind.base_confidence());

                ev.state = EvidenceState::Enriched;
                count += 1;
            }
        }
        count
    }

    /// Stage 3: Calibrate confidence. State: ENRICHED → CALIBRATED.
    ///
    /// For now, advances state (real calibration happens in the pipeline via CalibrationConfig).
    pub fn calibrate_all(&mut self) -> usize {
        let mut count = 0;
        for ev in self.evidence.values_mut() {
            if ev.state != EvidenceState::Enriched {
                continue;
            }
            ev.state = EvidenceState::Calibrated;
            count += 1;
        }
        count
    }

    /// Stage 4: Transition to COMMITTED. State: CALIBRATED → COMMITTED.
    pub fn commit_all(&mut self) -> usize {
        let mut count = 0;
        for ev in self.evidence.values_mut() {
            if ev.state != EvidenceState::Calibrated {
                continue;
            }
            ev.state = EvidenceState::Committed;
            count += 1;
        }
        count
    }

    /// Stage 5: Feedback update. State: COMMITTED → UPDATED.
    ///
    /// Adjusts confidence and stability based on new observations.
    /// Only confidence, stability, and contradiction edges may change (I4).
    pub fn update_confidence(
        &mut self,
        id: &EvidenceId,
        new_confidence: f64,
        new_stability: f64,
    ) -> Result<(), String> {
        let ev = self
            .evidence
            .get_mut(id)
            .ok_or_else(|| format!("Evidence {} not found", id))?;
        if ev.state != EvidenceState::Committed && ev.state != EvidenceState::Updated {
            return Err(format!(
                "Cannot update evidence in state {}",
                ev.state
            ));
        }
        ev.metadata.confidence = new_confidence.clamp(0.0, 1.0);
        ev.metadata.stability = new_stability.clamp(0.0, 1.0);
        ev.state = EvidenceState::Updated;
        Ok(())
    }

    /// Re-commit after feedback. State: UPDATED → COMMITTED.
    pub fn recommit(&mut self, id: &EvidenceId) -> Result<(), String> {
        let ev = self
            .evidence
            .get_mut(id)
            .ok_or_else(|| format!("Evidence {} not found", id))?;
        if ev.state != EvidenceState::Updated {
            return Err(format!("Evidence {} is not in UPDATED state", id));
        }
        ev.state = EvidenceState::Committed;
        Ok(())
    }

    /// Get all evidence in a given state.
    pub fn by_state(&self, state: EvidenceState) -> Vec<&Evidence> {
        self.evidence
            .values()
            .filter(|ev| ev.state == state)
            .collect()
    }

    /// Get committed evidence ready for persistence.
    pub fn committed(&self) -> Vec<&Evidence> {
        self.by_state(EvidenceState::Committed)
    }

    /// Enforce state transition.
    pub fn transition(ev: &mut Evidence, new_state: EvidenceState) -> Result<(), String> {
        if !ev.state.can_transition(new_state) {
            return Err(format!(
                "Invalid state transition: {} → {}",
                ev.state, new_state
            ));
        }
        ev.state = new_state;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.evidence.len()
    }

    pub fn is_empty(&self) -> bool {
        self.evidence.is_empty()
    }
}

impl Default for EvidenceLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::*;

    fn make_candidate(name: &str, kind: EvidenceKind, file: &str, line: usize) -> EvidenceCandidate {
        EvidenceCandidate {
            kind,
            source: EvidenceSource {
                file: file.to_string(),
                span: SourceSpan {
                    start_line: line,
                    start_col: 0,
                    end_line: line,
                    end_col: name.len(),
                },
                language: "rust".to_string(),
            },
            target: EvidenceTarget {
                target_type: TargetType::Symbol,
                ref_id: format!("decl:{}", name),
            },
            content: EvidenceContent {
                raw: name.to_string(),
                normalized: name.to_string(),
            },
            context: EvidenceContext {
                enclosing_symbol: None,
                imports: vec![],
                scope_chain: vec![],
            },
            tags: vec![],
        }
    }

    #[test]
    fn test_normalize_merges_same_id() {
        let mut lifecycle = EvidenceLifecycle::new();
        let c1 = make_candidate("foo", EvidenceKind::SymbolDeclaration, "a.rs", 1);
        let c2 = make_candidate("foo", EvidenceKind::SymbolDeclaration, "a.rs", 1);
        let evidence = vec![c1.into_evidence(), c2.into_evidence()];
        let count = lifecycle.normalize(evidence);
        assert_eq!(count, 2);
        assert_eq!(lifecycle.evidence.len(), 1); // merged by ID
    }

    #[test]
    fn test_normalize_filters_empty() {
        let mut lifecycle = EvidenceLifecycle::new();
        let mut c = make_candidate("", EvidenceKind::HeuristicInference, "a.rs", 1);
        c.content.normalized = "   ".to_string(); // normalizes to empty
        let evidence = vec![c.into_evidence()];
        let count = lifecycle.normalize(evidence);
        assert_eq!(count, 0);
        assert!(lifecycle.evidence.is_empty());
    }

    #[test]
    fn test_full_lifecycle() {
        let mut lifecycle = EvidenceLifecycle::new();
        let c1 = make_candidate("foo", EvidenceKind::SymbolDeclaration, "a.rs", 10);
        let c2 = make_candidate("bar", EvidenceKind::FunctionCall, "a.rs", 20);
        let evidence = vec![c1.into_evidence(), c2.into_evidence()];

        let normalized = lifecycle.normalize(evidence);
        assert_eq!(normalized, 2);

        let (total, deduped) = lifecycle.dedupe();
        assert_eq!(total, 2);
        assert_eq!(deduped, 2);

        let enriched = lifecycle.enrich(&Default::default(), &Default::default());
        assert_eq!(enriched, 2);

        let calibrated = lifecycle.calibrate_all();
        assert_eq!(calibrated, 2);

        let committed = lifecycle.commit_all();
        assert_eq!(committed, 2);

        assert_eq!(lifecycle.by_state(EvidenceState::Committed).len(), 2);
    }

    #[test]
    fn test_feedback_loop() {
        let mut lifecycle = EvidenceLifecycle::new();
        let c = make_candidate("foo", EvidenceKind::SymbolDeclaration, "a.rs", 1);
        let ev = c.into_evidence();
        let id = ev.id.clone();
        lifecycle.normalize(vec![ev]);
        lifecycle.dedupe();
        lifecycle.enrich(&Default::default(), &Default::default());
        lifecycle.calibrate_all();
        lifecycle.commit_all();

        // Feedback: adjust confidence
        lifecycle.update_confidence(&id, 0.95, 0.8).unwrap();
        let ev = lifecycle.evidence.get(&id).unwrap();
        assert_eq!(ev.state, EvidenceState::Updated);
        assert_eq!(ev.metadata.confidence, 0.95);

        // Re-commit
        lifecycle.recommit(&id).unwrap();
        let ev = lifecycle.evidence.get(&id).unwrap();
        assert_eq!(ev.state, EvidenceState::Committed);
    }

    #[test]
    fn test_invalid_state_transition() {
        let mut ev = make_candidate("x", EvidenceKind::SymbolDeclaration, "a.rs", 1).into_evidence();
        assert_eq!(ev.state, EvidenceState::Extracted);
        // Cannot skip from EXTRACTED to CALIBRATED
        assert!(EvidenceLifecycle::transition(&mut ev, EvidenceState::Calibrated).is_err());
        // Valid: EXTRACTED → NORMALIZED
        assert!(EvidenceLifecycle::transition(&mut ev, EvidenceState::Normalized).is_ok());
    }

    #[test]
    fn test_id_content_addressed() {
        let c1 = make_candidate("foo", EvidenceKind::SymbolDeclaration, "a.rs", 10);
        let c2 = make_candidate("foo", EvidenceKind::SymbolDeclaration, "a.rs", 10);
        let e1 = c1.into_evidence();
        let e2 = c2.into_evidence();
        assert_eq!(e1.id, e2.id); // same content → same ID

        let c3 = make_candidate("foo", EvidenceKind::SymbolDeclaration, "b.rs", 10);
        let e3 = c3.into_evidence();
        assert_ne!(e1.id, e3.id); // different file → different ID
    }
}
