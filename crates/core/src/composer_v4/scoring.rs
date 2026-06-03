//! Stable scoring tuple for the v4 planner.
//!
//! Mirrors the alignment plan's 17-component ordered scoring tuple. `Ord` is derived in declaration order so a tuple
//! compares hard-rejects first, then user-constraint violations,
//! then scientific appropriateness, then goal/modality relevance,
//! etc., with the lexical tie-breaker (`stable_lexical_id`) at the
//! end.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// One component of the scoring tuple. Higher-cost values sort
/// later; the entire tuple compares lexicographically (`Ord`
/// derived in declaration order).
#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    TS,
    Default,
    schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ScoringValue {
    /// Best — no penalty.
    #[default]
    Pass,
    /// Soft penalty.
    Warn,
    /// Hard reject.
    Reject,
}

/// 17-component scoring tuple. Lower is better; comparison is
/// lexicographic.
///
/// Shape is stable; the planner computes each component at plan time.
#[derive(
    Debug,
    Clone,
    Default,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    TS,
    schemars::JsonSchema,
)]
// Forward-compat: a session persisted before a newer scoring component
// was added (e.g. `goal_relevance_penalty`) must still deserialize — any
// field absent on disk falls back to its `Default` (the tuple is recomputed
// at plan time, so a defaulted load value is never authoritative). Matches
// the "forward-compat-by-serde-default" upcast convention in persistence.rs.
#[serde(default)]
#[ts(export)]
pub struct ScoringTuple {
    /// 1. Hard policy violation.
    pub hard_policy_violation: ScoringValue,
    /// 2. Required contract unsatisfied.
    pub required_contract_unsatisfied: ScoringValue,
    /// 3. User-constraint violation (preferred backend, no-network,
    ///    container-only, prefer-reproducible).
    pub user_constraint_violation: ScoringValue,
    /// 4. Scientific appropriateness penalty (claim_boundary /
    ///    method-fit signals).
    pub scientific_appropriateness_penalty: u32,
    /// 5. Goal/modality-relevance penalty (Pillar B). Count of
    ///    goal-IRRELEVANT nodes: nodes whose backing atom is
    ///    affiliated (via the archetype catalog) with a modality
    ///    that the request never asked for and that are not
    ///    synthesis/companion/terminal nodes. Placed AFTER the
    ///    hard-reject / user-constraint / scientific terms and
    ///    BEFORE `untrusted_node_count` so a semantically-aligned
    ///    DAG outranks a structurally-cheaper but off-modality one,
    ///    without masking genuine hard defects. Designed to be `0`
    ///    on every coherent DAG.
    pub goal_relevance_penalty: u32,
    /// 6. Production-trust ratio (lower count of trusted nodes is
    ///    worse). Stored as `u32::MAX - count` so lower is better
    ///    after sort.
    pub untrusted_node_count: u32,
    /// 7. Unresolved blocking-assumption count.
    pub unresolved_assumptions: u32,
    /// 8. Risky-adapter count.
    pub risky_adapter_count: u32,
    /// 9. Total adapter count (lossless + lossy + risky).
    pub total_adapter_count: u32,
    /// 10. Validation coverage score (lower-is-better encoding).
    pub validation_coverage_penalty: u32,
    /// 11. Evidence quality (lower-is-better encoding).
    pub evidence_quality_penalty: u32,
    /// 12. Reproducibility score penalty.
    pub reproducibility_penalty: u32,
    /// 13. Explainability score penalty (Opaque types penalize).
    pub explainability_penalty: u32,
    /// 14. Backend availability penalty.
    pub backend_availability_penalty: u32,
    /// 15. Runtime cost estimate (compute hours).
    pub runtime_cost_estimate: u32,
    /// 16. Data movement cost.
    pub data_movement_cost: u32,
    /// 17. Stable lexical tie-breaker (sorted TaskNodeId chain).
    pub stable_lexical_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passing_tuple_is_least() {
        let pass = ScoringTuple::default();
        let warn = ScoringTuple {
            user_constraint_violation: ScoringValue::Warn,
            ..Default::default()
        };
        let reject = ScoringTuple {
            hard_policy_violation: ScoringValue::Reject,
            ..Default::default()
        };

        assert!(pass < warn);
        assert!(warn < reject);
    }

    #[test]
    fn lexical_tie_break_orders_alphabetically() {
        let a = ScoringTuple {
            stable_lexical_id: "a".into(),
            ..Default::default()
        };
        let b = ScoringTuple {
            stable_lexical_id: "b".into(),
            ..Default::default()
        };
        assert!(a < b);
    }

    #[test]
    fn hard_reject_supersedes_better_other_components() {
        let pass_with_high_cost = ScoringTuple {
            runtime_cost_estimate: 10000,
            ..Default::default()
        };
        let reject_with_low_cost = ScoringTuple {
            hard_policy_violation: ScoringValue::Reject,
            runtime_cost_estimate: 0,
            ..Default::default()
        };
        // Reject sorts later because hard_policy_violation comes
        // first in declaration order.
        assert!(pass_with_high_cost < reject_with_low_cost);
    }

    #[test]
    fn round_trip_through_serde() {
        let tup = ScoringTuple {
            user_constraint_violation: ScoringValue::Warn,
            risky_adapter_count: 2,
            stable_lexical_id: "stage_a:stage_b".into(),
            ..Default::default()
        };
        let json = serde_json::to_string(&tup).unwrap();
        let back: ScoringTuple = serde_json::from_str(&json).unwrap();
        assert_eq!(tup, back);
    }
}
