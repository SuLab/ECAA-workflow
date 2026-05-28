//! V4 loader integration tests for the modality-ontology
//! coverage matrix.

use ecaa_workflow_core::ontology_scope::*;
use ecaa_workflow_core::workflow_contracts::workflow_intent::BioinformaticsModality;

#[test]
fn loads_canonical_matrix() {
    let m = OntologyScopeMatrix::load_from_path("../../config/modality-ontology-coverage.yaml")
        .expect("matrix loads");
    assert_eq!(m.version, "1.0.0");
    assert_eq!(m.coverage.len(), 12);
}

#[test]
fn canonical_matrix_resolves_known_pairs() {
    let m = OntologyScopeMatrix::load_from_path("../../config/modality-ontology-coverage.yaml")
        .expect("matrix loads");
    // Bulk RNA-seq primary set contains GO + SO + EDAM.
    assert_eq!(
        m.check(&BioinformaticsModality::BulkRnaseq, "GO"),
        ScopeCheck::InPrimary
    );
    // Single-cell RNA-seq secondary set contains SO.
    assert_eq!(
        m.check(&BioinformaticsModality::SingleCellRnaseq, "SO"),
        ScopeCheck::InSecondary
    );
    // Multi-omics uses the `union` token in primary — any prefix
    // resolves to InPrimary.
    assert_eq!(
        m.check(&BioinformaticsModality::MultiOmics, "ChEBI"),
        ScopeCheck::InPrimary
    );
    // Out-of-scope: a prefix not in any set.
    assert_eq!(
        m.check(&BioinformaticsModality::BulkRnaseq, "DOAP"),
        ScopeCheck::OutOfScope
    );
}

#[test]
fn missing_file_errors() {
    let r = OntologyScopeMatrix::load_from_path("does-not-exist.yaml");
    assert!(matches!(r, Err(OntologyScopeError::NotFound(_))));
}

#[test]
fn prefix_of_iri_handles_purl_and_compact() {
    assert_eq!(
        OntologyScopeMatrix::prefix_of_iri("http://purl.obolibrary.org/obo/GO_0008150").as_deref(),
        Some("GO")
    );
    assert_eq!(
        OntologyScopeMatrix::prefix_of_iri("GO:0008150").as_deref(),
        Some("GO")
    );
    assert_eq!(
        OntologyScopeMatrix::prefix_of_iri("EDAM:data_1383").as_deref(),
        Some("EDAM")
    );
    assert_eq!(
        OntologyScopeMatrix::prefix_of_iri("http://example.org/x").as_deref(),
        None
    );
    // Lowercase compact prefix (e.g. existing `data:0863` style) is
    // intentionally NOT recognised — those are EDAM compact CURIEs in
    // a different naming convention. The IRI -> ontology prefix mapping
    // only covers PURL + uppercase-CURIE forms.
    assert_eq!(
        OntologyScopeMatrix::prefix_of_iri("data:0863").as_deref(),
        None
    );
}
