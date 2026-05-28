//! Tier F property test for F11 — promotion is refused for any
//! candidate that lacks required evidence per the
//! validation × lifecycle promotion grid in
//! `config/promotion-gate-policy.yaml`.
//!
//! The invariant the property locks in:
//!
//! 1. **Grid-driven refusal.** A node attempting to enter a target
//! state without the grid's required validator-class counts is
//! `PromotionDecision::Deny`. F11 was historically a hard-coded
//! "production-only" refusal; v4 P3 generalizes to "any state's
//! requirements unmet" via the config-driven grid.
//! 2. **Missing-credential refusal.** When the target state has a
//! `required_approvals` list, a node missing the credential class
//! is `Deny` with the approval in `missing_approvals`.
//! 3. **Production refused by default.** A node with only
//! `contract + golden` passing validators cannot reach Production
//! (which requires metamorphic min_count: 2 + bio min_count: 2
//! + statistical_sanity + reproducibility + two approval classes).

use ecaa_workflow_core::promotion_gate_policy::{
    PassingClassCounts, PromotionDecision, PromotionGatePolicy,
};
use ecaa_workflow_core::workflow_contracts::lifecycle::LifecycleState;

fn load_canonical_policy() -> std::sync::Arc<PromotionGatePolicy> {
    PromotionGatePolicy::load_from_file(std::path::Path::new(
        "../../config/promotion-gate-policy.yaml",
    ))
    .or_else(|_| {
        PromotionGatePolicy::load_from_file(std::path::Path::new(
            "config/promotion-gate-policy.yaml",
        ))
    })
    .expect("canonical promotion-gate-policy.yaml must load")
}

#[test]
fn promotion_to_production_refused_when_grid_unsatisfied() {
    let policy = load_canonical_policy();
    // A node with only contract + golden passes Implemented but not
    // Production — Production demands metamorphic >= 2, bio >= 2,
    // statistical_sanity, reproducibility, plus two approval classes.
    let counts = PassingClassCounts {
        contract: 1,
        golden: 1,
        metamorphic: 0,
        biological_invariant: 0,
        statistical_sanity: 0,
        reproducibility: 0,
    };
    let decision = policy.consult(&LifecycleState::Production, &counts, &[]);
    assert!(
        matches!(decision, PromotionDecision::Deny { .. }),
        "Production with only contract+golden should Deny, got {decision:?}"
    );
    if let PromotionDecision::Deny {
        missing_classes,
        missing_approvals,
    } = decision
    {
        // Multiple classes should be missing.
        assert!(!missing_classes.is_empty());
        // The two required-credential classes should both be missing
        // (we passed no approvals).
        assert!(missing_approvals.contains(&"domain_expert".to_string()));
        assert!(missing_approvals.contains(&"validation_engineer".to_string()));
    }
}

#[test]
fn promotion_to_locally_validated_refused_without_metamorphic() {
    let policy = load_canonical_policy();
    let counts = PassingClassCounts {
        contract: 1,
        golden: 1,
        metamorphic: 0, // required min_count: 1
        biological_invariant: 1,
        statistical_sanity: 0,
        reproducibility: 0,
    };
    let decision = policy.consult(&LifecycleState::LocallyValidated, &counts, &[]);
    assert!(
        matches!(decision, PromotionDecision::Deny { .. }),
        "LocallyValidated without metamorphic should Deny, got {decision:?}"
    );
}

#[test]
fn promotion_to_locally_validated_allowed_with_all_classes() {
    let policy = load_canonical_policy();
    let counts = PassingClassCounts {
        contract: 1,
        golden: 1,
        metamorphic: 1,
        biological_invariant: 1,
        statistical_sanity: 0,
        reproducibility: 0,
    };
    let decision = policy.consult(&LifecycleState::LocallyValidated, &counts, &[]);
    assert!(
        matches!(decision, PromotionDecision::Allow),
        "LocallyValidated with all four required classes should Allow, got {decision:?}"
    );
}

#[test]
fn promotion_to_benchmark_validated_refused_without_min_2_metamorphic() {
    let policy = load_canonical_policy();
    let counts = PassingClassCounts {
        contract: 1,
        golden: 1,
        metamorphic: 1, // required min_count: 2
        biological_invariant: 1,
        statistical_sanity: 1,
        reproducibility: 1,
    };
    let decision = policy.consult(&LifecycleState::BenchmarkValidated, &counts, &[]);
    assert!(
        matches!(decision, PromotionDecision::Deny { .. }),
        "BenchmarkValidated with only 1 metamorphic should Deny, got {decision:?}"
    );
}

#[test]
fn promotion_to_production_allowed_with_full_evidence_and_approvals() {
    let policy = load_canonical_policy();
    let counts = PassingClassCounts {
        contract: 1,
        golden: 1,
        metamorphic: 2,
        biological_invariant: 2,
        statistical_sanity: 1,
        reproducibility: 1,
    };
    let approvals = vec![
        "domain_expert".to_string(),
        "validation_engineer".to_string(),
    ];
    let decision = policy.consult(&LifecycleState::Production, &counts, &approvals);
    assert!(
        matches!(decision, PromotionDecision::Allow),
        "Production with full evidence + both approvals should Allow, got {decision:?}"
    );
}

#[test]
fn hypothesized_state_allowed_with_zero_evidence() {
    let policy = load_canonical_policy();
    let counts = PassingClassCounts::default();
    let decision = policy.consult(&LifecycleState::Hypothesized, &counts, &[]);
    assert!(
        matches!(decision, PromotionDecision::Allow),
        "Hypothesized with no evidence should Allow, got {decision:?}"
    );
}
