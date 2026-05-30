//! Tier F property test for F3 — a required input with no
//! type-compatible source is never *silently* treated as covered: the
//! engine surfaces it as `Unknown` (needs SME/validator) or
//! `Incompatible`, never as a clean `Compatible`/`CompatibleWithAdapters`.
//! This is the dangling-input guarantee that keeps the planner from
//! fabricating coverage.
//!
//! Replaces the prior `prop_assert!(true)` placeholder. Producer and
//! consumer are drawn from disjoint EDAM id ranges so they are always
//! distinct and carry no curated subsumption/adapter path.

use ecaa_workflow_core::compatibility::engine::{
    CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine, PlanningContext,
};
use ecaa_workflow_core::workflow_contracts::port::PortContract;
use ecaa_workflow_core::workflow_contracts::semantic_type::SemanticType;
use proptest::prelude::*;

fn typed_port(name: &str, iri: &str) -> PortContract {
    PortContract {
        name: name.into(),
        semantic_type: SemanticType::edam(iri, "f03"),
        ..Default::default()
    }
}

proptest! {
    /// A consumer requiring `data:A` whose only candidate producer
    /// emits an unrelated `data:B` (no curated subsumption, no
    /// bridging adapter) must NOT prove `Compatible` or
    /// `CompatibleWithAdapters` — the input is dangling and the engine
    /// must say so rather than silently satisfy it.
    #[test]
    fn unrelated_producer_does_not_silently_cover(a in 0u32..4999, b in 0u32..4999) {
        let producer_iri = format!("data:{:04}", a);
        let consumer_iri = format!("data:{:04}", b + 5000); // disjoint range ⇒ a != b
        let producer = typed_port("out", &producer_iri);
        let consumer = typed_port("in", &consumer_iri);
        let engine = DeterministicCompatibilityEngine::new();
        let result = engine.prove(&producer, &consumer, &PlanningContext::default());
        prop_assert!(
            !matches!(
                result,
                CompatibilityResult::Compatible(_)
                    | CompatibilityResult::CompatibleWithAdapters { .. }
            ),
            "F3 violation: unrelated producer ({producer_iri}) silently covered required \
             input ({consumer_iri}): {result:?}"
        );
    }
}
