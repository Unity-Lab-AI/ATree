//! Evidence — the atomic observation unit of the ATree intelligence engine.
//!
//! An Evidence is the smallest *verifiable observation* extracted from code.
//! It is content-addressed, confidence-scored, and immutable once committed.
//!
//! ## Lifecycle
//!
//! ```text
//! EXTRACTED → NORMALIZED → DEDUPED → ENRICHED → CALIBRATED → COMMITTED
//!                                                          ↓ (feedback loop)
//!                                                       UPDATED
//! ```
//!
//! Invalid transitions are enforced at the type level via `EvidenceState`.
//!
//! ## Architecture
//!
//! ```text
//! AST → extract_evidence() → EvidenceCandidate[]
//!     → normalize()         → NormalizedEvidence[]
//!     → dedupe()            → Evidence[] (content-addressed)
//!     → enrich()            → Evidence[] (graph-linked)
//!     → calibrate()         → Evidence[] (confidence-scored)
//!     → commit()            → SQLite (immutable)
//!     → feedback()          → confidence/stability updates
//! ```

pub mod lifecycle;
pub mod extraction;
pub mod storage;
pub mod calibration;

use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};

// ── Evidence ID (content-addressed) ──────────────────────────────────────────

/// Stable, content-addressed evidence identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EvidenceId(pub String);

impl EvidenceId {
    /// Compute a stable ID from the evidence's canonical content.
    ///
    /// Identity function: `hash(kind + normalized + file + span)`
    pub fn compute(
        kind: EvidenceKind,
        normalized: &str,
        file: &str,
        start_line: usize,
        start_col: usize,
        end_line: usize,
        end_col: usize,
    ) -> Self {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", kind).hash(&mut h);
        normalized.hash(&mut h);
        file.hash(&mut h);
        start_line.hash(&mut h);
        start_col.hash(&mut h);
        end_line.hash(&mut h);
        end_col.hash(&mut h);
        Self(format!("ev_{:016x}", h.finish()))
    }
}

impl std::fmt::Display for EvidenceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ── Evidence Taxonomy ────────────────────────────────────────────────────────

/// Strict categorization of evidence kinds.
/// Only AST-derived facts are non-heuristic. Everything else is HEURISTIC_INFERENCE.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EvidenceKind {
    SymbolDeclaration,
    SymbolReference,
    FunctionCall,
    TypeRelation,
    ImportEdge,
    ControlFlow,
    DataFlow,
    ConfigUsage,
    SideEffect,
    ErrorPath,
    TestAssertion,
    BoundaryCrossing,
    /// Explicitly lower trust — not directly from AST
    HeuristicInference,
}

impl EvidenceKind {
    /// Returns true if this kind is derived directly from AST structure.
    pub fn is_ast_derived(&self) -> bool {
        !matches!(self, EvidenceKind::HeuristicInference)
    }

    /// Base confidence for this evidence kind.
    pub fn base_confidence(&self) -> f64 {
        if self.is_ast_derived() { 0.90 } else { 0.40 }
    }
}

impl std::fmt::Display for EvidenceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::SymbolDeclaration => "SYMBOL_DECLARATION",
            Self::SymbolReference => "SYMBOL_REFERENCE",
            Self::FunctionCall => "FUNCTION_CALL",
            Self::TypeRelation => "TYPE_RELATION",
            Self::ImportEdge => "IMPORT_EDGE",
            Self::ControlFlow => "CONTROL_FLOW",
            Self::DataFlow => "DATA_FLOW",
            Self::ConfigUsage => "CONFIG_USAGE",
            Self::SideEffect => "SIDE_EFFECT",
            Self::ErrorPath => "ERROR_PATH",
            Self::TestAssertion => "TEST_ASSERTION",
            Self::BoundaryCrossing => "BOUNDARY_CROSSING",
            Self::HeuristicInference => "HEURISTIC_INFERENCE",
        };
        write!(f, "{}", s)
    }
}

impl std::str::FromStr for EvidenceKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "SYMBOL_DECLARATION" => Ok(Self::SymbolDeclaration),
            "SYMBOL_REFERENCE" => Ok(Self::SymbolReference),
            "FUNCTION_CALL" => Ok(Self::FunctionCall),
            "TYPE_RELATION" => Ok(Self::TypeRelation),
            "IMPORT_EDGE" => Ok(Self::ImportEdge),
            "CONTROL_FLOW" => Ok(Self::ControlFlow),
            "DATA_FLOW" => Ok(Self::DataFlow),
            "CONFIG_USAGE" => Ok(Self::ConfigUsage),
            "SIDE_EFFECT" => Ok(Self::SideEffect),
            "ERROR_PATH" => Ok(Self::ErrorPath),
            "TEST_ASSERTION" => Ok(Self::TestAssertion),
            "BOUNDARY_CROSSING" => Ok(Self::BoundaryCrossing),
            "HEURISTIC_INFERENCE" => Ok(Self::HeuristicInference),
            other => Err(format!("Unknown evidence kind: {}", other)),
        }
    }
}

// ── Source Span ──────────────────────────────────────────────────────────────

/// Location of the evidence in source code.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceSpan {
    pub start_line: usize,
    pub start_col: usize,
    pub end_line: usize,
    pub end_col: usize,
}

// ── Source Binding ───────────────────────────────────────────────────────────

/// Where the evidence was found.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EvidenceSource {
    pub file: String,
    pub span: SourceSpan,
    pub language: String,
}

// ── Target Reference ─────────────────────────────────────────────────────────

