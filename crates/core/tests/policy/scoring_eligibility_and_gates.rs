//! Tests for quality-gate penalty application and default-eligibility checks.
//!
//! Covers the three scenarios from the D.3 task spec:
//!   1. Quality-gate penalty arithmetic (zero gates, 1 blocking, 2 blocking,
//!      1 blocking + 1 non-blocking).
//!   2. Default-eligibility pass/fail with criterion-list reporting.
//!   3. Integration: composite score drops after a blocking gate failure AND
//!      the candidate is ineligible for the defaultRecommended tier.
//!
//! Tests load penalty values from the live policy JSON so future threshold
//! changes propagate automatically.

use ecaa_workflow_core::composite_score::{
    apply_quality_gate_penalties, check_default_eligibility, CandidateMetadata, Confidence,
    GateSeverity, QualityGatePenalties, QualityGateResult,
};
use std::path::Path;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn policy_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy/best-practice-scoring-policy.json")
}

fn load_policy() -> serde_json::Value {
    let raw = std::fs::read_to_string(policy_path()).expect("policy file readable");
    serde_json::from_str(&raw).expect("policy parses as JSON")
}

fn load_penalties() -> QualityGatePenalties {
    let v = load_policy();
    serde_json::from_value(v["qualityGatePenalties"].clone())
        .expect("qualityGatePenalties deserializes")
}

fn load_criteria() -> Vec<String> {
    let v = load_policy();
    serde_json::from_value(v["defaultEligibilityCriteria"].clone())
        .expect("defaultEligibilityCriteria deserializes as Vec<String>")
}

fn gate(id: &str, passed: bool, severity: GateSeverity) -> QualityGateResult {
    QualityGateResult {
        gate_id: id.to_string(),
        passed,
        severity,
    }
}

// ---------------------------------------------------------------------------
// Part 1 — Quality-gate penalty arithmetic
// ---------------------------------------------------------------------------

#[test]
fn no_gates_score_unchanged() {
    let penalties = load_penalties();
    let score = apply_quality_gate_penalties(4.0, &[], &penalties);
    assert!(
        (score - 4.0).abs() < 1e-9,
        "no gates: expected 4.0, got {score}"
    );
}

#[test]
fn all_gates_passing_score_unchanged() {
    let penalties = load_penalties();
    let gates = vec![
        gate("repro_check", true, GateSeverity::Blocking),
        gate("freshness_check", true, GateSeverity::NonBlocking),
    ];
    let score = apply_quality_gate_penalties(4.0, &gates, &penalties);
    assert!(
        (score - 4.0).abs() < 1e-9,
        "all-pass gates: expected 4.0, got {score}"
    );
}

#[test]
fn one_blocking_failure_subtracts_blocking_penalty() {
    let penalties = load_penalties();
    // Policy value: blocking = -1.0
    let gates = vec![gate("repro_check", false, GateSeverity::Blocking)];
    let score = apply_quality_gate_penalties(4.0, &gates, &penalties);
    let expected = 4.0 + penalties.blocking; // 4.0 + (-1.0) = 3.0
    assert!(
        (score - expected).abs() < 1e-9,
        "1 blocking failure: expected {expected}, got {score}"
    );
}

#[test]
fn two_blocking_failures_stack_additively() {
    let penalties = load_penalties();
    let gates = vec![
        gate("repro_check", false, GateSeverity::Blocking),
        gate("fdr_check", false, GateSeverity::Blocking),
    ];
    let score = apply_quality_gate_penalties(4.0, &gates, &penalties);
    let expected = 4.0 + 2.0 * penalties.blocking; // 4.0 + 2*(-1.0) = 2.0
    assert!(
        (score - expected).abs() < 1e-9,
        "2 blocking failures: expected {expected}, got {score}"
    );
}

#[test]
fn one_blocking_one_non_blocking_stack_correctly() {
    let penalties = load_penalties();
    let gates = vec![
        gate("repro_check", false, GateSeverity::Blocking),
        gate("citation_depth", false, GateSeverity::NonBlocking),
    ];
    let score = apply_quality_gate_penalties(4.0, &gates, &penalties);
    // 4.0 + (-1.0) + (-0.25) = 2.75
    let expected = 4.0 + penalties.blocking + penalties.non_blocking;
    assert!(
        (score - expected).abs() < 1e-9,
        "1 blocking + 1 non-blocking: expected {expected}, got {score}"
    );
}

