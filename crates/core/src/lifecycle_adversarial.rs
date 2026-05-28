//! V3 lifecycle adversarial cases (design §7).
//!
//! Encodes the six non-monotonic lifecycle edges from
//!
//! 1. **Same-user contradiction** — a single actor produces two
//!    confirmation records on the same `assumption_id` with opposite
//!    resolutions. Surfaces a `LifecycleTransition::SameUserContradiction`
//!    so the adjudication queue carries both record ids and the SME
//!    (or operator) decides which one stands.
//!
//! 2. **Cross-user conflict** — two different actors record opposite
//!    resolutions on the same `assumption_id`. Different from
//!    same-user because the recovery affordance is operator review,
//!    not auto-resolve.
//!
//! 3. **Upstream invalidation** — the contract under a previously
//!    resolved assumption changed. The production
//!    `cascade_invalidate_assumptions` already handles this edge
//!    (it resets `AssumptionResolution::Unresolved` and emits
//!    `DecisionType::AssumptionInvalidated`); the
//!    `LifecycleTransition::UpstreamInvalidation` shape is included
//!    here for completeness so the property suite has a uniform
//!    fixture surface across all six edges.
//!
//! 4. **Forbidden waiver attempt** — an actor tried to waive an
//!    assumption whose `(defect_class, privacy_class)` resolves to
//!    `ResolutionPolicy::Blocking`. The waiver is rejected outright.
//!
//! 5. **Verifier-discovered unresolvability** — a downstream verifier
//!    discovered an assumption is unresolvable regardless of waivers
//!    or operator intervention (e.g. the data simply can't satisfy
//!    the requirement). The adjudication entry routes to operator
//!    review with a typed reason.
//!
//! 6. **Production-node revocation** — a `LifecycleState::Production`
//!    node's promotion authority retroactively revoked the
//!    production rating (e.g. a CVE in the implementation, an
//!    upstream registry yanked the digest). Every downstream DAG
//!    that depended on the node enters adjudication so the SME
//!    decides whether to re-emit on a substitute, accept the
//!    demoted draft, or refuse to run.
//!
//! Surfaced through `BlockerKind::AdjudicationRequired` so the UI
//! `LifecycleAdjudicationCard` can render the queue + Resolve
//! action.
//!
//! Substrate emission (v4 P2 just; v4 P3 will wrap these
//! events) is intentionally **not** wired here — that's a follow-up.
//! These detections write to the decision log (via the
//! `LifecycleTransition`, `ContradictionDetected`, and
//! `InvalidationCascaded` decision-log variants) plus the
//! session-scoped adjudication queue. v4 P3 lifts the same data
//! into the decision substrate stream when it lands.

