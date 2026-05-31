//! Evidence Confidence Calibration.
//!
//! Formula:
//! ```text
//! confidence = AST_weight × resolution_success × stability_factor × (1 - entropy_penalty)
//! ```
//!
//! Rules:
//! - AST-derived: base 0.9–1.0
//! - Heuristic: capped at 0.6 unless reinforced
//! - Unresolved symbols: decay by 0.3–0.5
//! - Downstream confidence ≤ upstream confidence (I3)

use crate::evidence::Evidence;

/// Configuration for confidence calibration.
#[derive(Debug, Clone)]
pub struct CalibrationConfig {
    /// Base confidence for AST-derived evidence.
    pub ast_base: f64,
    /// Base confidence for heuristic evidence.
    pub heuristic_base: f64,
    /// Decay factor for unresolved symbols.
    pub unresolved_decay: f64,
    /// Maximum confidence for heuristic evidence.
    pub heuristic_cap: f64,
    /// Entropy penalty weight.
    pub entropy_weight: f64,
}

impl Default for CalibrationConfig {
    fn default() -> Self {
        Self {
            ast_base: 0.90,
            heuristic_base: 0.40,
            unresolved_decay: 0.35,
            heuristic_cap: 0.60,
            entropy_weight: 0.15,
        }
    }
}

/// Calibrate a single evidence's confidence based on its kind, resolution status,
/// stability, and entropy.
pub fn calibrate_confidence(
    evidence: &mut Evidence,
    resolved: bool,
    config: &CalibrationConfig,
) {
    // Step 1: Base weight from kind
    let base = if evidence.kind.is_ast_derived() {
        config.ast_base
    } else {
        config.heuristic_base
    };

    // Step 2: Resolution success multiplier
    let resolution_factor = if resolved { 1.0 } else { 1.0 - config.unresolved_decay };

    // Step 3: Stability factor (evidence that appeared in multiple runs scores higher)
    let stability_factor = evidence.metadata.stability;

    // Step 4: Entropy penalty (noisy observations score lower)
    let entropy_penalty = evidence.metadata.entropy * config.entropy_weight;

    // Compute final confidence
    let mut confidence = base * resolution_factor * stability_factor * (1.0 - entropy_penalty);

    // Heuristic cap
    if !evidence.kind.is_ast_derived() {
        confidence = confidence.min(config.heuristic_cap);
    }

    // Clamp to [0, 1]
    evidence.metadata.confidence = confidence.clamp(0.0, 1.0);
}

/// Calibrate all evidence in a collection.
pub fn calibrate_all(
    evidence: &mut [Evidence],
    resolutions: &std::collections::HashMap<String, bool>,
    config: &CalibrationConfig,
) -> usize {
    let mut calibrated = 0;
    for ev in evidence.iter_mut() {
        let resolved = resolutions
            .get(&ev.target.ref_id)
            .copied()
            .unwrap_or(false);
        calibrate_confidence(ev, resolved, config);
        calibrated += 1;
    }
    calibrated
}

/// Compute entropy for an evidence unit based on text variability.
/// Lower entropy = more precise = higher confidence.
pub fn compute_entropy(raw: &str) -> f64 {
    if raw.is_empty() {
        return 1.0;
    }
    let mut freq = std::collections::HashMap::new();
    for c in raw.chars() {
        *freq.entry(c).or_insert(0u32) += 1;
    }
    let len = raw.len() as f64;
    let mut entropy = 0.0f64;
    for &count in freq.values() {
        let p = count as f64 / len;
        if p > 0.0 {
            entropy -= p * p.log2();
        }
    }
    // Normalize to [0, 1] by dividing by max possible entropy (log2 of alphabet size)
    let max_entropy = (freq.len() as f64).max(1.0).log2();
    if max_entropy > 0.0 {
        entropy / max_entropy
    } else {
        0.0
    }
}

/// Update stability based on how many times this evidence was observed across runs.
pub fn update_stability(current: f64, observed: bool, alpha: f64) -> f64 {
    // Exponential moving average
    if observed {
        current + alpha * (1.0 - current)
    } else {
        current * (1.0 - alpha)
    }
}

/// Enforce I3: confidence monotonicity for derived knowledge.
/// If `derived` was computed from `source`, then:
/// `derived.confidence` must be ≤ `source.confidence`.
pub fn enforce_monotonicity(source: &Evidence, derived: &mut Evidence) {
    if derived.metadata.confidence > source.metadata.confidence {
        derived.metadata.confidence = source.metadata.confidence;
    }
}