#[test]
fn penalty_matches_policy_json_values() {
    // Confirm the policy still has blocking=-1.0, nonBlocking=-0.25.
    // This test catches accidental threshold drift.
    let penalties = load_penalties();
    assert!(
        (penalties.blocking - (-1.0)).abs() < 1e-9,
        "policy blocking penalty expected -1.0, got {}",
        penalties.blocking
    );
    assert!(
        (penalties.non_blocking - (-0.25)).abs() < 1e-9,
        "policy nonBlocking penalty expected -0.25, got {}",
        penalties.non_blocking
    );
}

// ---------------------------------------------------------------------------
// Part 2 — Default-eligibility pass/fail
// ---------------------------------------------------------------------------

#[test]
fn all_criteria_satisfied_passes() {
    let criteria = load_criteria();
    let candidate = CandidateMetadata {
        blocking_gate_failures: 0,
        confidence: Confidence::High,
        supporting_evidence_count: 2,
        high_quality_evidence_count: 1,
        contradictory_evidence_count: 0,
        freshness_acceptable: true,
        literature_eligible: true,
    };
    let result = check_default_eligibility(&candidate, &criteria);
    assert!(
        result.passes,
        "all-satisfied candidate should pass; failed: {:?}",
        result.failed_criteria
    );
    assert!(result.failed_criteria.is_empty());
}

#[test]
fn blocking_gate_failure_fails_no_blocking_quality_gates() {
    let criteria = load_criteria();
    let candidate = CandidateMetadata {
        blocking_gate_failures: 1,
        confidence: Confidence::High,
        supporting_evidence_count: 2,
        high_quality_evidence_count: 1,
        contradictory_evidence_count: 0,
        freshness_acceptable: true,
        literature_eligible: true,
    };
    let result = check_default_eligibility(&candidate, &criteria);
    assert!(
        !result.passes,
        "candidate with blocking gate failure should fail eligibility"
    );
    assert!(
        result
            .failed_criteria
            .contains(&"no_blocking_quality_gates".to_string()),
        "failed_criteria must include no_blocking_quality_gates; got {:?}",
        result.failed_criteria
    );
}

#[test]
fn low_confidence_fails_confidence_not_low() {
    let criteria = load_criteria();
    let candidate = CandidateMetadata {
        blocking_gate_failures: 0,
        confidence: Confidence::Low,
        supporting_evidence_count: 2,
        high_quality_evidence_count: 1,
        contradictory_evidence_count: 0,
        freshness_acceptable: true,
        literature_eligible: true,
    };
    let result = check_default_eligibility(&candidate, &criteria);
    assert!(!result.passes);
    assert!(
        result
            .failed_criteria
            .contains(&"confidence_not_low".to_string()),
        "failed_criteria must include confidence_not_low; got {:?}",
        result.failed_criteria
    );
}

#[test]
fn three_criteria_failures_all_reported() {
    let criteria = load_criteria();
    let candidate = CandidateMetadata {
        blocking_gate_failures: 1,
        confidence: Confidence::Low,
        supporting_evidence_count: 0, // fails has_supporting_evidence
        high_quality_evidence_count: 0,
        contradictory_evidence_count: 0,
        freshness_acceptable: true,
        literature_eligible: true,
    };
    let result = check_default_eligibility(&candidate, &criteria);
    assert!(!result.passes);
    // Expect at minimum: no_blocking_quality_gates, confidence_not_low,
    // has_supporting_evidence, has_high_quality_support
    for criterion in &[
        "no_blocking_quality_gates",
        "confidence_not_low",
        "has_supporting_evidence",
        "has_high_quality_support",
    ] {
        assert!(
            result.failed_criteria.contains(&criterion.to_string()),
            "expected {criterion} in failed_criteria; got {:?}",
            result.failed_criteria
        );
    }
    assert!(
        result.failed_criteria.len() >= 3,
        "expected ≥ 3 failed criteria, got {}",
        result.failed_criteria.len()
    );
}

#[test]
fn missing_literature_eligibility_fails_criterion() {
    let criteria = load_criteria();
    let candidate = CandidateMetadata {
        blocking_gate_failures: 0,
        confidence: Confidence::High,
        supporting_evidence_count: 1,
        high_quality_evidence_count: 1,
        contradictory_evidence_count: 0,
        freshness_acceptable: true,
        literature_eligible: false, // fails literature_eligibility_confirmed
    };
    let result = check_default_eligibility(&candidate, &criteria);
    assert!(!result.passes);
    assert!(
        result
            .failed_criteria
            .contains(&"literature_eligibility_confirmed".to_string()),
        "failed_criteria must include literature_eligibility_confirmed; got {:?}",
        result.failed_criteria
    );
}

