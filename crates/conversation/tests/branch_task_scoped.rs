//! Task-scoped branch tests (M1.3).
//!
//! Covers three axes:
//! 1. `Session::branch_from_at_task` resets the named task to Ready and all
//!    transitive successors to Pending, leaving predecessors Completed.
//! 2. A branch without `task_id` keeps the full DAG state (session-scoped
//!    branch, M1.1 behaviour unchanged).
//! 3. `branch_session_with_task` returns a typed error for an unknown task_id.

#[path = "common/mod.rs"]
mod common;

use scripps_workflow_conversation::session::lineage::SessionLineage;
use scripps_workflow_conversation::session::Session;
use scripps_workflow_conversation::tools::branch::branch_session_with_task;
use scripps_workflow_core::dag::{TaskState, DAG};

// ── helpers ──────────────────────────────────────────────────────────────────

/// Build a simple linear 3-task DAG: a → b → c, all Completed initially.
fn make_linear_dag() -> DAG {
    let v = serde_json::json!({
        "version": "1",
        "workflow_id": "wf-test",
        "tasks": {
            "a": {
                "kind": "computation",
                "state": { "status": "completed", "result": {"ok": true} },
                "depends_on": [],
                "assignee": "agent",
                "description": "task a"
            },
            "b": {
                "kind": "computation",
                "state": { "status": "completed", "result": {"ok": true} },
                "depends_on": ["a"],
                "assignee": "agent",
                "description": "task b"
            },
            "c": {
                "kind": "computation",
                "state": { "status": "completed", "result": {"ok": true} },
                "depends_on": ["b"],
                "assignee": "agent",
                "description": "task c"
            }
        }
    });
    let mut dag: DAG = serde_json::from_value(v).expect("dag deserialization");
    dag.rebuild_reverse_deps();
    dag
}

/// Build a session with the 3-task linear DAG.
fn session_with_dag() -> Session {
    let mut s = Session::new(false);
    s.state = scripps_workflow_conversation::session::SessionState::Emitted;
    s.dag = Some(make_linear_dag());
    s
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Branch at task "b": b must become Ready, c must become Pending, a
/// must remain Completed. The child lineage must record branched_from_task_id.
#[test]
fn branch_at_task_resets_named_task_and_descendants() {
    let parent = session_with_dag();
    let child = Session::branch_from_at_task(&parent, false, Some("b".to_string()));
    let child_dag = child.dag.as_ref().expect("child must have a dag");

    assert!(
        matches!(child_dag.tasks["a"].state, TaskState::Completed { .. }),
        "predecessor 'a' must remain Completed; got {:?}",
        child_dag.tasks["a"].state
    );
    assert!(
        matches!(child_dag.tasks["b"].state, TaskState::Ready),
        "branch-target 'b' must become Ready; got {:?}",
        child_dag.tasks["b"].state
    );
    assert!(
        matches!(child_dag.tasks["c"].state, TaskState::Pending),
        "descendant 'c' must become Pending; got {:?}",
        child_dag.tasks["c"].state
    );

    // Lineage must record the task boundary.
    let lineage = child.lineage.as_ref().expect("child must have lineage");
    assert_eq!(
        lineage.branched_from_task_id.as_deref(),
        Some("b"),
        "lineage must record branched_from_task_id = 'b'"
    );
}

/// Branch without task_id keeps full DAG state (M1.1 regression).
#[test]
fn branch_without_task_id_preserves_full_dag() {
    let parent = session_with_dag();
    let child = Session::branch_from_at_task(&parent, false, None);
    let child_dag = child.dag.as_ref().expect("child must have dag");

    // All tasks still Completed — no reset.
    for (id, task) in &child_dag.tasks {
        assert!(
            matches!(task.state, TaskState::Completed { .. }),
            "task {id} should remain Completed without task_id; got {:?}",
            task.state
        );
    }
    let lineage = child.lineage.as_ref().expect("child must have lineage");
    assert!(
        lineage.branched_from_task_id.is_none(),
        "lineage.branched_from_task_id must be None without task_id"
    );
}

/// `branch_session_with_task` returns a typed error for an unknown task_id.
#[test]
fn branch_with_bogus_task_id_returns_tool_error() {
    let parent = session_with_dag();
    let result = branch_session_with_task(&parent, Some("does_not_exist"), None);
    assert!(
        result.is_error,
        "branch with bogus task_id must return an error; got {result:?}"
    );
    let body = serde_json::to_string(&result.content).unwrap();
    assert!(
        body.contains("does_not_exist") || body.contains("unknown") || body.contains("not found"),
        "error must reference the unknown task_id; got {body}"
    );
}

/// When no dag is present on the parent, branch at task returns an error.
#[test]
fn branch_at_task_without_dag_returns_error() {
    let mut parent = Session::new(false);
    parent.state = scripps_workflow_conversation::session::SessionState::Emitted;
    // No dag set.
    let result = branch_session_with_task(&parent, Some("b"), None);
    assert!(
        result.is_error,
        "branch with task_id but no parent dag must return error"
    );
}

/// Branch at root task (task "a"): all tasks reset to Ready/Pending.
#[test]
fn branch_at_root_task_resets_all() {
    let parent = session_with_dag();
    let child = Session::branch_from_at_task(&parent, false, Some("a".to_string()));
    let child_dag = child.dag.as_ref().unwrap();

    assert!(
        matches!(child_dag.tasks["a"].state, TaskState::Ready),
        "'a' must be Ready when it is the branch target"
    );
    assert!(
        matches!(child_dag.tasks["b"].state, TaskState::Pending),
        "descendant 'b' must be Pending"
    );
    assert!(
        matches!(child_dag.tasks["c"].state, TaskState::Pending),
        "descendant 'c' must be Pending"
    );
}

/// SessionLineage serializes branched_from_task_id correctly.
#[test]
fn session_lineage_serializes_branched_from_task_id() {
    let parent = session_with_dag();
    let child = Session::branch_from_at_task(&parent, false, Some("b".to_string()));
    let lineage = child.lineage.unwrap();

    let json = serde_json::to_value(&lineage).unwrap();
    assert_eq!(
        json["branched_from_task_id"].as_str(),
        Some("b"),
        "branched_from_task_id must round-trip through JSON"
    );
}

/// Lineage without branched_from_task_id deserializes with None (backward-compat).
#[test]
fn legacy_lineage_without_task_id_deserializes_as_none() {
    let raw = serde_json::json!({
        "schema_version": "0.1.0",
        "parent_session_id": "00000000-0000-0000-0000-000000000001",
        "branched_at": "2026-05-22T00:00:00Z",
    });
    let lineage: SessionLineage = serde_json::from_value(raw).unwrap();
    assert!(
        lineage.branched_from_task_id.is_none(),
        "missing branched_from_task_id must default to None"
    );
}
