use ecaa_workflow_core::dag::TaskState;
use ecaa_workflow_core::package_import::reconstruct_workflow_dag_from_package;
use std::fs;

/// Write a minimal but valid WORKFLOW.json (lowered DAG) with 3 tasks and one
/// dependency edge, plus empty proofs/assumptions sidecars.
///
/// NOTE: `TaskKind`, `DiscoveryKind`, and `Assignee` all serialize with
/// `#[serde(rename_all = "snake_case")]` (verified in `crates/core/src/dag.rs`),
/// so the on-disk shapes are `{"discovery":"source"}` / `"computation"` /
/// `"validation"` / `"agent"` — NOT the PascalCase forms.
fn write_pkg(root: &std::path::Path) {
    fs::create_dir_all(root.join("runtime")).unwrap();
    let dag = serde_json::json!({
        "version": "1",
        "workflow_id": "wf_test",
        "current_task": null,
        "tasks": {
            "data_acq": {
                "kind": {"discovery": "source"}, "state": {"status": "completed", "result": {}},
                "depends_on": [], "assignee": "agent", "description": "acquire"
            },
            "align": {
                "kind": "computation", "state": {"status": "completed", "result": {}},
                "depends_on": ["data_acq"], "assignee": "agent", "description": "align"
            },
            "report": {
                "kind": "validation", "state": {"status": "pending"},
                "depends_on": ["align"], "assignee": "agent", "description": "report"
            }
        },
        "execution_order": ["data_acq", "align", "report"]
    });
    fs::write(
        root.join("WORKFLOW.json"),
        serde_json::to_vec_pretty(&dag).unwrap(),
    )
    .unwrap();
    fs::write(root.join("runtime/proofs.jsonl"), b"").unwrap();
    fs::write(root.join("runtime/assumptions.jsonl"), b"").unwrap();
}

#[test]
fn reconstructs_task_set_and_states() {
    let dir = tempfile::tempdir().unwrap();
    write_pkg(dir.path());
    let (wf, states) = reconstruct_workflow_dag_from_package(dir.path()).unwrap();

    // Task set parity.
    let mut ids: Vec<_> = wf.nodes.iter().map(|n| n.id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec!["align", "data_acq", "report"]);

    // Task-state parity from WORKFLOW.json.
    assert!(matches!(
        states.get("data_acq"),
        Some(TaskState::Completed { .. })
    ));
    assert!(matches!(states.get("report"), Some(TaskState::Pending)));

    // Dependency edges present (align←data_acq, report←align). Edge direction/kind
    // may be Unproven when proofs.jsonl is empty; assert connectivity by count.
    assert!(
        wf.edges.len() >= 2,
        "expected ≥2 dependency edges, got {}",
        wf.edges.len()
    );
}

#[test]
fn missing_workflow_json_errors() {
    let dir = tempfile::tempdir().unwrap();
    let err = reconstruct_workflow_dag_from_package(dir.path()).unwrap_err();
    assert!(err.to_string().to_lowercase().contains("workflow.json"));
}
