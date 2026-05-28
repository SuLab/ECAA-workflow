//! Verifies that the `ro-crate-metadata.json` descriptor's
//! `conformsTo` asserts the WRROC v0.5 profile IRIs (process /
//! workflow / provenance) alongside the existing RO-Crate 1.1 IRI.
//! The ParameterConnection-per-edge and one-p-plan:Plan-per-package
//! tests live elsewhere; this file covers the conformance-IRI check
//! only.

use scripps_workflow_core::classify::ClassificationResult;
use scripps_workflow_core::dag::DAG;
use scripps_workflow_core::ro_crate::build_metadata;
use std::collections::BTreeMap;

fn fixture_dag() -> DAG {
    DAG {
        version: "1.0.0".into(),
        schema_version: scripps_workflow_core::dag::current_dag_schema_version(),
        workflow_id: "test_wf".into(),
        current_task: None,
        tasks: BTreeMap::new(),
        reverse_deps: BTreeMap::new(),
        run_id: None,
    }
}

fn fixture_classification() -> ClassificationResult {
    ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "".into(),
        domain: "transcriptomics".into(),
        workflow_description: "Test workflow".into(),
        confidence: 1.0,
        confidence_label: "high".into(),
        edam_topic: "topic_3170".into(),
        edam_operation: "operation_3223".into(),
        organisms: vec![],
        methods_specified: vec![],
        data_sources: vec![],
        intake_text: "test".into(),
        goal: None,
        archetype_id: Some("bulk_rnaseq_de".into()),
        additional_modalities: vec![],
        tie_candidates: vec![],
    }
}

#[test]
fn conforms_to_asserts_wrroc_v05() {
    let dag = fixture_dag();
    let metadata = build_metadata(
        &dag,
        &fixture_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    let graph = metadata["@graph"]
        .as_array()
        .expect("@graph must be an array");

    let descriptor = graph
        .iter()
        .find(|e| e["@id"] == "ro-crate-metadata.json")
        .expect("ro-crate-metadata.json descriptor must exist");

    let conforms = descriptor["conformsTo"]
        .as_array()
        .expect("conformsTo must be an array");

    let ids: Vec<&str> = conforms
        .iter()
        .map(|c| c["@id"].as_str().expect("each conformsTo entry needs @id"))
        .collect();

    assert!(
        ids.contains(&"https://w3id.org/ro/crate/1.1"),
        "must still assert RO-Crate 1.1; got {ids:?}"
    );
    assert!(
        ids.contains(&"https://w3id.org/ro/wfrun/process/0.5"),
        "must assert WRROC Process Run Crate 0.5; got {ids:?}"
    );
    assert!(
        ids.contains(&"https://w3id.org/ro/wfrun/workflow/0.5"),
        "must assert WRROC Workflow Run Crate 0.5; got {ids:?}"
    );
    assert!(
        ids.contains(&"https://w3id.org/ro/wfrun/provenance/0.5"),
        "must assert WRROC Provenance Run Crate 0.5; got {ids:?}"
    );
}
