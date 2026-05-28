//! Composite scoring for best-practice candidate methods.
//!
//! The agent applies this formula when running `discover_*` tasks against
//! `policies/best-practice-scoring-policy.json`. This module provides the
//! deterministic Rust implementation so tooling, tests, and the agent
//! prompt all describe the same algorithm.
//!
//! # Axis model
//!
//! Five axes, each scored in [0.0, 5.0]:
//! - `default_suitability` (0.30) — baseline fitness of the method for the stage class.
//! - `robustness`          (0.20) — reproducibility + failure-mode hygiene.
//! - `adoption`            (0.10) — ecosystem maturity + community uptake.
//! - `operational_fit`     (0.10) — runtime cost + infrastructure compatibility.
//! - `spec_match`          (0.30) — boost when the candidate appears in
//!   `task.spec.spec_preferred_methods`.
//!
//! # Renormalization
//!
//! When an axis listed in `renormalizeWhenAxisMissing` has **no input
//! source** (e.g., `spec_match` when `task.spec.spec_preferred_methods` is
//! empty), its weight is redistributed proportionally among the remaining
//! axes so the effective weights still sum to 1.0.
//!
//! "Missing" means no input can produce a non-zero value for the axis —
//! NOT that the axis value is 0.0 after scoring. A candidate that scores
//! 0.0 on robustness IS scored; the weight stays at 0.20. A candidate
//! where specMatch cannot fire (empty `spec_preferred_methods`) is the
//! missing case.
//!
//! # Quality-gate penalties and default eligibility
//!
//! After computing the composite score (including renormalization and
//! specMatch boost), the agent applies [`apply_quality_gate_penalties`] to
//! subtract per-gate deltas, then checks [`check_default_eligibility`] to
//! determine whether the candidate may appear in the `defaultRecommended`
//! tier. Ineligible candidates may still appear as `tentative` or
//! `alternative`. Both functions are testable reference implementations;
//! the LLM agent enforces the same rules at runtime.

use serde::{Deserialize, Serialize};

/// Per-axis scores in [0.0, 5.0].
#[derive(Debug, Clone, PartialEq)]
pub struct AxisScores {
    pub default_suitability: f64,
    pub robustness: f64,
    pub adoption: f64,
    pub operational_fit: f64,
    /// `None` when the specMatch axis has no input source (empty
    /// `spec_preferred_methods`). `Some(v)` when the axis was scored —
    /// even if `v == 0.0` (candidate is not in the preferred list, but
    /// the list itself is non-empty).
    pub spec_match: Option<f64>,
}

/// Policy weight configuration parsed from `compositeScoreWeights`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CompositeScoreWeights {
    pub default_suitability: f64,
    pub robustness: f64,
    pub adoption: f64,
    pub operational_fit: f64,
    pub spec_match: f64,
    /// Axes in this list have their weight redistributed when the axis
    /// has no input source (see module-level docs). Absent or empty →
    /// literal multiplication (backward-compatible behaviour).
    #[serde(default)]
    pub renormalize_when_axis_missing: Vec<String>,
}

impl Default for CompositeScoreWeights {
    fn default() -> Self {
        Self {
            default_suitability: 0.30,
            robustness: 0.20,
            adoption: 0.10,
            operational_fit: 0.10,
            spec_match: 0.30,
            renormalize_when_axis_missing: vec!["specMatch".to_string()],
        }
    }
}

