//! Integration tests for composite-score weight renormalization.
//!
//! These tests verify the four scenarios from the D.1 task:
//! 1. All axes present — literal weighted sum, no renormalization.
//! 2. specMatch missing — weights redistributed among 4 axes; effective
//!    weights sum to 1.0.
//! 3. IVD/ATAC fixture comparison — before/after delta is positive and in
//!    the expected direction.
//! 4. Non-listed axis missing falls through to literal multiplication.
//!
//! The tests load weights from the live
//! `config/downstream-policy/best-practice-scoring-policy.json` so any
//! future weight changes automatically propagate here.

use scripps_workflow_core::composite_score::{
    compute_composite_score, AxisScores, CompositeScoreWeights,
};
use std::path::Path;

fn policy_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy/best-practice-scoring-policy.json")
}

fn load_weights() -> CompositeScoreWeights {
    let raw = std::fs::read_to_string(policy_path()).expect("policy file readable");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("policy parses as JSON");
    serde_json::from_value(v["compositeScoreWeights"].clone())
        .expect("compositeScoreWeights deserializes into CompositeScoreWeights")
}

// ---------------------------------------------------------------------------
// Test 1 — all axes present, no renormalization
// ---------------------------------------------------------------------------
#[test]
fn all_axes_present_no_renormalization() {
    let weights = load_weights();
    let scores = AxisScores {
        default_suitability: 4.5,
        robustness: 4.0,
        adoption: 3.5,
        operational_fit: 3.0,
        spec_match: Some(5.0),
    };
    // Expected = literal weighted sum using policy weights.
    let expected = weights.default_suitability * 4.5
        + weights.robustness * 4.0
        + weights.adoption * 3.5
        + weights.operational_fit * 3.0
        + weights.spec_match * 5.0;
    let got = compute_composite_score(&scores, &weights);
    assert!(
        (got - expected).abs() < 1e-9,
        "Test 1 FAIL: expected {expected:.6}, got {got:.6}"
    );
}