use crate::workflow_contracts::policy_rule_id::PolicyRuleId;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The six non-monotonic lifecycle edges from v3 §7.
///
/// `#[serde(tag = "kind", rename_all = "snake_case")]` produces flat
/// JSON shapes like `{"kind":"same_user_contradiction","actor":"alan",
///...}` so wire consumers can pattern-match on the discriminator
/// without inspecting the payload first.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LifecycleTransition {
    /// A single actor authored two confirmation records on the same
    /// `assumption_id` with opposite resolutions. The newer record
    /// contradicts the prior; both ids are recorded so adjudication
    /// can present a side-by-side picker.
    SameUserContradiction {
        /// The actor who authored both records.
        actor: String,
        /// The `assumption_id` both records reference.
        assumption_id: String,
        /// `DecisionRecord` id of the prior resolution.
        prior_record_id: String,
        /// `DecisionRecord` id of the contradicting resolution.
        new_record_id: String,
    },
    /// Two different actors recorded opposite resolutions on the
    /// same `assumption_id`. `records` lists every `DecisionRecord`
    /// id involved so the adjudication entry carries the full
    /// disagreement context.
    CrossUserConflict {
        /// First actor.
        actor_a: String,
        /// Second actor.
        actor_b: String,
        /// The `assumption_id` both actors are arguing over.
        assumption_id: String,
        /// `DecisionRecord` ids participating in the conflict.
        records: Vec<String>,
    },
    /// The contract under a previously resolved assumption changed.
    /// `cascade_invalidate_assumptions` is the production
    /// detection path; this shape exists so the property suite has a
    /// uniform fixture across all six edges.
    UpstreamInvalidation {
        /// The `assumption_id` whose contract changed.
        assumption_id: String,
        /// Short description of what changed under the assumption
        /// (e.g. `"affects_nodes: [old] → [new]"`).
        invalidating_change: String,
        /// Task node ids the change propagates to.
        affected_downstream: Vec<String>,
    },
    /// An actor tried to waive an assumption whose policy rule is
    /// `ResolutionPolicy::Blocking`. The waiver is rejected and the
    /// adjudication entry routes to operator review with the
    /// rejected actor + policy-rule id recorded.
    ForbiddenWaiverAttempt {
        /// Actor who attempted the waiver.
        actor: String,
        /// The `assumption_id` the actor tried to waive.
        assumption_id: String,
        /// `(defect_class, privacy_class)` policy-rule id from
        /// `AssumptionPolicyTable` (e.g. `"genome_build_mismatch:clinical"`).
        /// V3 §10.2 registry-validated when minted via
        /// [`PolicyRuleId::new`]; legacy ids round-trip via the
        /// permissive deserialization path.
        policy_rule_id: PolicyRuleId,
    },
    /// A downstream verifier discovered an assumption is
    /// unresolvable regardless of waivers or operator intervention.
    /// `verifier` is the verifier identifier (typically a stage id
    /// or task id); `reason` is the verifier's free-text statement.
    VerifierUnresolvability {
        /// The `assumption_id` the verifier flagged.
        assumption_id: String,
        /// Verifier identifier (stage id, task id, validator id).
        verifier: String,
        /// Free-text reason from the verifier.
        reason: String,
    },
    /// A `LifecycleState::Production` node's promotion authority
    /// retroactively revoked the production rating. Every downstream
    /// DAG that depended on the node enters adjudication.
    ProductionNodeRevocation {
        /// The `TaskNode` id whose production rating was revoked.
        node_id: String,
        /// Stringified `LifecycleState` the node was in before the
        /// revocation (typically `"production"`).
        prior_state: String,
        /// Free-text reason for the revocation (CVE id, registry
        /// yank notice, deprecation citation).
        reason: String,
        /// DAG ids that depended on the revoked node.
        affected_dags: Vec<String>,
    },
}