/// Compute the composite score for one candidate, applying renormalization
/// when an axis in `weights.renormalize_when_axis_missing` is missing.
///
/// # Returns
/// A value in [0.0, 5.0].
///
/// # Renormalization contract
/// If `scores.spec_match` is `None` AND `"specMatch"` is listed in
/// `weights.renormalize_when_axis_missing`, the four remaining weights are
/// scaled so they sum to 1.0 before multiplication. If
/// `renormalize_when_axis_missing` is absent or empty the literal
/// multiplication is applied — `scores.spec_match` defaults to 0.0 and
/// the specMatch weight is effectively lost (preserving the prior
/// behaviour for callers that don't populate the field).
pub fn compute_composite_score(scores: &AxisScores, weights: &CompositeScoreWeights) -> f64 {
    let should_renorm_spec_match = scores.spec_match.is_none()
        && weights
            .renormalize_when_axis_missing
            .iter()
            .any(|a| a == "specMatch");

    if should_renorm_spec_match {
        // Redistribute specMatch weight proportionally across the remaining 4 axes.
        let remaining_weight = weights.default_suitability
            + weights.robustness
            + weights.adoption
            + weights.operational_fit;
        if remaining_weight == 0.0 {
            return 0.0;
        }
        let ds_eff = weights.default_suitability / remaining_weight;
        let rb_eff = weights.robustness / remaining_weight;
        let ad_eff = weights.adoption / remaining_weight;
        let of_eff = weights.operational_fit / remaining_weight;

        ds_eff * scores.default_suitability
            + rb_eff * scores.robustness
            + ad_eff * scores.adoption
            + of_eff * scores.operational_fit
    } else {
        // Literal multiplication. spec_match defaults to 0.0 when None.
        let sm = scores.spec_match.unwrap_or(0.0);
        weights.default_suitability * scores.default_suitability
            + weights.robustness * scores.robustness
            + weights.adoption * scores.adoption
            + weights.operational_fit * scores.operational_fit
            + weights.spec_match * sm
    }
}

// ---------------------------------------------------------------------------
// Quality-gate penalties
// ---------------------------------------------------------------------------

/// Penalty amounts parsed from `qualityGatePenalties` in the policy JSON.
///
/// Both values are negative in the policy (e.g. `blocking: -1.0`). The
/// functions in this module preserve their sign: callers subtract the
/// magnitude, or equivalently *add* the (negative) penalty to the score.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QualityGatePenalties {
    /// Score delta per blocking gate failure (should be ≤ 0.0).
    pub blocking: f64,
    /// Score delta per non-blocking gate failure (should be ≤ 0.0).
    pub non_blocking: f64,
}

impl Default for QualityGatePenalties {
    fn default() -> Self {
        Self {
            blocking: -1.0,
            non_blocking: -0.25,
        }
    }
}

/// Severity classification for a single quality-gate evaluation result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateSeverity {
    Blocking,
    NonBlocking,
}

/// Result of evaluating one quality gate for a candidate.
#[derive(Debug, Clone)]
pub struct QualityGateResult {
    /// Stable identifier for this gate (e.g. `"reproducibility_check"`).
    pub gate_id: String,
    /// Whether the candidate passed this gate.
    pub passed: bool,
    /// Severity when the gate is failed.
    pub severity: GateSeverity,
}

/// Apply `qualityGatePenalties` to a composite score.
///
/// For each failed gate, adds the corresponding delta (a negative value) to
/// `base_score`. Penalties stack additively. The returned score is NOT
/// clamped here — callers apply `finalScoreClamp` from the policy.
///
/// When `gates` is empty or all gates pass, `base_score` is returned unchanged.
///
/// # Backward compatibility
/// If the caller has no quality-gate metadata (e.g. an older discovery task),
/// pass an empty slice; this function is a no-op in that case.
pub fn apply_quality_gate_penalties(
    base_score: f64,
    gates: &[QualityGateResult],
    policy: &QualityGatePenalties,
) -> f64 {
    let mut score = base_score;
    for gate in gates {
        if !gate.passed {
            score += match gate.severity {
                GateSeverity::Blocking => policy.blocking,
                GateSeverity::NonBlocking => policy.non_blocking,
            };
        }
    }
    score
}

// ---------------------------------------------------------------------------
// Default eligibility
// ---------------------------------------------------------------------------

/// Candidate metadata consumed by [`check_default_eligibility`].
///
/// All fields have safe defaults so callers only need to populate the fields
/// they have. An all-default candidate passes every criterion that isn't
/// about blocking gates or contradictions (which default to zero).
#[derive(Debug, Clone, Default)]
pub struct CandidateMetadata {
    /// Number of blocking quality-gate failures for this candidate.
    pub blocking_gate_failures: usize,
    /// Confidence tier as reported by the discovery step.
    pub confidence: Confidence,
    /// Number of supporting evidence records.
    pub supporting_evidence_count: usize,
    /// Number of high-quality evidence records (official sources, benchmarks,
    /// or primary literature as defined by `citationMinimum.highQualitySourceTypes`).
    pub high_quality_evidence_count: usize,
    /// Number of evidence records flagged as contradicted / mixed / unresolved
    /// / retracted (the `contradiction.blockingStatuses` list in the policy).
    pub contradictory_evidence_count: usize,
    /// Whether the candidate's claim freshness is in an acceptable state.
    /// Maps to `freshness.acceptableStatuses` in the policy.
    pub freshness_acceptable: bool,
    /// Whether the literature-eligibility flag is set for this candidate.
    pub literature_eligible: bool,
}

