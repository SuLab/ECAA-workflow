//! Tier F property test for F2 — a required input port is *covered*
//! when an upstream producer carries a type-compatible output. Drives
//! the production compatibility engine: a consumer input typed
//! `data:NNNN` is provably satisfied by a producer output of the
//! identical ontology term, across generated EDAM ids.
//!
//! Replaces the prior `prop_assert!(true)` placeholder. The companion
//! negative property (a required input with no compatible source is
//! *not* silently covered) lives in `f03_no_dangling_required`.

use ecaa_workflow_core::compatibility::engine::{
    CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine, PlanningContext,
};
use ecaa_workflow_core::workflow_contracts::port::PortContract;
use ecaa_workflow_core::workflow_contracts::semantic_type::SemanticType;
use proptest::prelude::*;

/// Build a port carrying the given EDAM ontology term.
fn typed_port(name: &str, iri: &str) -> PortContract {
    PortContract {
        name: name.into(),
        semantic_type: SemanticType::edam(iri, "f02"),
        ..Default::default()
    }
}

proptest! {
    /// A required input typed `data:NNNN` is covered by a producer
    /// output of the identical type — the engine proves the edge holds
    /// (`Compatible` or `CompatibleWithAdapters`), never leaving the
    /// input uncovered (`Unknown`/`Incompatible`).
    #[test]
    fn identical_type_producer_covers_required_input(iri_n in 0u32..9999) {
        let iri = format!("data:{:04}", iri_n);
        let producer = typed_port("out", &iri);
        let consumer = typed_port("in", &iri);
        let engine = DeterministicCompatibilityEngine::new();
        let result = engine.prove(&producer, &consumer, &PlanningContext::default());
        prop_assert!(
            matches!(
                result,
                CompatibilityResult::Compatible(_)
                    | CompatibilityResult::CompatibleWithAdapters { .. }
            ),
            "F2 violation: identical-type producer did not cover required input ({iri}): {result:?}"
        );
    }
}
