//! Tier F property tests for F22: ontology resolution is scoped to
//! the modality coverage matrix per v4 design §2.6.
//!
//! Annotation-only enforcement today (v4 P1) — these properties verify
//! determinism and the typed 4-outcome shape. v4 P2 will tighten the
//! property to assert substrate emission of `OntologyScopeChecked` for
//! forbidden-ontology proposals.

use proptest::prelude::*;
use ecaa_workflow_core::ontology_scope::{OntologyScopeMatrix, ScopeCheck};
use ecaa_workflow_core::workflow_contracts::workflow_intent::BioinformaticsModality;

proptest! {
    /// Same (modality × prefix) lookup returns the same outcome
    /// across replays, and the outcome is always one of the four
    /// typed `ScopeCheck` variants.
    #[test]
    fn known_ontology_resolves_deterministically(
        modality in prop_oneof![
            Just(BioinformaticsModality::BulkRnaseq),
            Just(BioinformaticsModality::SingleCellRnaseq),
            Just(BioinformaticsModality::Proteomics),
            Just(BioinformaticsModality::Metagenomics),
            Just(BioinformaticsModality::GenericOmics),
        ],
        ontology_prefix in prop_oneof![
            Just("GO"), Just("CL"), Just("SO"), Just("EDAM"),
            Just("PR"), Just("ChEBI"), Just("MONDO"), Just("UBERON"),
            Just("NCBITaxon"), Just("ENVO"), Just("OBI"),
        ],
    ) {
        let matrix = OntologyScopeMatrix::load_from_path(
            "../../config/modality-ontology-coverage.yaml"
        ).expect("matrix loads");
        // Same input → same output across two calls.
        let a = matrix.check(&modality, ontology_prefix);
        let b = matrix.check(&modality, ontology_prefix);
        prop_assert_eq!(a.clone(), b);
        // Must resolve to one of the 4 typed outcomes.
        prop_assert!(matches!(a,
            ScopeCheck::InPrimary | ScopeCheck::InSecondary
            | ScopeCheck::Forbidden | ScopeCheck::OutOfScope));
    }

    /// IRI prefix extraction is deterministic across PURL + compact
    /// forms and always yields `GO` for `GO_<digits>` / `GO:<digits>`
    /// IRIs.
    #[test]
    fn prefix_extraction_is_deterministic(suffix in "[0-9]{4,8}") {
        let purl = format!("http://purl.obolibrary.org/obo/GO_{}", suffix);
        let compact = format!("GO:{}", suffix);
        let purl_prefix = OntologyScopeMatrix::prefix_of_iri(&purl);
        let compact_prefix = OntologyScopeMatrix::prefix_of_iri(&compact);
        prop_assert_eq!(purl_prefix.as_deref(), Some("GO"));
        prop_assert_eq!(compact_prefix.as_deref(), Some("GO"));
    }
}
