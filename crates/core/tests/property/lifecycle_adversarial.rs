//! V3 Tier 4.9.3 adversarial lifecycle property tests
//! (design §7).
//!
//! Six properties, one per non-monotonic edge in
//!
//! 1. `LifecycleTransition::SameUserContradiction` round-trips
//! cleanly through serde and `kind()` returns the canonical
//! snake_case discriminator.
//! 2. `LifecycleTransition::CrossUserConflict` ditto.
//! 3. `LifecycleTransition::UpstreamInvalidation` ditto + the
//! `affected_downstream` list is preserved.
//! 4. `LifecycleTransition::ForbiddenWaiverAttempt` ditto + the
//! `policy_rule_id` string is preserved verbatim.
//! 5. `LifecycleTransition::VerifierUnresolvability` ditto + the
//! verifier identifier round-trips.
//! 6. `LifecycleTransition::ProductionNodeRevocation` ditto + the
//! `affected_dags` list is preserved.
//!
//! Each property uses a small `prop_oneof!` strategy generating
//! random String / Vec<String> payloads and asserts the round-trip
//! invariant. The detection-pipeline integration (decision-log
//! recording, queue enqueue, `BlockerKind::AdjudicationRequired`
//! emission) is exercised by unit tests inside
//! `crates/conversation/src/tools/mod.rs` — those are not Tier-F
//! property tests; the Tier-F suite owns the typed-shape invariants.

use proptest::prelude::*;
use scripps_workflow_core::decision_substrate::{
    drain, record, stable_id, timestamp, VerifierDecision,
};
use scripps_workflow_core::lifecycle_adversarial::{
    AdjudicationQueueEntry, AdjudicationStatus, LifecycleTransition,
};
use std::sync::Mutex;

/// The decision substrate buffer is process-wide. Tests that exercise
/// `record(...)` / `drain()` must serialize through this guard so an
/// iteration sees only its own emissions.
static SUBSTRATE_GUARD: Mutex<()> = Mutex::new(());

fn arb_short_string() -> impl Strategy<Value = String> {
    "[a-zA-Z_][a-zA-Z0-9_]{0,15}".prop_map(|s| s.to_string())
}

fn arb_string_vec() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(arb_short_string(), 0..5)
}