// ---------------------------------------------------------------------------
// Test 2 — specMatch missing, renormalization applied
// ---------------------------------------------------------------------------
#[test]
fn spec_match_missing_renormalization_applied() {
    let weights = load_weights();
    // Confirm the policy has specMatch in renormalizeWhenAxisMissing.
    assert!(
        weights
            .renormalize_when_axis_missing
            .contains(&"specMatch".to_string()),
        "policy must list specMatch in renormalizeWhenAxisMissing"
    );

    let scores = AxisScores {
        default_suitability: 4.5,
        robustness: 4.0,
        adoption: 3.5,
        operational_fit: 3.0,
        spec_match: None, // no spec_preferred_methods in this stage
    };

    // Effective weights after redistributing specMatch's weight (0.30).
    // remaining = ds + rb + ad + of = 0.30 + 0.20 + 0.10 + 0.10 = 0.70
    let remaining = weights.default_suitability
        + weights.robustness
        + weights.adoption
        + weights.operational_fit;
    let ds_eff = weights.default_suitability / remaining;
    let rb_eff = weights.robustness / remaining;
    let ad_eff = weights.adoption / remaining;
    let of_eff = weights.operational_fit / remaining;

    let expected = ds_eff * 4.5 + rb_eff * 4.0 + ad_eff * 3.5 + of_eff * 3.0;
    let got = compute_composite_score(&scores, &weights);

    assert!(
        (got - expected).abs() < 1e-9,
        "Test 2 FAIL: expected {expected:.6}, got {got:.6}"
    );

    // The effective weights sum to 1.0.
    let w_sum = ds_eff + rb_eff + ad_eff + of_eff;
    assert!(
        (w_sum - 1.0).abs() < 1e-9,
        "effective weights sum {w_sum:.9} != 1.0"
    );

    // Renorm score must exceed literal (which loses 30% budget).
    let literal_score = {
        let mut literal_weights = weights.clone();
        literal_weights.renormalize_when_axis_missing = vec![];
        compute_composite_score(
            &AxisScores {
                spec_match: None,
                ..scores.clone()
            },
            &literal_weights,
        )
    };
    assert!(
        got > literal_score,
        "Test 2 FAIL: renorm {got:.6} should exceed literal {literal_score:.6}"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — IVD/ATAC fixture comparison: before vs after delta
//
// Fixture data sourced from testdata/best-practice-scoring/fixtures.json:
//   - clear_default_winner / candidate_default:  ds=4.5, rb=4.2, ad=4.1, of=4.0
//   - alternative_beats_primary / candidate_alt: ds=4.8, rb=4.6, ad=4.0, of=4.2
//
// Both have empty spec_preferred_methods today (0 of 92 atoms populate it).
// ---------------------------------------------------------------------------
#[test]
fn ivd_atac_fixture_score_delta_positive() {
    let weights = load_weights();

    struct Fixture {
        label: &'static str,
        default_suitability: f64,
        robustness: f64,
        adoption: f64,
        operational_fit: f64,
    }

    let fixtures = [
        Fixture {
            label: "ivd:clear_default_winner:candidate_default",
            default_suitability: 4.5,
            robustness: 4.2,
            adoption: 4.1,
            operational_fit: 4.0,
        },
        Fixture {
            label: "atac:alternative_beats_primary:candidate_alternative",
            default_suitability: 4.8,
            robustness: 4.6,
            adoption: 4.0,
            operational_fit: 4.2,
        },
        Fixture {
            label: "ivd:close_contest:candidate_default",
            default_suitability: 4.4,
            robustness: 4.2,
            adoption: 4.1,
            operational_fit: 4.0,
        },
        Fixture {
            label: "ivd:close_contest:candidate_alternative",
            default_suitability: 4.3,
            robustness: 4.1,
            adoption: 4.0,
            operational_fit: 3.9,
        },
    ];

    eprintln!(
        "\n{:<50} {:>8} {:>8} {:>8}",
        "candidate", "before", "after", "delta"
    );
    eprintln!("{}", "-".repeat(78));

    for f in &fixtures {
        let scores_none = AxisScores {
            default_suitability: f.default_suitability,
            robustness: f.robustness,
            adoption: f.adoption,
            operational_fit: f.operational_fit,
            spec_match: None,
        };

        // Before: literal multiplication (spec_match = None → 0.0)
        let before = {
            let mut old = weights.clone();
            old.renormalize_when_axis_missing = vec![];
            compute_composite_score(&scores_none, &old)
        };

        // After: renormalization applied
        let after = compute_composite_score(&scores_none, &weights);
        let delta = after - before;

        eprintln!(
            "{:<50} {:>8.4} {:>8.4} {:>+8.4}",
            f.label, before, after, delta
        );

        assert!(
            delta > 0.0,
            "Test 3 FAIL for {}: expected positive delta, got {delta:.6}",
            f.label
        );
        // Delta should be within (0, 1.5] — renorm can't produce more than
        // the 0.30 budget applied to a max 5.0 score = 1.5 uplift.
        assert!(
            delta <= 1.5 + 1e-9,
            "Test 3 FAIL for {}: delta {delta:.6} exceeds max possible 1.5",
            f.label
        );
    }
}

// ---------------------------------------------------------------------------
// Test 4 — non-listed axis missing falls through to literal multiplication
// ---------------------------------------------------------------------------
#[test]
fn non_listed_axis_missing_falls_through_to_literal() {
    // Construct weights where renormalize_when_axis_missing is empty.
    // spec_match = None with no renorm list → treated as 0.0.
    let mut weights = load_weights();
    weights.renormalize_when_axis_missing = vec![];

    let scores = AxisScores {
        default_suitability: 4.0,
        robustness: 3.5,
        adoption: 3.0,
        operational_fit: 3.0,
        spec_match: None,
    };

    // Literal: spec_match contributes 0.0
    let expected = weights.default_suitability * 4.0
        + weights.robustness * 3.5
        + weights.adoption * 3.0
        + weights.operational_fit * 3.0
        + weights.spec_match * 0.0;
    let got = compute_composite_score(&scores, &weights);

    assert!(
        (got - expected).abs() < 1e-9,
        "Test 4 FAIL: expected literal {expected:.6}, got {got:.6}"
    );
    // The 30% budget is genuinely lost in the literal path.
    // Confirm this is less than the renorm path.
    let renorm_weights = load_weights();
    let renorm_score = compute_composite_score(&scores, &renorm_weights);
    assert!(
        renorm_score > got,
        "Test 4 FAIL: renorm {renorm_score:.6} should exceed literal {got:.6}"
    );
}
