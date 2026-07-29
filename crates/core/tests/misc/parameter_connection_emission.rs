//! Verifies that the PRE-EXECUTION PLAN crate's `ro-crate-metadata.json`
//! descriptor declares EXECUTION-AWARE `conformsTo` (T6′): only the profiles a
//! workflow *definition* truthfully satisfies (`PLAN_PROFILE_IRIS` — base
//! RO-Crate 1.1, workflow-ro-crate/1.0, ecaa/v0.2), and NOT the three WRROC
//! v0.5 run profiles (process / workflow / provenance), which document
//! *executed* runs and are added only on finalize. The
//! ParameterConnection-per-edge and one-p-plan:Plan-per-package tests live
//! elsewhere; this file covers the conformance-IRI check only.

use ecaa_workflow_core::classify::ClassificationResult;
use ecaa_workflow_core::dag::DAG;
use ecaa_workflow_core::ro_crate::build_metadata;
use std::collections::BTreeMap;

fn fixture_dag() -> DAG {
    DAG {
        version: "1.0.0".into(),
        schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
        workflow_id: "test_wf".into(),
        current_task: None,
        tasks: BTreeMap::new(),
        reverse_deps: BTreeMap::new(),
        run_id: None,
        execution_order: Vec::new(),
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

/// The plan-crate descriptor declares base RO-Crate 1.1 + workflow-ro-crate/1.0
/// + ecaa/v0.2, and does NOT (yet) declare any of the three WRROC v0.5 run
/// profiles — those document executed runs and are added only on finalize.
#[test]
fn conforms_to_is_plan_set_not_wrroc_run_profiles() {
    let dag = fixture_dag();
    let metadata = build_metadata(
        &dag,
        &fixture_classification(),
        &ecaa_workflow_core::clock::FrozenClock::default(),
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

    // The truthful plan-set profiles are present.
    assert!(
        ids.contains(&"https://w3id.org/ro/crate/1.1"),
        "must assert base RO-Crate 1.1; got {ids:?}"
    );
    assert!(
        ids.contains(&"https://w3id.org/workflowhub/workflow-ro-crate/1.0"),
        "must assert the WorkflowHub workflow-ro-crate/1.0 profile; got {ids:?}"
    );
    assert!(
        ids.contains(&"https://w3id.org/ecaa/v0.2"),
        "must assert the ECAA v0.2 profile; got {ids:?}"
    );

    // The three WRROC run profiles are EXECUTION-ONLY and absent from a plan
    // crate (regression guard against the rejected Task-6 synthetic-action hack).
    for run_iri in ecaa_workflow_types::consts::EXECUTED_ADDED_PROFILE_IRIS {
        assert!(
            !ids.contains(run_iri),
            "plan crate must NOT claim execution-only run profile {run_iri}; got {ids:?}"
        );
    }
}

/// The plan-crate descriptor declares EXACTLY the plan profile set — no more,
/// no less (the executed crate's full 6-IRI set is asserted by the
/// finalize-path tests in `ro_crate.rs`).
#[test]
fn conforms_to_declares_exactly_the_plan_profile_set() {
    let dag = fixture_dag();
    let metadata = build_metadata(
        &dag,
        &fixture_classification(),
        &ecaa_workflow_core::clock::FrozenClock::default(),
    );
    let graph = metadata["@graph"]
        .as_array()
        .expect("@graph must be an array");

    let descriptor = graph
        .iter()
        .find(|e| e["@id"] == "ro-crate-metadata.json")
        .expect("ro-crate-metadata.json descriptor must exist");

    let ids: std::collections::BTreeSet<&str> = descriptor["conformsTo"]
        .as_array()
        .expect("conformsTo must be an array")
        .iter()
        .map(|c| c["@id"].as_str().expect("each conformsTo entry needs @id"))
        .collect();

    let expected: std::collections::BTreeSet<&str> = ecaa_workflow_types::consts::PLAN_PROFILE_IRIS
        .iter()
        .copied()
        .collect();
    assert_eq!(
        ids, expected,
        "plan descriptor must declare exactly the plan profile set; got {ids:?}"
    );
}