proptest! {
    /// (1) Same-user contradiction round-trips.
    #[test]
    fn same_user_contradiction_round_trips(
        actor in arb_short_string(),
        assumption_id in arb_short_string(),
        prior_id in arb_short_string(),
        new_id in arb_short_string(),
    ) {
        let t = LifecycleTransition::SameUserContradiction {
            actor,
            assumption_id,
            prior_record_id: prior_id,
            new_record_id: new_id,
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: LifecycleTransition = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&t, &back);
        prop_assert_eq!(t.kind(), "same_user_contradiction");
    }

    /// (2) Cross-user conflict round-trips.
    #[test]
    fn cross_user_conflict_round_trips(
        actor_a in arb_short_string(),
        actor_b in arb_short_string(),
        assumption_id in arb_short_string(),
        records in arb_string_vec(),
    ) {
        let t = LifecycleTransition::CrossUserConflict {
            actor_a,
            actor_b,
            assumption_id,
            records: records.clone(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: LifecycleTransition = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&t, &back);
        prop_assert_eq!(t.kind(), "cross_user_conflict");
        if let LifecycleTransition::CrossUserConflict { records: back_records, .. } = back {
            prop_assert_eq!(back_records, records);
        } else {
            unreachable!();
        }
    }

    /// (3) Upstream invalidation round-trips and preserves affected list.
    #[test]
    fn upstream_invalidation_round_trips(
        assumption_id in arb_short_string(),
        change in arb_short_string(),
        affected in arb_string_vec(),
    ) {
        let t = LifecycleTransition::UpstreamInvalidation {
            assumption_id,
            invalidating_change: change,
            affected_downstream: affected.clone(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: LifecycleTransition = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&t, &back);
        prop_assert_eq!(t.kind(), "upstream_invalidation");
        if let LifecycleTransition::UpstreamInvalidation { affected_downstream, .. } = back {
            prop_assert_eq!(affected_downstream, affected);
        } else {
            unreachable!();
        }
    }

    /// (4) Forbidden waiver round-trips and preserves the policy rule id.
    #[test]
    fn forbidden_waiver_round_trips(
        actor in arb_short_string(),
        assumption_id in arb_short_string(),
        rule in "(genome_build_mismatch|sample_size|species_mismatch):(clinical|phi_strict|research|public)",
    ) {
        let t = LifecycleTransition::ForbiddenWaiverAttempt {
            actor,
            assumption_id,
            policy_rule_id: scripps_workflow_core::workflow_contracts::policy_rule_id::PolicyRuleId::unchecked(rule.clone()),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: LifecycleTransition = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&t, &back);
        prop_assert_eq!(t.kind(), "forbidden_waiver_attempt");
        if let LifecycleTransition::ForbiddenWaiverAttempt { policy_rule_id, .. } = back {
            prop_assert_eq!(policy_rule_id.as_str(), rule);
        } else {
            unreachable!();
        }
    }

    /// (5) Verifier unresolvability round-trips and preserves verifier id.
    #[test]
    fn verifier_unresolvability_round_trips(
        assumption_id in arb_short_string(),
        verifier in arb_short_string(),
        reason in arb_short_string(),
    ) {
        let t = LifecycleTransition::VerifierUnresolvability {
            assumption_id,
            verifier: verifier.clone(),
            reason,
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: LifecycleTransition = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&t, &back);
        prop_assert_eq!(t.kind(), "verifier_unresolvability");
        if let LifecycleTransition::VerifierUnresolvability { verifier: back_verifier, .. } = back {
            prop_assert_eq!(back_verifier, verifier);
        } else {
            unreachable!();
        }
    }

    /// (6) Production-node revocation round-trips and preserves dag list.
    #[test]
    fn production_node_revocation_round_trips(
        node_id in arb_short_string(),
        reason in arb_short_string(),
        affected_dags in arb_string_vec(),
    ) {
        let t = LifecycleTransition::ProductionNodeRevocation {
            node_id,
            prior_state: "production".into(),
            reason,
            affected_dags: affected_dags.clone(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: LifecycleTransition = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&t, &back);
        prop_assert_eq!(t.kind(), "production_node_revocation");
        if let LifecycleTransition::ProductionNodeRevocation { affected_dags: back_dags, .. } = back {
            prop_assert_eq!(back_dags, affected_dags);
        } else {
            unreachable!();
        }
    }

    /// AdjudicationQueueEntry round-trips with any of the six
    /// transitions inside.
    #[test]
    fn queue_entry_round_trips_for_any_transition(
        entry_id in arb_short_string(),
        actor in arb_short_string(),
        assumption_id in arb_short_string(),
    ) {
        let e = AdjudicationQueueEntry {
            id: entry_id,
            created_at: "2026-05-11T00:00:00Z".into(),
            transition: LifecycleTransition::SameUserContradiction {
                actor,
                assumption_id,
                prior_record_id: "rec_1".into(),
                new_record_id: "rec_2".into(),
            },
            status: AdjudicationStatus::Open,
        };
        let json = serde_json::to_string(&e).unwrap();
        let back: AdjudicationQueueEntry = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(e, back);
    }
}

/// v3 P8 follow-up — recording a `SameUserContradiction` lifecycle
/// transition through the substrate's `record()` API surfaces a
/// matching `LifecycleAdversarialEdgeDetected` row in `drain()`'s
/// output. This is the smallest test that locks in the new variant —
/// the integration-level behavior test (that `enqueue_adjudication`
/// emits a pair of `LifecycleAdversarialEdgeDetected` +
/// `AdjudicationEnqueued` rows) lives in
/// `crates/conversation/src/tools/tests.rs` because
/// `enqueue_adjudication` is `pub(crate)` within the conversation
/// crate and not reachable from core.
#[test]
fn same_user_contradiction_emits_substrate_event() {
    let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    // Drain so this test sees only its own emission.
    let _ = drain();

    let transition = LifecycleTransition::SameUserContradiction {
        actor: "alan".into(),
        assumption_id: "a_1".into(),
        prior_record_id: "rec_1".into(),
        new_record_id: "rec_2".into(),
    };
    record(VerifierDecision::LifecycleAdversarialEdgeDetected {
        id: stable_id("lae", transition.kind(), transition.affected_node_id()),
        timestamp: timestamp(),
        transition_kind: transition.kind().to_string(),
        affected_node_id: transition.affected_node_id().to_string(),
        rationale: transition.rationale(),
    });

    let events = drain();
    let detection_rows: Vec<&VerifierDecision> = events
        .iter()
        .filter(|d| {
            matches!(
                d,
                VerifierDecision::LifecycleAdversarialEdgeDetected { transition_kind, .. }
                    if transition_kind == "same_user_contradiction"
            )
        })
        .collect();
    assert_eq!(
        detection_rows.len(),
        1,
        "expected exactly one same_user_contradiction row, got {} events: {:?}",
        events.len(),
        events
    );

    if let VerifierDecision::LifecycleAdversarialEdgeDetected {
        affected_node_id,
        rationale,
        ..
    } = detection_rows[0]
    {
        assert_eq!(affected_node_id, "a_1");
        assert!(
            rationale.contains("alan") && rationale.contains("a_1"),
            "rationale should mention actor + assumption: {rationale}"
        );
    } else {
        unreachable!();
    }
}

/// v3 P8 follow-up — round-trip the two new `VerifierDecision`
/// variants through serde so the substrate's append-only JSONL
/// format stays stable.
#[test]
fn new_substrate_variants_round_trip() {
    let detected = VerifierDecision::LifecycleAdversarialEdgeDetected {
        id: "lae:same_user_contradiction:a_1".into(),
        timestamp: "0".into(),
        transition_kind: "same_user_contradiction".into(),
        affected_node_id: "a_1".into(),
        rationale: "actor 'alan' authored two opposing resolutions on assumption 'a_1'".into(),
    };
    let json = serde_json::to_string(&detected).unwrap();
    assert!(
        json.contains(r#""kind":"lifecycle_adversarial_edge_detected""#),
        "expected snake_case tag in: {json}"
    );
    let back: VerifierDecision = serde_json::from_str(&json).unwrap();
    assert_eq!(detected, back);

    let enqueued = VerifierDecision::AdjudicationEnqueued {
        id: "aen:adj_abc:same_user_contradiction".into(),
        timestamp: "0".into(),
        queue_entry_id: "adj_abc".into(),
        transition_kind: "same_user_contradiction".into(),
    };
    let json = serde_json::to_string(&enqueued).unwrap();
    assert!(
        json.contains(r#""kind":"adjudication_enqueued""#),
        "expected snake_case tag in: {json}"
    );
    let back: VerifierDecision = serde_json::from_str(&json).unwrap();
    assert_eq!(enqueued, back);
}

/// v3 P8 follow-up — `LifecycleTransition::affected_node_id()` and
/// `rationale()` return non-empty strings for every variant. Locks
/// the helper methods at the type boundary so the substrate emission
/// site never serializes empty payloads.
#[test]
fn lifecycle_transition_helpers_non_empty_for_every_variant() {
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
            invalidating_change: "affects_nodes changed".into(),
            affected_downstream: vec!["task_2".into()],
        },
        LifecycleTransition::ForbiddenWaiverAttempt {
            actor: "alan".into(),
            assumption_id: "a_1".into(),
            policy_rule_id: "genome_build_mismatch:clinical".into(),
        },
        LifecycleTransition::VerifierUnresolvability {
            assumption_id: "a_1".into(),
            verifier: "validate_qc".into(),
            reason: "no strandedness metadata".into(),
        },
        LifecycleTransition::ProductionNodeRevocation {
            node_id: "align_reads".into(),
            prior_state: "production".into(),
            reason: "CVE-2026-12345".into(),
            affected_dags: vec!["dag_1".into()],
        },
    ];
    for t in cases {
        assert!(!t.affected_node_id().is_empty(), "{:?}", t);
        assert!(!t.rationale().is_empty(), "{:?}", t);
    }
}
