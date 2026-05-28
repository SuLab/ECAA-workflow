//! Integration test: scheduler dispatches independent tasks during
//! session-pause (i.e., when a discover_* task is Blocked for SME review).
//!
//! DAG fixture:
//!   A (Blocked / discover_markers) — the SME-review gate
//!   B (Ready)  — depends on A  → must NOT be dispatched while pausing
//!   C (Ready)  — no dependencies → MUST be dispatched while pausing
//!   D (Ready)  — depends on C   → MUST be dispatched while pausing
//!                (C itself is not blocked, so D's upstream is clear)
//!
//! Two scenarios:
//!  1. session_pausing=true  → picks contain C and D; B absent.
//!  2. session_pausing=false → picks contain B, C, D (normal budget).

use ecaa_workflow_core::dag::{
    Assignee, BlockedRecord, ResourceClass, Task, TaskId, TaskKind, TaskState, DAG,
};
use ecaa_workflow_harness::scheduler::{
    pause_dependent_tasks, pick_ready_respecting_budgets, pick_ready_with_lanes, LaneBudget,
    SchedulerBudget,
};
use std::collections::{BTreeMap, HashSet};

fn make_task(id: &str, state: TaskState, depends_on: Vec<&str>) -> (TaskId, Task) {
    (
        TaskId::from(id),
        Task {
            kind: TaskKind::Computation,
            state,
            depends_on: depends_on.iter().map(|s| TaskId::from(*s)).collect(),
            assignee: Assignee::Agent,
            description: id.into(),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: None,
            safety: Default::default(),
        },
    )
}

fn build_dag(tasks: Vec<(TaskId, Task)>) -> DAG {
    let mut map = BTreeMap::new();
    for (id, t) in tasks {
        map.insert(id, t);
    }
    let mut dag = DAG {
        version: "1".into(),
        schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
        workflow_id: "test-pause".into(),
        current_task: None,
        tasks: map,
        reverse_deps: BTreeMap::new(),
        run_id: None,
    };
    dag.rebuild_reverse_deps();
    dag
}

fn blocked_state() -> TaskState {
    TaskState::Blocked {
        record: BlockedRecord {
            reason: "[requires_sme_review] discover_markers needs SME input".into(),
            attempts: vec![],
        },
    }
}

fn fixture_dag() -> DAG {
    build_dag(vec![
        make_task("discover_a", blocked_state(), vec![]),
        make_task("task_b", TaskState::Ready, vec!["discover_a"]),
        make_task("task_c", TaskState::Ready, vec![]),
        make_task("task_d", TaskState::Ready, vec!["task_c"]),
    ])
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[test]
fn pause_dependent_tasks_identifies_transitive_dependents() {
    let dag = fixture_dag();
    let blocked: HashSet<TaskId> = [TaskId::from("discover_a")].into_iter().collect();
    let deps = pause_dependent_tasks(&dag, &blocked);

    // B depends on A → in set.
    assert!(
        deps.contains(&TaskId::from("task_b")),
        "task_b must be pause-dependent"
    );
    // C and D have no path through A → not in set.
    assert!(
        !deps.contains(&TaskId::from("task_c")),
        "task_c must NOT be pause-dependent"
    );
    assert!(
        !deps.contains(&TaskId::from("task_d")),
        "task_d must NOT be pause-dependent"
    );
    // A itself is not included (only dependents are collected).
    assert!(
        !deps.contains(&TaskId::from("discover_a")),
        "discover_a is the blocker, not a dependent"
    );
}

#[test]
fn scheduler_session_pause_dispatches_independent_tasks() {
    let dag = fixture_dag();

    // Compute the pause exclusion set as main.rs does.
    let blocked_ids: HashSet<TaskId> = dag
        .tasks
        .iter()
        .filter(|(_, t)| matches!(t.state, TaskState::Blocked { .. }))
        .map(|(id, _)| id.clone())
        .collect();
    let pause_excluded = pause_dependent_tasks(&dag, &blocked_ids);

    // Budget big enough to pick all Ready tasks.
    let budget = SchedulerBudget {
        cpu_slots: 4,
        gpu_slots: 0,
    };
    let raw_picks = pick_ready_respecting_budgets(&dag, budget);

    // Filter as main.rs does when session_pausing == true.
    let pausing_picks: Vec<TaskId> = raw_picks
        .into_iter()
        .filter(|id| !pause_excluded.contains(id))
        .collect();

    // C and D must be dispatched.
    assert!(
        pausing_picks.contains(&TaskId::from("task_c")),
        "task_c (no blocked dep) must be dispatched during pause; got: {:?}",
        pausing_picks
    );
    assert!(
        pausing_picks.contains(&TaskId::from("task_d")),
        "task_d (depends only on Ready task_c) must be dispatched during pause; got: {:?}",
        pausing_picks
    );
    // B must NOT be dispatched (transitively depends on blocked A).
    assert!(
        !pausing_picks.contains(&TaskId::from("task_b")),
        "task_b (depends on blocked discover_a) must NOT be dispatched; got: {:?}",
        pausing_picks
    );
}

#[test]
fn scheduler_no_pause_dispatches_all_ready_tasks() {
    let dag = fixture_dag();
    let budget = SchedulerBudget {
        cpu_slots: 4,
        gpu_slots: 0,
    };
    // When not pausing, pause_excluded is empty — all Ready tasks are candidates.
    let pause_excluded: HashSet<TaskId> = HashSet::new();
    let raw_picks = pick_ready_respecting_budgets(&dag, budget);
    let picks: Vec<TaskId> = raw_picks
        .into_iter()
        .filter(|id| !pause_excluded.contains(id))
        .collect();

    // All three Ready tasks returned (B, C, D — not A which is Blocked).
    assert!(
        picks.contains(&TaskId::from("task_b")),
        "task_b must appear when not pausing"
    );
    assert!(
        picks.contains(&TaskId::from("task_c")),
        "task_c must appear when not pausing"
    );
    assert!(
        picks.contains(&TaskId::from("task_d")),
        "task_d must appear when not pausing"
    );
    assert_eq!(
        picks.len(),
        3,
        "exactly 3 Ready tasks expected; got: {:?}",
        picks
    );
}

#[test]
fn scheduler_session_pause_dispatches_independent_tasks_lane_mode() {
    // Same invariant verified with the two-lane picker.
    let dag = fixture_dag();

    let blocked_ids: HashSet<TaskId> = dag
        .tasks
        .iter()
        .filter(|(_, t)| matches!(t.state, TaskState::Blocked { .. }))
        .map(|(id, _)| id.clone())
        .collect();
    let pause_excluded = pause_dependent_tasks(&dag, &blocked_ids);

    let lanes = LaneBudget {
        processing_slots: 2,
        validation_slots: 2,
        gpu_slots: 0,
    };
    let raw_picks = pick_ready_with_lanes(&dag, lanes);
    let pausing_picks: Vec<TaskId> = raw_picks
        .into_iter()
        .filter(|id| !pause_excluded.contains(id))
        .collect();

    assert!(
        pausing_picks.contains(&TaskId::from("task_c")),
        "task_c must be dispatched (lane mode); got: {:?}",
        pausing_picks
    );
    assert!(
        !pausing_picks.contains(&TaskId::from("task_b")),
        "task_b must not be dispatched (lane mode); got: {:?}",
        pausing_picks
    );
}
