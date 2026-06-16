//! Evidence weights and composition for scope-based resolution.
//!
//! Modeled after GitNexusRelay's:
//! - `gitnexus-shared/src/scope-resolution/evidence-weights.ts`
//! - `gitnexus-shared/src/scope-resolution/registries/evidence.ts`
//!
//! Every `ResolutionEvidence.weight` value MUST reference `EvidenceWeights`;
//! inline magic numbers are a lint violation. Evidence composes additively;
//! the sum is capped at 1.0.

use serde::{Serialize, Deserialize};

// ── Evidence weights (RFC Appendix A — authoritative values) ────────────────

/// Authoritative weight map for scope-based resolution evidence.
///
/// Starting calibration for scope-based resolution. Shadow-first rollout
/// tunes these against legacy DAG parity.
pub struct EvidenceWeights;

impl EvidenceWeights {
    // ── Where-found signals (visibility) ──────────────────────────────────
    /// `BindingRef.origin === 'local'`
    pub const LOCAL: f64 = 0.55;
    /// `BindingRef.origin === 'import'`
    pub const IMPORT: f64 = 0.45;
    /// `BindingRef.origin === 'reexport'`
    pub const REEXPORT: f64 = 0.40;
    /// `BindingRef.origin === 'namespace'`
    pub const NAMESPACE: f64 = 0.40;
    /// `BindingRef.origin === 'wildcard'`
    pub const WILDCARD: f64 = 0.30;

    // ── Scope-chain deduction (per-hop) ───────────────────────────────────
    /// Deducted per parent-hop taken (depth-0 = 0, depth-1 = −0.02, …)
    pub const SCOPE_CHAIN_PER_DEPTH: f64 = -0.02;

    // ── Receiver-type-binding signal (decays by MRO depth) ────────────────
    /// Weight applied when receiver's type binding resolves to a class that
    /// declares the candidate. Decays by MRO depth.
    pub const TYPE_BINDING_BY_MRO_DEPTH: [f64; 5] = [0.50, 0.42, 0.36, 0.32, 0.30];

    // ── Corroborating signals ──────────────────────────────────────────────
    /// `def.ownerId === resolvedReceiver.def.id` (exact owner match)
    pub const OWNER_MATCH: f64 = 0.20;
    /// Explanatory only — retained for debuggability. Never discriminates.
    pub const KIND_MATCH: f64 = 0.00;

    // ── Arity compatibility ────────────────────────────────────────────────
    pub const ARITY_COMPATIBLE: f64 = 0.10;
    pub const ARITY_UNKNOWN: f64 = 0.00;
    pub const ARITY_INCOMPATIBLE: f64 = -0.15;

    // ── Global fallback ────────────────────────────────────────────────────
    /// Hit via `QualifiedNameIndex.byQualifiedName`
    pub const GLOBAL_QUALIFIED: f64 = 0.35;
    /// Fallback hit in a `byName` index
    pub const GLOBAL_NAME: f64 = 0.10;

    // ── Degraded signals ───────────────────────────────────────────────────
    /// Call/reference through a `dynamic-unresolved` edge
    pub const DYNAMIC_IMPORT_UNRESOLVED: f64 = 0.02;

    // ── Unresolved-import cap ──────────────────────────────────────────────
    /// Multiplicative cap on edge-derived evidence when `ImportEdge.linkStatus === 'unresolved'`
    pub const UNLINKED_IMPORT_MULTIPLIER: f64 = 0.50;
}

/// Look up the type-binding signal weight for a given MRO depth,
/// falling back to the last tabulated value for depths beyond the table.
pub fn type_binding_weight_at_depth(mro_depth: i32) -> f64 {
    let table = EvidenceWeights::TYPE_BINDING_BY_MRO_DEPTH;
    if mro_depth < 0 { return table[0]; }
    let idx = mro_depth as usize;
    if idx >= table.len() { return table[table.len() - 1]; }
    table[idx]
}

// ── Resolution evidence ─────────────────────────────────────────────────────

/// One piece of evidence for a `Resolution`. Multiple signals corroborate
/// a single match; their weights compose additively to produce `confidence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolutionEvidence {
    pub kind: EvidenceKind,
    /// Signal weight, sourced from `EvidenceWeights`. Additive; sum capped at 1.0.
    pub weight: f64,
    /// Optional debug annotation.
    pub note: Option<String>,
}