impl LifecycleTransition {
    /// Short snake_case discriminator. Used by
    /// `BlockerKind::AdjudicationRequired::transition_kind` so the
    /// UI dispatch table can pick the right card without unpacking
    /// the full payload.
    pub fn kind(&self) -> &'static str {
        match self {
            LifecycleTransition::SameUserContradiction { .. } => "same_user_contradiction",
            LifecycleTransition::CrossUserConflict { .. } => "cross_user_conflict",
            LifecycleTransition::UpstreamInvalidation { .. } => "upstream_invalidation",
            LifecycleTransition::ForbiddenWaiverAttempt { .. } => "forbidden_waiver_attempt",
            LifecycleTransition::VerifierUnresolvability { .. } => "verifier_unresolvability",
            LifecycleTransition::ProductionNodeRevocation { .. } => "production_node_revocation",
        }
    }

    /// The primary affected node/assumption identifier on a
    /// lifecycle transition. Used by the v4 P3 verifier-decision
    /// substrate (`VerifierDecision::LifecycleAdversarialEdgeDetected`)
    /// so a `grep` of `runtime/verifier-decisions.jsonl` can locate
    /// every event touching a given node without unpacking the
    /// transition's variant-specific payload.
    ///
    /// For variants that primarily reference an `assumption_id`
    /// (`SameUserContradiction`, `CrossUserConflict`,
    /// `UpstreamInvalidation`, `ForbiddenWaiverAttempt`,
    /// `VerifierUnresolvability`) this returns the assumption id.
    /// For `ProductionNodeRevocation` it returns the revoked
    /// `node_id`.
    pub fn affected_node_id(&self) -> &str {
        match self {
            LifecycleTransition::SameUserContradiction { assumption_id, .. } => assumption_id,
            LifecycleTransition::CrossUserConflict { assumption_id, .. } => assumption_id,
            LifecycleTransition::UpstreamInvalidation { assumption_id, .. } => assumption_id,
            LifecycleTransition::ForbiddenWaiverAttempt { assumption_id, .. } => assumption_id,
            LifecycleTransition::VerifierUnresolvability { assumption_id, .. } => assumption_id,
            LifecycleTransition::ProductionNodeRevocation { node_id, .. } => node_id,
        }
    }

    /// One-line narrative summarizing the transition. Used by the
    /// v4 P3 verifier-decision substrate
    /// (`VerifierDecision::LifecycleAdversarialEdgeDetected::rationale`)
    /// so the UI's `LifecycleAdjudicationCard` can render a humane
    /// summary without unpacking the variant-specific payload.
    pub fn rationale(&self) -> String {
        match self {
            LifecycleTransition::SameUserContradiction {
                actor,
                assumption_id,
                ..
            } => format!(
                "actor '{}' authored two opposing resolutions on assumption '{}'",
                actor, assumption_id
            ),
            LifecycleTransition::CrossUserConflict {
                actor_a,
                actor_b,
                assumption_id,
                ..
            } => format!(
                "actors '{}' and '{}' disagree on assumption '{}'",
                actor_a, actor_b, assumption_id
            ),
            LifecycleTransition::UpstreamInvalidation {
                assumption_id,
                invalidating_change,
                ..
            } => format!(
                "assumption '{}' invalidated by upstream change: {}",
                assumption_id, invalidating_change
            ),
            LifecycleTransition::ForbiddenWaiverAttempt {
                actor,
                assumption_id,
                policy_rule_id,
            } => format!(
                "actor '{}' attempted to waive assumption '{}' under blocking policy '{}'",
                actor, assumption_id, policy_rule_id
            ),
            LifecycleTransition::VerifierUnresolvability {
                assumption_id,
                verifier,
                reason,
            } => format!(
                "verifier '{}' flagged assumption '{}' as unresolvable: {}",
                verifier, assumption_id, reason
            ),
            LifecycleTransition::ProductionNodeRevocation {
                node_id,
                prior_state,
                reason,
                ..
            } => format!(
                "production node '{}' (prior state '{}') revoked: {}",
                node_id, prior_state, reason
            ),
        }
    }
}

/// One entry on the session-scoped adjudication queue. Persisted on
/// `Session::adjudication_queue`; surfaced through
/// `BlockerKind::AdjudicationRequired` and the
/// `LifecycleAdjudicationCard` UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct AdjudicationQueueEntry {
    /// Stable id (`adj_<12hex>` per session). Used by the
    /// `POST /api/chat/session/:id/adjudication/:entry_id/resolve`
    /// endpoint and the `BlockerKind::AdjudicationRequired::queue_entry_id`
    /// field.
    pub id: String,
    /// ISO-8601 UTC timestamp the entry was queued.
    pub created_at: String,
    /// The lifecycle transition that triggered the queue entry.
    pub transition: LifecycleTransition,
    /// Resolution status. Defaults to `Open` at creation.
    pub status: AdjudicationStatus,
}

/// Resolution status of an `AdjudicationQueueEntry`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdjudicationStatus {
    /// Queued, awaiting SME / operator review.
    Open,
    /// Resolved by an actor with a recorded decision.
    Resolved {
        /// Who resolved the entry.
        decided_by: String,
        /// Free-text decision narrative.
        decision: String,
        /// ISO-8601 UTC timestamp.
        decided_at: String,
    },
    /// Escalated to operator review (no SME-level recovery
    /// affordance applies).
    DeferredToOperator {
        /// Free-text reason for the escalation.
        reason: String,
    },
}

