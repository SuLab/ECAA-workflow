//! Tier F property test for F6 — every compatibility proof has
//! sufficient content. Specifically, the F6 invariant is that every
//! proof names producer output, consumer input, matched facets,
//! ontology path (or local-extension rationale), validators, adapter
//! insertions, and residual assumptions.
//!
//! The ontology-subsumption-path enrichment for `LocalExtension` /
//! `Opaque` semantic types ensures the path is non-empty even for
//! the variants where the compatibility engine's `semantic_compat`
//! returns `Ok(Vec::new())` (same-id `LocalExtension` self-loops,
//! `Opaque` pass-throughs). Property assertions:
//!
//! 1. `ontology_path_for_semantic_type` returns a non-empty vector
//! for every `SemanticType` variant.
//! 2. The path always references something the proof can cite —
//! OntologyTerm.iri, LocalExtension parents+id, or an
//! opaque-hash bucket id.

use proptest::prelude::*;
use ecaa_workflow_core::compatibility::proof_builder::ontology_path_for_semantic_type;
use ecaa_workflow_core::workflow_contracts::semantic_type::{
    LocalExtensionMaturity, SemanticType,
};

fn arb_semantic_type() -> impl Strategy<Value = SemanticType> {
    prop_oneof![
        (any::<u16>(), any::<u16>()).prop_map(|(a, b)| SemanticType::OntologyTerm {
            iri: format!("data:{:04}", a % 10_000),
            label: format!("term_{b}"),
            ontology_version: None,
        }),
        (any::<u8>(), any::<u8>()).prop_map(|(ns, id)| SemanticType::LocalExtension {
            namespace: format!("swfc_{ns}"),
            id: format!("local_{id}"),
            proposed_parent_terms: vec![],
            definition: String::new(),
            maturity: LocalExtensionMaturity::Minted,
        }),
        any::<u32>().prop_map(|n| SemanticType::Opaque {
            description: format!("opaque test type {n}"),
        }),
    ]
}

proptest! {
    #[test]
    fn ontology_path_is_non_empty_for_every_variant(st in arb_semantic_type()) {
        let path = ontology_path_for_semantic_type(&st);
        prop_assert!(
            !path.is_empty(),
            "F6 violation: empty ontology path for {st:?}"
        );
    }
}

#[test]
fn local_extension_with_parents_includes_id_at_end() {
    let st = SemanticType::LocalExtension {
        namespace: "swfc".into(),
        id: "novel_thing".into(),
        proposed_parent_terms: vec!["data:0863".into(), "data:1234".into()],
        definition: "test".into(),
        maturity: LocalExtensionMaturity::Minted,
    };
    let path = ontology_path_for_semantic_type(&st);
    // Path should contain parent terms + namespace:id.
    assert!(path.contains(&"data:0863".to_string()));
    assert!(path.contains(&"data:1234".to_string()));
    assert!(path.contains(&"swfc:novel_thing".to_string()));
}

#[test]
fn opaque_same_description_has_same_bucket_id() {
    let st1 = SemanticType::Opaque {
        description: "profiler failed: corrupted FASTQ header".into(),
    };
    let st2 = SemanticType::Opaque {
        description: "profiler failed: corrupted FASTQ header".into(),
    };
    let p1 = ontology_path_for_semantic_type(&st1);
    let p2 = ontology_path_for_semantic_type(&st2);
    assert_eq!(p1, p2);
    assert!(p1[0].starts_with("opaque:"));
}

#[test]
fn opaque_different_description_has_different_bucket() {
    let st1 = SemanticType::Opaque {
        description: "reason A".into(),
    };
    let st2 = SemanticType::Opaque {
        description: "reason B".into(),
    };
    let p1 = ontology_path_for_semantic_type(&st1);
    let p2 = ontology_path_for_semantic_type(&st2);
    assert_ne!(p1, p2);
}