/// Kinds of evidence that can contribute to a resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceKind {
    /// Found as a local binding
    Local,
    /// Found via scope-chain walk
    ScopeChain,
    /// Found via import
    Import,
    /// Found via receiver type-binding MRO walk
    TypeBinding,
    /// Owner matches receiver
    OwnerMatch,
    /// Kind matches (explanatory, weight 0)
    KindMatch,
    /// Arity is compatible
    ArityMatch,
    /// Found via global qualified name index
    GlobalQualified,
    /// Found via global name index
    GlobalName,
    /// Through a dynamic-unresolved import
    DynamicImportUnresolved,
}

// ── Raw signals ──────────────────────────────────────────────────────────────

/// Raw signals observed for a single candidate during the 7-step walk.
/// Optional fields encode "this signal did not fire".
#[derive(Debug, Clone, Default)]
pub struct RawSignals {
    /// Visibility origin of the binding that produced this candidate.
    pub origin: Option<BindingOrigin>,
    /// Depth at which the binding was found (hops up from start scope).
    pub scope_chain_depth: Option<usize>,
    /// Whether the import edge that brought this name is unresolved.
    pub via_unlinked_import: bool,
    /// MRO depth when candidate came via receiver's type-binding walk.
    pub type_binding_mro_depth: Option<i32>,
    /// `def.ownerId === resolvedReceiver.def.nodeId`
    pub owner_match: bool,
    /// Always true for candidates that pass `accepted_kinds`.
    pub kind_match: bool,
    /// Arity compatibility verdict.
    pub arity_verdict: Option<ArityVerdict>,
    /// Candidate flows through a `dynamic-unresolved` ImportEdge.
    pub dynamic_unresolved: bool,
}

/// Binding visibility origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BindingOrigin {
    #[default]
    Local,
    Import,
    Reexport,
    Namespace,
    Wildcard,
    /// Found via global qualified name index
    GlobalQualified,
    /// Found via global name index
    GlobalName,
}

/// Arity compatibility verdict from the language provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArityVerdict {
    Compatible,
    Unknown,
    Incompatible,
}

// ── Evidence composition ────────────────────────────────────────────────────