#[test]
fn contradictory_evidence_fails_criterion() {
    let criteria = load_criteria();
    let candidate = CandidateMetadata {
        blocking_gate_failures: 0,
        confidence: Confidence::High,
        supporting_evidence_count: 2,
        high_quality_evidence_count: 1,
        contradictory_evidence_count: 1, // fails no_contradictory_claims
        freshness_acceptable: true,
        literature_eligible: true,
    };
    let result = check_default_eligibility(&candidate, &criteria);
    assert!(!result.passes);
    assert!(
        result
            .failed_criteria
            .contains(&"no_contradictory_claims".to_string()),
        "failed_criteria must include no_contradictory_claims; got {:?}",
        result.failed_criteria
    );
}

#[test]
fn freshness_issue_fails_criterion() {
    let criteria = load_criteria();
    let candidate = CandidateMetadata {
        blocking_gate_failures: 0,
        confidence: Confidence::High,
        supporting_evidence_count: 2,
        high_quality_evidence_count: 1,
        contradictory_evidence_count: 0,
        freshness_acceptable: false, // fails no_freshness_issues
        literature_eligible: true,
    };
    let result = check_default_eligibility(&candidate, &criteria);
    assert!(!result.passes);
    assert!(
        result
            .failed_criteria
            .contains(&"no_freshness_issues".to_string()),
        "failed_criteria must include no_freshness_issues; got {:?}",
        result.failed_criteria
    );
}

#[test]
fn empty_criteria_list_always_passes() {
    // Backward-compat: if the policy lacks defaultEligibilityCriteria,
    // the caller passes an empty slice and every candidate passes.
    let candidate = CandidateMetadata {
        blocking_gate_failures: 5,
        confidence: Confidence::Low,
        supporting_evidence_count: 0,
        high_quality_evidence_count: 0,
        contradictory_evidence_count: 3,
        freshness_acceptable: false,
        literature_eligible: false,
    };
    let result = check_default_eligibility(&candidate, &[]);
    assert!(
        result.passes,
        "empty criteria list should always pass (backward-compat)"
    );
}

// ---------------------------------------------------------------------------
// Part 3 — Integration: penalty lowers composite; eligibility gate blocks tier
// ---------------------------------------------------------------------------

#[test]
fn blocking_gate_lowers_score_and_fails_eligibility() {
    use ecaa_workflow_core::composite_score::{
        compute_composite_score, AxisScores, CompositeScoreWeights,
    };

    let policy = load_policy();
    let weights: CompositeScoreWeights =
        serde_json::from_value(policy["compositeScoreWeights"].clone())
            .expect("weights deserialize");
    let penalties = load_penalties();
    let criteria = load_criteria();

    // Composite score 5.0 (all axes at max with spec_match = None → renorm).
    let axes = AxisScores {
        default_suitability: 5.0,
        robustness: 5.0,
        adoption: 5.0,
        operational_fit: 5.0,
        spec_match: None,
    };
    let base_composite = compute_composite_score(&axes, &weights);
    assert!(
        (base_composite - 5.0).abs() < 1e-9,
        "all-5 renorm should give 5.0, got {base_composite}"
    );

    // Apply 1 blocking gate failure: 5.0 + (-1.0) = 4.0
    let gates = vec![gate("repro_check", false, GateSeverity::Blocking)];
    let penalized = apply_quality_gate_penalties(base_composite, &gates, &penalties);
    let expected_penalized = 5.0 + penalties.blocking; // 4.0
    assert!(
        (penalized - expected_penalized).abs() < 1e-9,
        "penalized score expected {expected_penalized}, got {penalized}"
    );

    // Despite 4.0 > 3.5 threshold, candidate fails eligibility.
    let candidate = CandidateMetadata {
        blocking_gate_failures: 1,
        confidence: Confidence::High,
        supporting_evidence_count: 2,
        high_quality_evidence_count: 1,
        contradictory_evidence_count: 0,
        freshness_acceptable: true,
        literature_eligible: true,
    };
    let eligibility = check_default_eligibility(&candidate, &criteria);
    assert!(
        !eligibility.passes,
        "candidate with blocking gate failure must not pass default eligibility"
    );
    assert!(
        eligibility
            .failed_criteria
            .contains(&"no_blocking_quality_gates".to_string()),
        "no_blocking_quality_gates must appear in failed_criteria; got {:?}",
        eligibility.failed_criteria
    );

    // The penalized score is above 3.5 (threshold) but the candidate cannot
    // be default-recommended because passes_default_eligibility_criteria=false.
    let threshold: f64 = serde_json::from_value(policy["defaultRecommendationThreshold"].clone())
        .expect("threshold deserializes");
    assert!(
        penalized > threshold,
        "penalized score {penalized} should still be above threshold {threshold}"
    );
    assert!(
        !eligibility.passes,
        "above-threshold but ineligible candidate must not be default-recommended"
    );
}
