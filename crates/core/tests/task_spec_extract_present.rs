//! Token-reduction tactic #1 gate: every task in the emitted DAG must have
//! a `runtime/outputs/<task_id>/task-spec.json` sidecar with the expected
//! shape so the executor agent can read its focused slice without parsing
//! the full 40+ KB WORKFLOW.json on turn 1.

use ecaa_workflow_core::classify::ClassificationResult;
use ecaa_workflow_core::dag::{
    current_dag_schema_version, Assignee, ResourceClass, Task, TaskId, TaskKind, TaskState, DAG,
};
use ecaa_workflow_core::emitter::{emit_package, EmitConfig};
use std::collections::BTreeMap;
use tempfile::TempDir;

fn minimal_classification() -> ClassificationResult {
    ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "config/stage-taxonomies/rnaseq-de.yaml".into(),
        domain: "computational biology".into(),
        workflow_description: "task-spec integration smoke test".into(),
        edam_topic: "topic:3308".into(),
        edam_operation: "operation:3223".into(),
        confidence: 0.85,
        confidence_label: "high".into(),
        organisms: vec![],
        methods_specified: vec![],
        data_sources: vec![],
        intake_text: "task-spec-extract-present test fixture".into(),
        goal: None,
        archetype_id: None,
        additional_modalities: vec![],
        tie_candidates: vec![],
    }
}

fn two_task_dag() -> DAG {
    let mut tasks: BTreeMap<TaskId, Task> = BTreeMap::new();
    tasks.insert(
        "data_acquisition_gse000001".to_string().into(),
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Pending,
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "Fetch raw count matrix from GEO".into(),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: Some("data_acquisition_geo".into()),
            safety: Default::default(),
        },
    );
    tasks.insert(
        "preprocessing_qc".to_string().into(),
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Pending,
            depends_on: vec!["data_acquisition_gse000001".into()],
            assignee: Assignee::Agent,
            description: "Quality control and filtering".into(),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: Some("preprocessing_qc".into()),
            safety: Default::default(),
        },
    );
    let mut dag = DAG {
        version: "1.0".into(),
        schema_version: current_dag_schema_version(),
        workflow_id: "test-task-spec-extract".into(),
        current_task: None,
        tasks,
        reverse_deps: BTreeMap::new(),
        run_id: None,
    };
    dag.rebuild_reverse_deps();
    dag
}

/// Every task in the emitted DAG must have a `task-spec.json` sidecar
/// inside its `runtime/outputs/<task_id>/` directory.
#[test]
fn task_spec_sidecar_present_for_every_task() {
    let tmp = TempDir::new().unwrap();
    let dag = two_task_dag();
    let clf = minimal_classification();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");

    emit_package(&EmitConfig {
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir,
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
    })
    .expect("emit_package must succeed");

    for task_id in dag.tasks.keys() {
        let spec_path = tmp
            .path()
            .join("runtime/outputs")
            .join(task_id.as_str())
            .join("task-spec.json");
        assert!(
            spec_path.exists(),
            "task-spec.json missing for task {task_id}: expected at {spec_path:?}"
        );

        // Parse and validate mandatory fields.
        let raw = std::fs::read_to_string(&spec_path)
            .unwrap_or_else(|e| panic!("could not read task-spec.json for {task_id}: {e}"));
        let spec: serde_json::Value = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("task-spec.json for {task_id} is not valid JSON: {e}"));

        assert_eq!(
            spec["task_id"].as_str(),
            Some(task_id.as_str()),
            "task_id field mismatch in task-spec.json for {task_id}"
        );
        assert!(
            spec.get("kind").is_some(),
            "task-spec.json for {task_id} missing 'kind' field"
        );
        assert!(
            spec.get("depends_on").is_some(),
            "task-spec.json for {task_id} missing 'depends_on' field"
        );
        assert!(
            spec.get("description").is_some(),
            "task-spec.json for {task_id} missing 'description' field"
        );
    }
}

/// The `task_id` field in the sidecar must match the task's map key,
/// not be null or empty.
#[test]
fn task_spec_task_id_matches_map_key() {
    let tmp = TempDir::new().unwrap();
    let dag = two_task_dag();
    let clf = minimal_classification();
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");

    emit_package(&EmitConfig {
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir,
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
    })
    .expect("emit_package must succeed");

    for task_id in dag.tasks.keys() {
        let raw = std::fs::read_to_string(
            tmp.path()
                .join("runtime/outputs")
                .join(task_id.as_str())
                .join("task-spec.json"),
        )
        .unwrap();
        let spec: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let embedded_id = spec["task_id"]
            .as_str()
            .expect("task_id must be a string in task-spec.json");
        assert_eq!(
            embedded_id,
            task_id.as_str(),
            "embedded task_id '{embedded_id}' != map key '{task_id}'"
        );
    }
}