/// Translate accumulated raw signals into a `ResolutionEvidence[]`.
///
/// Emission order mirrors the `EvidenceWeights` layout:
/// where-found → type-binding → corroborators → arity → degraded.
pub fn compose_evidence(signals: &RawSignals) -> Vec<ResolutionEvidence> {
    let mut out = Vec::new();

    // ── Where-found visibility ─────────────────────────────────────────
    if let Some(origin) = signals.origin {
        let base_weight = match origin {
            BindingOrigin::Local => EvidenceWeights::LOCAL,
            BindingOrigin::Import => EvidenceWeights::IMPORT,
            BindingOrigin::Reexport => EvidenceWeights::REEXPORT,
            BindingOrigin::Namespace => EvidenceWeights::NAMESPACE,
            BindingOrigin::Wildcard => EvidenceWeights::WILDCARD,
            BindingOrigin::GlobalQualified => EvidenceWeights::GLOBAL_QUALIFIED,
            BindingOrigin::GlobalName => EvidenceWeights::GLOBAL_NAME,
        };
        let capped = if signals.via_unlinked_import {
            base_weight * EvidenceWeights::UNLINKED_IMPORT_MULTIPLIER
        } else {
            base_weight
        };
        let kind = match origin {
            BindingOrigin::Local => EvidenceKind::Local,
            BindingOrigin::Import | BindingOrigin::Reexport | BindingOrigin::Namespace | BindingOrigin::Wildcard => EvidenceKind::Import,
            BindingOrigin::GlobalQualified => EvidenceKind::GlobalQualified,
            BindingOrigin::GlobalName => EvidenceKind::GlobalName,
        };
        let note = if signals.via_unlinked_import {
            Some(format!("via unresolved import ({}× cap)", EvidenceWeights::UNLINKED_IMPORT_MULTIPLIER))
        } else {
            None
        };
        out.push(ResolutionEvidence { kind, weight: capped, note });
    }

    // ── Scope-chain depth deduction ────────────────────────────────────
    if let Some(depth) = signals.scope_chain_depth {
        if depth > 0 {
            out.push(ResolutionEvidence {
                kind: EvidenceKind::ScopeChain,
                weight: EvidenceWeights::SCOPE_CHAIN_PER_DEPTH * depth as f64,
                note: Some(format!("depth={}", depth)),
            });
        }
    }

    // ── Type-binding / MRO path ────────────────────────────────────────
    if let Some(mro_depth) = signals.type_binding_mro_depth {
        out.push(ResolutionEvidence {
            kind: EvidenceKind::TypeBinding,
            weight: type_binding_weight_at_depth(mro_depth),
            note: Some(format!("mroDepth={}", mro_depth)),
        });
    }

    // ── Owner match ────────────────────────────────────────────────────
    if signals.owner_match {
        out.push(ResolutionEvidence {
            kind: EvidenceKind::OwnerMatch,
            weight: EvidenceWeights::OWNER_MATCH,
            note: None,
        });
    }

    // ── Kind match (always present; weight 0; retained for debug) ──────
    out.push(ResolutionEvidence {
        kind: EvidenceKind::KindMatch,
        weight: EvidenceWeights::KIND_MATCH,
        note: None,
    });

    // ── Arity ──────────────────────────────────────────────────────────
    if let Some(verdict) = signals.arity_verdict {
        let (weight, note) = match verdict {
            ArityVerdict::Compatible => (EvidenceWeights::ARITY_COMPATIBLE, "compatible"),
            ArityVerdict::Unknown => (EvidenceWeights::ARITY_UNKNOWN, "unknown"),
            ArityVerdict::Incompatible => (EvidenceWeights::ARITY_INCOMPATIBLE, "incompatible"),
        };
        out.push(ResolutionEvidence {
            kind: EvidenceKind::ArityMatch,
            weight,
            note: Some(note.to_string()),
        });
    }

    // ── Dynamic-unresolved ─────────────────────────────────────────────
    if signals.dynamic_unresolved {
        out.push(ResolutionEvidence {
            kind: EvidenceKind::DynamicImportUnresolved,
            weight: EvidenceWeights::DYNAMIC_IMPORT_UNRESOLVED,
            note: None,
        });
    }

    out
}

/// Sum evidence weights and clamp to `[0, 1]`.
pub fn confidence_from_evidence(evidence: &[ResolutionEvidence]) -> f64 {
    let sum: f64 = evidence.iter().map(|e| e.weight).sum();
    sum.clamp(0.0, 1.0)
}

// ── Resolution ──────────────────────────────────────────────────────────────