impl AdjudicationStatus {
    /// True when this status represents a fully-resolved (or
    /// operator-deferred) entry. `Open` is the only non-resolved
    /// status.
    pub fn is_terminal(&self) -> bool {
        !matches!(self, AdjudicationStatus::Open)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_transition_kind_is_stable() {
        let t = LifecycleTransition::SameUserContradiction {
            actor: "alan".into(),
            assumption_id: "a_1".into(),
            prior_record_id: "rec_1".into(),
            new_record_id: "rec_2".into(),
        };
        assert_eq!(t.kind(), "same_user_contradiction");
    }

    #[test]
    fn every_transition_kind_round_trips() {
        let cases = vec![
            LifecycleTransition::SameUserContradiction {
                actor: "alan".into(),
                assumption_id: "a_1".into(),
                prior_record_id: "rec_1".into(),
                new_record_id: "rec_2".into(),
            },
            LifecycleTransition::CrossUserConflict {
                actor_a: "alan".into(),
                actor_b: "bob".into(),
                assumption_id: "a_1".into(),
                records: vec!["rec_1".into(), "rec_2".into()],
            },
            LifecycleTransition::UpstreamInvalidation {
                assumption_id: "a_1".into(),
                invalidating_change: "affects_nodes: [old] → [new]".into(),
                affected_downstream: vec!["task_2".into()],
            },
            LifecycleTransition::ForbiddenWaiverAttempt {
                actor: "alan".into(),
                assumption_id: "a_1".into(),
                policy_rule_id: PolicyRuleId::unchecked("genome_build_mismatch:clinical"),
            },
            LifecycleTransition::VerifierUnresolvability {
                assumption_id: "a_1".into(),
                verifier: "validate_qc".into(),
                reason: "input data lacks strandedness metadata".into(),
            },
            LifecycleTransition::ProductionNodeRevocation {
                node_id: "align_reads".into(),
                prior_state: "production".into(),
                reason: "CVE-2026-12345 in STAR 2.7.11a".into(),
                affected_dags: vec!["dag_1".into()],
            },
        ];
        for t in cases {
            let json = serde_json::to_string(&t).expect("serialize");
            let back: LifecycleTransition = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(t, back);
        }
    }

    #[test]
    fn adjudication_status_is_terminal_for_resolved_and_deferred() {
        assert!(!AdjudicationStatus::Open.is_terminal());
        assert!(AdjudicationStatus::Resolved {
            decided_by: "alan".into(),
            decision: "accepted".into(),
            decided_at: "2026-05-11T00:00:00Z".into(),
        }
        .is_terminal());
        assert!(AdjudicationStatus::DeferredToOperator {
            reason: "too complex".into(),
        }
        .is_terminal());
    }

    #[test]
    fn queue_entry_round_trips() {
        let e = AdjudicationQueueEntry {
            id: "adj_abc123def456".into(),
            created_at: "2026-05-11T00:00:00Z".into(),
            transition: LifecycleTransition::SameUserContradiction {
                actor: "alan".into(),
                assumption_id: "a_1".into(),
                prior_record_id: "rec_1".into(),
                new_record_id: "rec_2".into(),
            },
            status: AdjudicationStatus::Open,
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: AdjudicationQueueEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    /// The serde shape is internally-tagged + snake_case so wire
    /// consumers can dispatch on the discriminator alone.
    #[test]
    fn lifecycle_transition_serde_shape_is_flat() {
        let t = LifecycleTransition::ProductionNodeRevocation {
            node_id: "n1".into(),
            prior_state: "production".into(),
            reason: "yank".into(),
            affected_dags: vec!["d1".into()],
        };
        let v: serde_json::Value = serde_json::to_value(&t).unwrap();
        assert_eq!(v["kind"], "production_node_revocation");
        assert_eq!(v["node_id"], "n1");
    }
}
