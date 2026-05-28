//! Integration coverage for `DagError::OrphanedTask`.
//!
//! Constructs a DAG with one isolated task (no incoming, no outgoing edges)
//! alongside a second connected pair, and asserts the typed validator surfaces
//! the orphan via the shaped variant — not via the catch-all `Other(_)`. Plan
//! §S5.12 typed-error coverage; pairs with the inline `validate_dag_typed_*`
//! tests in `dag.rs` that exercise the other variants.

use ecaa_workflow_core::dag::{
    current_dag_schema_version, validate_dag_typed, Assignee, DagError, ResourceClass, Task,
    TaskId, TaskKind, TaskState, DAG,
};
use std::collections::BTreeMap;

fn pending_task(deps: Vec<&str>) -> Task {
    Task {
        kind: TaskKind::Computation,
        state: TaskState::Pending,
        depends_on: deps.into_iter().map(TaskId::from).collect(),
        assignee: Assignee::Agent,
        description: "test".into(),
        spec: None,
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,
        required_artifacts: vec![],
        container: None,
        source_atom_id: None,
        safety: Default::default(),
    }
}

fn make_dag(tasks: Vec<(&str, Task)>) -> DAG {
    let mut dag = DAG {
        version: "1.0".into(),
        schema_version: current_dag_schema_version(),
        workflow_id: "test_orphan".into(),
        current_task: None,
        tasks: tasks
            .into_iter()
            .map(|(k, v)| (TaskId::from(k), v))
            .collect(),
        reverse_deps: BTreeMap::new(),
        run_id: None,
    };
    dag.rebuild_reverse_deps();
    dag
}

#[test]
fn validate_dag_typed_returns_orphaned_task_for_isolated_node() {
    let dag = make_dag(vec![
        ("root", pending_task(vec![])),
        ("child", pending_task(vec!["root"])),
        // `island` has no incoming + no outgoing edges → orphan when ≥2 tasks.
        ("island", pending_task(vec![])),
    ]);
    match validate_dag_typed(&dag) {
        Err(DagError::OrphanedTask { task_id }) => {
            assert_eq!(
                task_id.as_str(),
                "island",
                "expected the isolated `island` task to be flagged"
            );
        }
        other => panic!("expected OrphanedTask, got {:?}", other),
    }
}