/// A ranked resolution candidate returned by registry lookups.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Resolution {
    /// The symbol definition that was resolved.
    pub def_id: String,
    /// Σ of `evidence[].weight`, capped at 1.0.
    pub confidence: f64,
    /// Per-signal evidence trace.
    pub evidence: Vec<ResolutionEvidence>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_type_binding_weight_at_depth() {
        assert_eq!(type_binding_weight_at_depth(0), 0.50);
        assert_eq!(type_binding_weight_at_depth(1), 0.42);
        assert_eq!(type_binding_weight_at_depth(4), 0.30);
        // Beyond table — falls back to last value
        assert_eq!(type_binding_weight_at_depth(10), 0.30);
        // Negative — falls back to first value
        assert_eq!(type_binding_weight_at_depth(-1), 0.50);
    }

    #[test]
    fn test_compose_evidence_local() {
        let signals = RawSignals {
            origin: Some(BindingOrigin::Local),
            kind_match: true,
            ..Default::default()
        };
        let evidence = compose_evidence(&signals);
        assert_eq!(evidence.len(), 2); // local + kind_match
        assert_eq!(evidence[0].kind, EvidenceKind::Local);
        assert!((evidence[0].weight - 0.55).abs() < 0.001);
        assert_eq!(evidence[1].kind, EvidenceKind::KindMatch);
        assert_eq!(evidence[1].weight, 0.0);
    }

    #[test]
    fn test_compose_evidence_import_unlinked() {
        let signals = RawSignals {
            origin: Some(BindingOrigin::Import),
            via_unlinked_import: true,
            kind_match: true,
            ..Default::default()
        };
        let evidence = compose_evidence(&signals);
        assert_eq!(evidence[0].kind, EvidenceKind::Import);
        // 0.45 * 0.5 = 0.225
        assert!((evidence[0].weight - 0.225).abs() < 0.001);
        assert!(evidence[0].note.as_ref().unwrap().contains("unresolved"));
    }

    #[test]
    fn test_compose_evidence_scope_chain() {
        let signals = RawSignals {
            origin: Some(BindingOrigin::Local),
            scope_chain_depth: Some(2),
            kind_match: true,
            ..Default::default()
        };
        let evidence = compose_evidence(&signals);
        let scope_chain = evidence.iter().find(|e| e.kind == EvidenceKind::ScopeChain).unwrap();
        assert!((scope_chain.weight - (-0.04)).abs() < 0.001);
    }

    #[test]
    fn test_compose_evidence_owner_match() {
        let signals = RawSignals {
            origin: Some(BindingOrigin::Local),
            owner_match: true,
            kind_match: true,
            ..Default::default()
        };
        let evidence = compose_evidence(&signals);
        let owner = evidence.iter().find(|e| e.kind == EvidenceKind::OwnerMatch).unwrap();
        assert!((owner.weight - 0.20).abs() < 0.001);
    }

    #[test]
    fn test_compose_evidence_arity() {
        let signals = RawSignals {
            origin: Some(BindingOrigin::Local),
            arity_verdict: Some(ArityVerdict::Compatible),
            kind_match: true,
            ..Default::default()
        };
        let evidence = compose_evidence(&signals);
        let arity = evidence.iter().find(|e| e.kind == EvidenceKind::ArityMatch).unwrap();
        assert!((arity.weight - 0.10).abs() < 0.001);
    }

    #[test]
    fn test_compose_evidence_dynamic_unresolved() {
        let signals = RawSignals {
            origin: Some(BindingOrigin::Local),
            dynamic_unresolved: true,
            kind_match: true,
            ..Default::default()
        };
        let evidence = compose_evidence(&signals);
        let dyn_ev = evidence.iter().find(|e| e.kind == EvidenceKind::DynamicImportUnresolved).unwrap();
        assert!((dyn_ev.weight - 0.02).abs() < 0.001);
    }

    #[test]
    fn test_confidence_from_evidence() {
        let evidence = vec![
            ResolutionEvidence { kind: EvidenceKind::Local, weight: 0.55, note: None },
            ResolutionEvidence { kind: EvidenceKind::OwnerMatch, weight: 0.20, note: None },
            ResolutionEvidence { kind: EvidenceKind::KindMatch, weight: 0.00, note: None },
        ];
        assert!((confidence_from_evidence(&evidence) - 0.75).abs() < 0.001);
    }

    #[test]
    fn test_confidence_clamped_to_1() {
        let evidence = vec![
            ResolutionEvidence { kind: EvidenceKind::Local, weight: 0.55, note: None },
            ResolutionEvidence { kind: EvidenceKind::Import, weight: 0.45, note: None },
            ResolutionEvidence { kind: EvidenceKind::OwnerMatch, weight: 0.20, note: None },
        ];
        assert_eq!(confidence_from_evidence(&evidence), 1.0);
    }

    #[test]
    fn test_confidence_clamped_to_0() {
        let evidence = vec![
            ResolutionEvidence { kind: EvidenceKind::ArityMatch, weight: -0.15, note: None },
            ResolutionEvidence { kind: EvidenceKind::ScopeChain, weight: -0.10, note: None },
        ];
        assert_eq!(confidence_from_evidence(&evidence), 0.0);
    }

    #[test]
    fn test_compose_evidence_full_signal_set() {
        let signals = RawSignals {
            origin: Some(BindingOrigin::Import),
            scope_chain_depth: Some(1),
            type_binding_mro_depth: Some(0),
            owner_match: true,
            kind_match: true,
            arity_verdict: Some(ArityVerdict::Compatible),
            via_unlinked_import: false,
            dynamic_unresolved: false,
        };
        let evidence = compose_evidence(&signals);
        // import + scope-chain + type-binding + owner + kind + arity = 6
        assert_eq!(evidence.len(), 6);
        let conf = confidence_from_evidence(&evidence);
        assert!(conf > 0.0 && conf <= 1.0);
    }
}