/// Confidence tier for a discovery candidate.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Confidence {
    #[default]
    High,
    Moderate,
    Low,
}

/// Result of evaluating `defaultEligibilityCriteria` for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EligibilityResult {
    /// `true` when the candidate passes ALL default-eligibility criteria.
    pub passes: bool,
    /// Names of the criteria the candidate failed (empty when `passes == true`).
    pub failed_criteria: Vec<String>,
}

/// Evaluate every criterion in `policy.defaultEligibilityCriteria`.
///
/// Each criterion name maps to a boolean predicate over `candidate`:
///
/// | Criterion | Predicate |
/// |---|---|
/// | `no_blocking_quality_gates` | `blocking_gate_failures == 0` |
/// | `confidence_not_low` | `confidence != Low` |
/// | `has_supporting_evidence` | `supporting_evidence_count >= 1` |
/// | `has_high_quality_support` | `high_quality_evidence_count >= 1` |
/// | `no_contradictory_claims` | `contradictory_evidence_count == 0` |
/// | `no_freshness_issues` | `freshness_acceptable == true` |
/// | `literature_eligibility_confirmed` | `literature_eligible == true` |
///
/// Unknown criterion names (future policy additions) are skipped — they
/// produce a warning in tests but don't fail the check. This preserves
/// backward compatibility when new criteria are added to the policy before
/// the Rust module is updated.
///
/// The returned [`EligibilityResult`] carries `passes = false` and a
/// `failed_criteria` list whenever one or more criteria are unmet.
pub fn check_default_eligibility(
    candidate: &CandidateMetadata,
    criteria: &[String],
) -> EligibilityResult {
    let mut failed: Vec<String> = Vec::new();

    for criterion in criteria {
        let passes = match criterion.as_str() {
            "no_blocking_quality_gates" => candidate.blocking_gate_failures == 0,
            "confidence_not_low" => candidate.confidence != Confidence::Low,
            "has_supporting_evidence" => candidate.supporting_evidence_count >= 1,
            "has_high_quality_support" => candidate.high_quality_evidence_count >= 1,
            "no_contradictory_claims" => candidate.contradictory_evidence_count == 0,
            "no_freshness_issues" => candidate.freshness_acceptable,
            "literature_eligibility_confirmed" => candidate.literature_eligible,
            // Unknown criterion — skip rather than fail so new policy
            // additions don't break old code that hasn't been updated yet.
            _ => true,
        };
        if !passes {
            failed.push(criterion.clone());
        }
    }

    EligibilityResult {
        passes: failed.is_empty(),
        failed_criteria: failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_weights() -> CompositeScoreWeights {
        CompositeScoreWeights::default()
    }

    /// Effective weights after renormalization (specMatch missing, 4-axis split).
    fn renorm_effective() -> (f64, f64, f64, f64) {
        // remaining = 0.30 + 0.20 + 0.10 + 0.10 = 0.70
        let r = 0.70_f64;
        (0.30 / r, 0.20 / r, 0.10 / r, 0.10 / r)
    }

    #[test]
    fn all_axes_present_no_renormalization() {
        let weights = default_weights();
        let scores = AxisScores {
            default_suitability: 4.5,
            robustness: 4.0,
            adoption: 3.5,
            operational_fit: 3.0,
            spec_match: Some(5.0),
        };
        let expected = 0.30 * 4.5 + 0.20 * 4.0 + 0.10 * 3.5 + 0.10 * 3.0 + 0.30 * 5.0;
        let got = compute_composite_score(&scores, &weights);
        assert!(
            (got - expected).abs() < 1e-9,
            "expected {expected:.6}, got {got:.6}"
        );
    }

    #[test]
    fn spec_match_missing_renormalization_applied() {
        let weights = default_weights();
        let scores = AxisScores {
            default_suitability: 4.5,
            robustness: 4.0,
            adoption: 3.5,
            operational_fit: 3.0,
            spec_match: None,
        };
        let (ds_eff, rb_eff, ad_eff, of_eff) = renorm_effective();
        let expected = ds_eff * 4.5 + rb_eff * 4.0 + ad_eff * 3.5 + of_eff * 3.0;
        let got = compute_composite_score(&scores, &weights);
        assert!(
            (got - expected).abs() < 1e-9,
            "expected {expected:.6}, got {got:.6}"
        );
        // Effective weights must sum to 1.0
        let w_sum = ds_eff + rb_eff + ad_eff + of_eff;
        assert!(
            (w_sum - 1.0).abs() < 1e-9,
            "effective weights sum {w_sum:.9} != 1.0"
        );
    }

    #[test]
    fn renormalization_score_higher_than_literal_when_spec_match_zero() {
        // When spec_match is None vs. Some(0.0), the renorm path must produce
        // a strictly higher composite because it reclaims the 0.30 budget.
        let weights = default_weights();
        let base = AxisScores {
            default_suitability: 4.5,
            robustness: 4.0,
            adoption: 3.5,
            operational_fit: 3.0,
            spec_match: None,
        };
        let literal = AxisScores {
            spec_match: Some(0.0),
            ..base.clone()
        };
        let renorm_score = compute_composite_score(&base, &weights);
        let literal_score = compute_composite_score(&literal, &weights);
        assert!(
            renorm_score > literal_score,
            "renorm {renorm_score:.6} should exceed literal {literal_score:.6}"
        );
    }

    #[test]
    fn non_listed_axis_missing_falls_through_to_literal() {
        // defaultSuitability is NOT in renormalizeWhenAxisMissing.
        // Passing spec_match = None but also not listing "specMatch" in the
        // renorm list falls through to literal multiplication.
        let mut weights = default_weights();
        weights.renormalize_when_axis_missing = vec![]; // cleared
        let scores = AxisScores {
            default_suitability: 4.5,
            robustness: 4.0,
            adoption: 3.5,
            operational_fit: 3.0,
            spec_match: None, // treated as 0.0 — literal multiplication
        };
        // spec_match = None with empty renorm list → 0.0 contribution
        let expected = 0.30 * 4.5 + 0.20 * 4.0 + 0.10 * 3.5 + 0.10 * 3.0 + 0.30 * 0.0;
        let got = compute_composite_score(&scores, &weights);
        assert!(
            (got - expected).abs() < 1e-9,
            "expected {expected:.6}, got {got:.6}"
        );
    }

    #[test]
    fn backward_compat_absent_renormalize_field_uses_literal() {
        // A policy loaded without renormalizeWhenAxisMissing deserializes to an
        // empty list. Literal multiplication applies; no panic.
        let weights = CompositeScoreWeights {
            default_suitability: 0.30,
            robustness: 0.20,
            adoption: 0.10,
            operational_fit: 0.10,
            spec_match: 0.30,
            renormalize_when_axis_missing: vec![],
        };
        let scores = AxisScores {
            default_suitability: 4.0,
            robustness: 3.5,
            adoption: 3.0,
            operational_fit: 3.0,
            spec_match: None,
        };
        let expected = 0.30 * 4.0 + 0.20 * 3.5 + 0.10 * 3.0 + 0.10 * 3.0 + 0.30 * 0.0;
        let got = compute_composite_score(&scores, &weights);
        assert!(
            (got - expected).abs() < 1e-9,
            "backward-compat literal expected {expected:.6}, got {got:.6}"
        );
    }

    #[test]
    fn fixture_ivd_like_candidate_score_comparison() {
        // Mirrors the IVD / bulk-rnaseq fixture evaluation scores from
        // testdata/best-practice-scoring/fixtures.json (`clear_default_winner`).
        // candidate_default: ds=4.5, rb=4.2, ad=4.1, of=4.0, no spec prefs.
        let weights = default_weights();

        let before_score = {
            // Old behaviour: spec_match = None → treated as 0.0
            let mut old_weights = weights.clone();
            old_weights.renormalize_when_axis_missing = vec![];
            compute_composite_score(
                &AxisScores {
                    default_suitability: 4.5,
                    robustness: 4.2,
                    adoption: 4.1,
                    operational_fit: 4.0,
                    spec_match: None,
                },
                &old_weights,
            )
        };

        let after_score = compute_composite_score(
            &AxisScores {
                default_suitability: 4.5,
                robustness: 4.2,
                adoption: 4.1,
                operational_fit: 4.0,
                spec_match: None,
            },
            &weights,
        );

        let delta = after_score - before_score;
        // Renormalization must increase the score when specMatch was contributing 0.
        assert!(
            delta > 0.0,
            "IVD fixture: expected positive delta, got delta={delta:.6}"
        );
        // Print for SME sign-off table.
        eprintln!(
            "[fixture:ivd_clear_default_winner] before={before_score:.4} after={after_score:.4} delta={delta:+.4}"
        );

        // ATAC-like fixture (alternative_beats_primary, candidate_alternative):
        // ds=4.8, rb=4.6, ad=4.0, of=4.2
        let atac_before = {
            let mut old_weights = weights.clone();
            old_weights.renormalize_when_axis_missing = vec![];
            compute_composite_score(
                &AxisScores {
                    default_suitability: 4.8,
                    robustness: 4.6,
                    adoption: 4.0,
                    operational_fit: 4.2,
                    spec_match: None,
                },
                &old_weights,
            )
        };
        let atac_after = compute_composite_score(
            &AxisScores {
                default_suitability: 4.8,
                robustness: 4.6,
                adoption: 4.0,
                operational_fit: 4.2,
                spec_match: None,
            },
            &weights,
        );
        let atac_delta = atac_after - atac_before;
        assert!(
            atac_delta > 0.0,
            "ATAC fixture: expected positive delta, got delta={atac_delta:.6}"
        );
        eprintln!(
            "[fixture:atac_alternative_beats_primary] before={atac_before:.4} after={atac_after:.4} delta={atac_delta:+.4}"
        );
    }

    #[test]
    fn spec_match_present_zero_score_no_renorm() {
        // spec_match = Some(0.0): axis IS present (candidate not in prefs list,
        // but prefs list is non-empty). Weight must NOT be redistributed.
        let weights = default_weights();
        let scores = AxisScores {
            default_suitability: 4.5,
            robustness: 4.0,
            adoption: 3.5,
            operational_fit: 3.0,
            spec_match: Some(0.0),
        };
        // spec_match weight stays at 0.30 (contributes 0.0 * 0.30 = 0)
        let expected = 0.30 * 4.5 + 0.20 * 4.0 + 0.10 * 3.5 + 0.10 * 3.0 + 0.30 * 0.0;
        let got = compute_composite_score(&scores, &weights);
        assert!(
            (got - expected).abs() < 1e-9,
            "scored-zero spec_match must use literal path: expected {expected:.6}, got {got:.6}"
        );
    }

    #[test]
    fn final_score_stays_in_range() {
        // Max scores on all axes should produce ≤ 5.0.
        let weights = default_weights();
        let max_with_spec = AxisScores {
            default_suitability: 5.0,
            robustness: 5.0,
            adoption: 5.0,
            operational_fit: 5.0,
            spec_match: Some(5.0),
        };
        let max_without_spec = AxisScores {
            spec_match: None,
            ..max_with_spec.clone()
        };
        let s1 = compute_composite_score(&max_with_spec, &weights);
        let s2 = compute_composite_score(&max_without_spec, &weights);
        assert!(
            s1 <= 5.0 + 1e-9,
            "all-5 with spec_match should be ≤ 5.0, got {s1}"
        );
        assert!(
            s2 <= 5.0 + 1e-9,
            "all-5 without spec_match should be ≤ 5.0, got {s2}"
        );
        // Both should equal 5.0 exactly.
        assert!(
            (s1 - 5.0).abs() < 1e-9,
            "all-5 with spec should be 5.0, got {s1}"
        );
        assert!(
            (s2 - 5.0).abs() < 1e-9,
            "all-5 without spec (renorm) should be 5.0, got {s2}"
        );
    }
}
