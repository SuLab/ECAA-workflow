//! C2 (M10): a task left Running on disk with NO DispatchRecord (crash
//! between write_dag and append_dispatch) must be re-blocked by the
//! startup sweep, not silently left wedged Running forever.
use ecaa_workflow_core::dag::{
    Assignee, BlockedRecord, ResourceClass, Task, TaskId, TaskKind, TaskState, DAG,
};
use ecaa_workflow_harness::dispatch_wal::{
    dispatch_wal_schema_version, sweep_running_without_wal_record, DispatchRecord,
};
use std::collections::BTreeMap;

fn running_task() -> Task {
    Task {
        kind: TaskKind::Computation,
        state: TaskState::Running {
            started_at: "2026-06-10T00:00:00Z".into(),
            remote: None,
        },
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: String::new(),
        spec: None,
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,
        required_artifacts: vec![],
        container: None,
        source_atom_id: None,
        safety: Default::default(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        edam_operation: None,
        execution_index: None,
    }
}

fn dag_with(tasks: Vec<(&str, Task)>) -> DAG {
    let mut map = BTreeMap::new();
    for (id, t) in tasks {
        map.insert(TaskId::from(id), t);
    }
    let mut dag = DAG {
        version: "1".into(),
        schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
        workflow_id: "c2-test".into(),
        current_task: None,
        tasks: map,
        reverse_deps: BTreeMap::new(),
        run_id: None,
        execution_order: Vec::new(),
    };
    dag.rebuild_reverse_deps();
    dag
}

#[test]
fn running_task_without_any_wal_record_is_reblocked() {
    let mut dag = dag_with(vec![("alignment", running_task())]);
    let records: Vec<DispatchRecord> = vec![]; // crash before append_dispatch

    let reblocked = sweep_running_without_wal_record(&mut dag, &records);

    assert_eq!(reblocked, vec!["alignment".to_string()]);
    assert!(
        matches!(
            dag.tasks[&TaskId::from("alignment")].state,
            TaskState::Blocked { .. }
        ),
        "task with no WAL record must be Blocked"
    );
    // The marker must drive the server's blocker mapper to OrphanedByCrash.
    if let TaskState::Blocked { record } = &dag.tasks[&TaskId::from("alignment")].state {
        assert!(
            record.reason.starts_with("[orphaned_by_crash]"),
            "reblock reason must carry the orphaned_by_crash marker; got {}",
            record.reason
        );
    }
}

#[test]
fn running_task_with_a_wal_record_is_left_alone() {
    let mut dag = dag_with(vec![("alignment", running_task())]);
    let records = vec![DispatchRecord {
        schema_version: dispatch_wal_schema_version(),
        task_id: "alignment".into(),
        epoch: 1,
        harness_run_id: "prev".into(),
        started_at: "2026-06-10T00:00:00Z".into(),
        timeout_at: "2026-06-10T00:05:00Z".into(),
    }];

    let reblocked = sweep_running_without_wal_record(&mut dag, &records);

    assert!(
        reblocked.is_empty(),
        "a task WITH a WAL record is recovery's job, not the sweep's"
    );
    assert!(matches!(
        dag.tasks[&TaskId::from("alignment")].state,
        TaskState::Running { .. }
    ));
}
