//! Tier F property test for F19 — lifecycle promotion is gated by the
//! validation × lifecycle grid in `config/promotion-gate-policy.yaml`.
//! Ad-hoc promotion in code is forbidden.
//!
//! V4 alignment locks the invariant that the
//! canonical promotion-gate-policy.yaml is the source of truth for
//! the promotion decision. The property asserts:
//!
//! 1. The canonical YAML loads cleanly + carries every required state
//! keyed by `LifecycleState::canonical_name()`.
//! 2. The grid's decision for a randomly-generated
//! `(target, counts, approvals)` matches the deterministic
//! "every required class met" rule encoded by the schema.
//! 3. The decision shape is `Allow` or `Deny { missing_classes,
//! missing_approvals }` — never a hidden ad-hoc state.

use proptest::prelude::*;
use ecaa_workflow_core::promotion_gate_policy::{
    ClassRequirement, ClassRequirementTag, PassingClassCounts, PromotionDecision,
    PromotionGatePolicy,
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
fn canonical_policy_carries_every_required_state() {
    let policy = load_canonical_policy();
    for state in [
        LifecycleState::Hypothesized,
        LifecycleState::Contracted,
        LifecycleState::Implemented,
        LifecycleState::LocallyValidated,
        LifecycleState::BenchmarkValidated,
        LifecycleState::Production,
    ] {
        assert!(
            policy.states.contains_key(state.canonical_name()),
            "canonical promotion-gate-policy.yaml is missing state {}",
            state.canonical_name()
        );
    }
}

/// Helper: count required-class hits given the canonical policy's
/// per-state requirements. Used by the property assertion below.
fn satisfies_requirements(
    policy: &PromotionGatePolicy,
    target: &LifecycleState,
    counts: &PassingClassCounts,
) -> bool {
    let key = target.canonical_name();
    let Some(req) = policy.states.get(key) else {
        return false;
    };
    let check = |r: &ClassRequirement, actual: u32| -> bool {
        match r {
            ClassRequirement::Tag(ClassRequirementTag::Optional) => true,
            ClassRequirement::Tag(ClassRequirementTag::Required) => actual >= 1,
            ClassRequirement::MinCount { min_count } => actual >= *min_count,
        }
    };
    check(&req.contract, counts.contract)
        && check(&req.golden, counts.golden)
        && check(&req.metamorphic, counts.metamorphic)
        && check(&req.biological_invariant, counts.biological_invariant)
        && check(&req.statistical_sanity, counts.statistical_sanity)
        && check(&req.reproducibility, counts.reproducibility)
}

proptest! {
    /// The grid's `consult` decision for a randomly-generated
    /// `(target, counts)` must match the "every required class met
    /// + every approval recorded" deterministic rule encoded by the
    /// schema. F19: no ad-hoc decision path can override the grid.
    #[test]
    fn grid_decision_matches_schema(
        target in prop_oneof![
            Just(LifecycleState::Hypothesized),
            Just(LifecycleState::Contracted),
            Just(LifecycleState::Implemented),
            Just(LifecycleState::LocallyValidated),
            Just(LifecycleState::BenchmarkValidated),
            Just(LifecycleState::Production),
        ],
        contract_count in 0u32..3,
        golden_count in 0u32..3,
        metamorphic_count in 0u32..3,
        bio_count in 0u32..3,
        stat_count in 0u32..3,
        repro_count in 0u32..3,
    ) {
        let policy = load_canonical_policy();
        let counts = PassingClassCounts {
            contract: contract_count,
            golden: golden_count,
            metamorphic: metamorphic_count,
            biological_invariant: bio_count,
            statistical_sanity: stat_count,
            reproducibility: repro_count,
        };
        // Include both approval credentials so the approval gate is
        // not the failure axis under test (this property exercises
        // class-count gating).
        let approvals = vec![
            "domain_expert".to_string(),
            "validation_engineer".to_string(),
        ];
        let decision = policy.consult(&target, &counts, &approvals);
        let should_allow = satisfies_requirements(&policy, &target, &counts);
        match (should_allow, &decision) {
            (true, PromotionDecision::Allow) => {}
            (false, PromotionDecision::Deny { .. }) => {}
            (a, d) => prop_assert!(
                false,
                "F19 mismatch for {target:?}: should_allow={a}, decision={d:?}"
            ),
        }
    }
}

#[test]
fn production_requires_both_credential_approvals() {
    let policy = load_canonical_policy();
    // Maxed-out evidence but no approvals.
    let counts = PassingClassCounts {
        contract: 1,
        golden: 1,
        metamorphic: 2,
        biological_invariant: 2,
        statistical_sanity: 1,
        reproducibility: 1,
    };
    let decision = policy.consult(&LifecycleState::Production, &counts, &[]);
    assert!(
        matches!(decision, PromotionDecision::Deny { .. }),
        "Production without approvals must Deny, got {decision:?}"
    );
    if let PromotionDecision::Deny {
        missing_approvals, ..
    } = decision
    {
        assert!(missing_approvals.contains(&"domain_expert".to_string()));
        assert!(missing_approvals.contains(&"validation_engineer".to_string()));
    }
}

#[test]
fn production_allowed_with_full_evidence_and_credentials() {
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
        "Production with full evidence + both approvals must Allow, got {decision:?}"
    );
}

#[test]
fn no_promotion_path_exists_outside_the_grid() {
    // F19: the only way to obtain Allow is to satisfy the grid. Any
    // (state, zero-evidence, no-approvals) path that isn't
    // Hypothesized/Contracted/Implemented must Deny.
    let policy = load_canonical_policy();
    let counts = PassingClassCounts::default();
    for state in [
        LifecycleState::LocallyValidated,
        LifecycleState::BenchmarkValidated,
        LifecycleState::Production,
    ] {
        let decision = policy.consult(&state, &counts, &[]);
        assert!(
            matches!(decision, PromotionDecision::Deny { .. }),
            "F19: {} with zero evidence must Deny, got {decision:?}",
            state.canonical_name()
        );
    }
}
