//! Tier F property test for F15 — a downstream-policy denial is a
//! hard, structured decision: the promotion gate returns
//! `PromotionDecision::Deny` enumerating the missing evidence, never a
//! soft warning or an allow-with-caveat. The decision space is binary
//! (`Allow` | `Deny`) — there is no "warn" path a denial could
//! silently degrade into.
//!
//! Replaces the prior `prop_assert!(true)` placeholder. Distinct from
//! `f11_promotion_refusal` (which pins specific grid thresholds): F15
//! asserts the *hardness/structure* of a denial.

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
fn zero_evidence_production_promotion_is_denied_not_warned() {
    let policy = load_canonical_policy();
    let counts = PassingClassCounts::default(); // no passing validators at all
    let decision = policy.consult(&LifecycleState::Production, &counts, &[]);
    // Hard denial — not an Allow, not an allow-with-warning.
    assert!(
        matches!(decision, PromotionDecision::Deny { .. }),
        "F15 violation: zero-evidence Production promotion was not a hard Deny: {decision:?}"
    );
    // And the denial is *structured*: it enumerates what is missing,
    // rather than degrading to a vague free-text warning.
    if let PromotionDecision::Deny {
        missing_classes,
        missing_approvals,
    } = decision
    {
        assert!(
            !missing_classes.is_empty() || !missing_approvals.is_empty(),
            "F15 violation: a Deny must enumerate missing evidence (classes and/or approvals), \
             not emit an empty/soft denial"
        );
    }
}