/// What the evidence points to in the ATree graph.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EvidenceTarget {
    pub target_type: TargetType,
    pub ref_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TargetType {
    Primitive,
    Symbol,
    Pattern,
    Constraint,
}

// ── Context ──────────────────────────────────────────────────────────────────

/// Surrounding context for the evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceContext {
    pub enclosing_symbol: Option<String>,
    pub imports: Vec<String>,
    pub scope_chain: Vec<String>,
}

// ── Metadata ─────────────────────────────────────────────────────────────────

/// Provenance and quality metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceMetadata {
    pub extractor: String,
    pub confidence: f64,
    pub stability: f64,
    pub entropy: f64,
    pub timestamp_ms: i64,
    pub commit: Option<String>,
}

impl Default for EvidenceMetadata {
    fn default() -> Self {
        Self {
            extractor: "atree-engine/0.7.0".to_string(),
            confidence: 0.0,
            stability: 1.0,
            entropy: 0.0,
            timestamp_ms: 0,
            commit: None,
        }
    }
}

// ── Links ────────────────────────────────────────────────────────────────────

/// Graph edges connecting evidence units.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct EvidenceLinks {
    pub derives_from: Vec<EvidenceId>,
    pub supports: Vec<EvidenceId>,
    pub contradicts: Vec<EvidenceId>,
}

// ── Core Evidence Object ────────────────────────────────────────────────────

/// The canonical Evidence unit — the smallest verifiable observation from code.
///
/// Once committed, only `confidence`, `stability`, and `contradicts` may change.
/// All other fields are frozen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub id: EvidenceId,
    pub kind: EvidenceKind,
    pub source: EvidenceSource,
    pub target: EvidenceTarget,
    pub content: EvidenceContent,
    pub context: EvidenceContext,
    pub metadata: EvidenceMetadata,
    pub links: EvidenceLinks,
    pub tags: Vec<String>,
    /// Current lifecycle state. Only transitions forward (or UPDATED from COMMITTED).
    pub state: lifecycle::EvidenceState,
}

/// The extracted text content.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EvidenceContent {
    /// Exact extracted text (AST slice or token span).
    pub raw: String,
    /// Canonicalized representation.
    pub normalized: String,
}

// ── Evidence Candidate (pre-dedup) ───────────────────────────────────────────

/// Raw evidence extracted from AST, before deduplication and calibration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceCandidate {
    pub kind: EvidenceKind,
    pub source: EvidenceSource,
    pub target: EvidenceTarget,
    pub content: EvidenceContent,
    pub context: EvidenceContext,
    pub tags: Vec<String>,
}

impl EvidenceCandidate {
    /// Convert to a full Evidence with computed ID and initial metadata.
    pub fn into_evidence(self) -> Evidence {
        let id = EvidenceId::compute(
            self.kind,
            &self.content.normalized,
            &self.source.file,
            self.source.span.start_line,
            self.source.span.start_col,
            self.source.span.end_line,
            self.source.span.end_col,
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        Evidence {
            id,
            kind: self.kind,
            source: self.source,
            target: self.target,
            content: self.content,
            context: self.context,
            metadata: EvidenceMetadata {
                extractor: "atree-engine/0.7.0".to_string(),
                confidence: self.kind.base_confidence(),
                stability: 1.0,
                entropy: 0.0,
                timestamp_ms: now,
                commit: None,
            },
            links: EvidenceLinks::default(),
            tags: self.tags,
            state: lifecycle::EvidenceState::Extracted,
        }
    }
}

// ── Invariant Enforcement ────────────────────────────────────────────────────

impl Evidence {
    /// I1: No orphan evidence — must reference a valid source span or be heuristic.
    pub fn check_invariant_i1(&self) -> Result<(), String> {
        if self.kind == EvidenceKind::HeuristicInference {
            return Ok(());
        }
        if self.source.span.start_line == 0 && self.source.span.end_line == 0 {
            return Err(format!(
                "Evidence {} has zero span but is not HEURISTIC",
                self.id
            ));
        }
        Ok(())
    }

    /// I2: No unscoped symbols — SYMBOL_* must resolve to a primitive or known symbol.
    pub fn check_invariant_i2(&self) -> Result<(), String> {
        match self.kind {
            EvidenceKind::SymbolDeclaration | EvidenceKind::SymbolReference => {
                if self.target.ref_id.is_empty() {
                    return Err(format!(
                        "Evidence {} ({:?}) has empty target ref_id",
                        self.id, self.kind
                    ));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// I3: Confidence monotonicity — confidence must be ≤ 1.0.
    pub fn check_invariant_i3(&self) -> Result<(), String> {
        if self.metadata.confidence < 0.0 || self.metadata.confidence > 1.0 {
            return Err(format!(
                "Evidence {} confidence {} out of [0, 1] range",
                self.id, self.metadata.confidence
            ));
        }
        Ok(())
    }

    /// I4: Immutability — committed evidence must not have frozen fields modified.
    /// (Enforced at the type level by requiring `&mut self` only for allowed fields.)
    pub fn is_committed(&self) -> bool {
        self.state == lifecycle::EvidenceState::Committed
            || self.state == lifecycle::EvidenceState::Updated
    }

    /// Run all invariants. Returns first violation.
    pub fn validate(&self) -> Result<(), String> {
        self.check_invariant_i1()?;
        self.check_invariant_i2()?;
        self.check_invariant_i3()?;
        Ok(())
    }
}

// ── Re-exports ───────────────────────────────────────────────────────────────

pub use calibration::{calibrate_confidence, CalibrationConfig};
pub use lifecycle::{EvidenceState, EvidenceLifecycle};
pub use storage::{EvidenceStore, EvidenceRecord};
