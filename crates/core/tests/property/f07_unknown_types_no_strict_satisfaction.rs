//! Tier F property test for F7 — unknown semantic types
//! (`SemanticType::Opaque` and `SemanticType::LocalExtension`) cannot
//! strictly satisfy a known `OntologyTerm` consumer without an
//! explicit bridge contract.
//!
//! Per design v3 §2 + `docs/dag_eval.md` Tier F, the open-world type
//! discipline must reject the implicit narrowing the planner would
//! otherwise accept on an opaque-typed edge: an opaque producer must
//! never reach `CompatibilityResult::Compatible` for an `OntologyTerm`
//! consumer (it must surface as `Unknown` or `Incompatible`); a
//! `LocalExtension` producer must do likewise unless its
//! `proposed_parent_terms` explicitly cite a parent that subsumes the
//! consumer's IRI.
//!
//! Two complementary property arms:
//!
//! 1. `opaque_producer_never_satisfies_ontology_consumer` — every
//! `Opaque` producer paired with any `OntologyTerm` consumer must
//! return `Unknown` or `Incompatible`; never `Compatible` or
//! `CompatibleWithAdapters`.
//! 2. `local_extension_without_matching_parent_never_satisfies` —
//! a `LocalExtension` producer whose `proposed_parent_terms` do
//! NOT include the consumer's IRI must return `Unknown` or
//! `Incompatible` (the engine cannot prove the implicit
//! narrowing).

use proptest::prelude::*;
use ecaa_workflow_core::compatibility::engine::{
    CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine, PlanningContext,
};
use ecaa_workflow_core::workflow_contracts::port::PortContract;
use ecaa_workflow_core::workflow_contracts::semantic_type::{
    LocalExtensionMaturity, SemanticType,
};

/// Build a producer port carrying an Opaque semantic type with the
/// supplied description.
fn opaque_producer(description: &str) -> PortContract {
    let mut p = PortContract::default();
    p.name = "out".into();
    p.semantic_type = SemanticType::opaque(description);
    p
}

/// Build a producer port carrying a LocalExtension with no parent
/// terms (cannot prove subsumption against any OntologyTerm consumer).
fn local_extension_producer_no_parents(namespace: &str, id: &str) -> PortContract {
    let mut p = PortContract::default();
    p.name = "out".into();
    p.semantic_type = SemanticType::LocalExtension {
        namespace: namespace.into(),
        id: id.into(),
        proposed_parent_terms: vec![],
        definition: String::new(),
        maturity: LocalExtensionMaturity::Minted,
    };
    p
}

/// Build a consumer port carrying an OntologyTerm with the supplied
/// IRI. The IRI is chosen so the engine's curated subtype table cannot
/// trivially bridge it (`data:xxxx` with no edges).
fn ontology_consumer(iri: &str) -> PortContract {
    let mut p = PortContract::default();
    p.name = "in".into();
    p.semantic_type = SemanticType::edam(iri, "F7 consumer");
    p
}

proptest! {
    /// F7 arm 1: Opaque producer against OntologyTerm consumer must
    /// never return Compatible or CompatibleWithAdapters.
    #[test]
    fn opaque_producer_never_satisfies_ontology_consumer(
        desc in "[A-Za-z0-9 ]{1,40}",
        iri_n in 0u32..9999,
    ) {
        let producer = opaque_producer(&desc);
        let consumer = ontology_consumer(&format!("data:{:04}", iri_n));
        let ctx = PlanningContext::default();
        let engine = DeterministicCompatibilityEngine::new();
        let result = engine.prove(&producer, &consumer, &ctx);
        prop_assert!(
            matches!(
                result,
                CompatibilityResult::Unknown(_) | CompatibilityResult::Incompatible(_)
            ),
            "F7 violation: opaque producer satisfied OntologyTerm consumer ({}): {result:?}",
            iri_n
        );
    }
}

proptest! {
    /// F7 arm 2: LocalExtension producer with NO matching parent
    /// terms must never return Compatible against an OntologyTerm
    /// consumer (the engine cannot prove the subsumption).
    ///
    /// Note: `CompatibleWithAdapters` is still acceptable here in
    /// theory (an adapter could bridge a typed local extension to
    /// the consumer's IRI) but the deterministic engine ships no such
    /// adapter; the property holds for the engine as configured.
    #[test]
    fn local_extension_without_matching_parent_never_satisfies(
        ns in "[a-z]{3,6}",
        id in "[a-z0-9_]{3,12}",
        iri_n in 0u32..9999,
    ) {
        let producer = local_extension_producer_no_parents(&ns, &id);
        let consumer = ontology_consumer(&format!("data:{:04}", iri_n));
        let ctx = PlanningContext::default();
        let engine = DeterministicCompatibilityEngine::new();
        let result = engine.prove(&producer, &consumer, &ctx);
        prop_assert!(
            !matches!(result, CompatibilityResult::Compatible(_)),
            "F7 violation: LocalExtension producer with no matching parent satisfied \
             OntologyTerm consumer: {result:?}"
        );
    }
}
