//! Tier F property test for F5 — every edge carries a non-empty
//! compatibility proof. Two production edge-construction paths are
//! exercised: the compatibility engine's proof for a type-compatible
//! pair (the proof that gets lowered onto a real `EdgeContract`), and
//! the structural splice edge `EdgeContract::synthetic_splice` used
//! when an intake-fact gate removes an intermediate segment.
//!
//! Replaces the prior `prop_assert!(true)` placeholder.

use ecaa_workflow_core::compatibility::engine::{
    CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine, PlanningContext,
};
use ecaa_workflow_core::workflow_contracts::edge::EdgeContract;
use ecaa_workflow_core::workflow_contracts::port::PortContract;
use ecaa_workflow_core::workflow_contracts::semantic_type::SemanticType;
use proptest::prelude::*;

fn typed_port(name: &str, iri: &str) -> PortContract {
    PortContract {
        name: name.into(),
        semantic_type: SemanticType::edam(iri, "f05"),
        ..Default::default()
    }
}

proptest! {
    /// The proof the engine returns for a compatible pair names both
    /// endpoint types — the `CompatibilityProof` an edge carries is
    /// never empty on the producer/consumer-type axis.
    #[test]
    fn compatible_edge_proof_names_both_types(iri_n in 0u32..9999) {
        let iri = format!("data:{:04}", iri_n);
        let engine = DeterministicCompatibilityEngine::new();
        let result = engine.prove(
            &typed_port("out", &iri),
            &typed_port("in", &iri),
            &PlanningContext::default(),
        );
        let proof = match result {
            CompatibilityResult::Compatible(p) => p,
            CompatibilityResult::CompatibleWithAdapters { proof, .. } => proof,
            other => {
                return Err(TestCaseError::fail(format!(
                    "identical types ({iri}) did not prove compatible: {other:?}"
                )))
            }
        };
        prop_assert!(
            !proof.producer_type.is_empty(),
            "F5 violation: edge proof missing producer_type"
        );
        prop_assert!(
            !proof.consumer_type.is_empty(),
            "F5 violation: edge proof missing consumer_type"
        );
    }
}

#[test]
fn synthetic_splice_edge_carries_a_proof() {
    let e = EdgeContract::synthetic_splice("a".into(), "b".into());
    assert!(
        !e.proof.producer_type.is_empty() && !e.proof.consumer_type.is_empty(),
        "F5 violation: splice edge carries an empty-typed proof"
    );
    assert!(
        e.proof.rationale.is_some(),
        "F5 violation: splice edge must carry a rationale explaining why it exists"
    );
}
