//! Tier F property test for F1 — every emitted workflow DAG is
//! acyclic. Exercises the production validator `validate_dag_typed`
//! (petgraph toposort + Kosaraju SCC) over generated linear-chain
//! topologies and their cyclic perturbations, plus pinned degenerate
//! cases (self-loop, two-node cycle).
//!
//! Replaces the prior `prop_assert!(true)` placeholder: the real
//! property asserts that the acyclic shape validates clean and that a
//! single back-edge — at any position in a chain of any length — is
//! always surfaced as `DagError::CycleDetected`.

use ecaa_workflow_core::dag::{
    current_dag_schema_version, validate_dag_typed, Assignee, DagError, ResourceClass, Task,
    TaskId, TaskKind, TaskState, DAG,
};
use proptest::prelude::*;
use std::collections::BTreeMap;

fn computation_task(deps: Vec<String>) -> Task {
    Task {
        kind: TaskKind::Computation,
        state: TaskState::Pending,
        depends_on: deps.into_iter().map(|s| TaskId::from(s.as_str())).collect(),
        assignee: Assignee::Agent,
        description: "f01".into(),
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

fn make_dag(tasks: Vec<(String, Task)>) -> DAG {
    let mut dag = DAG {
        version: "1.0".into(),
        schema_version: current_dag_schema_version(),
        workflow_id: "f01".into(),
        current_task: None,
        tasks: tasks
            .into_iter()
            .map(|(k, v)| (TaskId::from(k.as_str()), v))
            .collect(),
        reverse_deps: BTreeMap::new(),
        run_id: None,
    };
    dag.rebuild_reverse_deps();
    dag
}

/// Connected linear chain: t0 produced first, each subsequent task
/// depends on its predecessor. Always acyclic and orphan-free.
fn linear_chain(n: usize) -> Vec<(String, Task)> {
    (0..n)
        .map(|i| {
            let deps = if i == 0 {
                vec![]
            } else {
                vec![format!("t{}", i - 1)]
            };
            (format!("t{}", i), computation_task(deps))
        })
        .collect()
}

#[test]
fn self_loop_is_rejected_as_cycle() {
    let dag = make_dag(vec![("a".into(), computation_task(vec!["a".into()]))]);
    assert!(
        matches!(
            validate_dag_typed(&dag),
            Err(DagError::CycleDetected { .. })
        ),
        "self-loop a->a must be CycleDetected, got {:?}",
        validate_dag_typed(&dag)
    );
}

#[test]
fn two_node_cycle_is_rejected() {
    let dag = make_dag(vec![
        ("a".into(), computation_task(vec!["b".into()])),
        ("b".into(), computation_task(vec!["a".into()])),
    ]);
    assert!(
        matches!(
            validate_dag_typed(&dag),
            Err(DagError::CycleDetected { .. })
        ),
        "a<->b cycle must be CycleDetected, got {:?}",
        validate_dag_typed(&dag)
    );
}

proptest! {
    /// Any linear chain (the canonical acyclic shape) validates clean.
    #[test]
    fn linear_chains_are_acyclic(n in 1usize..24) {
        let dag = make_dag(linear_chain(n));
        let result = validate_dag_typed(&dag);
        prop_assert!(
            result.is_ok(),
            "linear chain of {} tasks should be acyclic, got {:?}",
            n, result
        );
    }

    /// A linear chain plus a single back-edge from the tail to an
    /// earlier node closes a loop the validator must catch. `target <
    /// n-1` gives an interior cycle; `target == n-1` degenerates to a
    /// tail self-loop — both are cycles.
    #[test]
    fn chain_with_back_edge_is_rejected(n in 2usize..24, back in 0usize..256) {
        let target = back % n;
        let mut tasks = linear_chain(n);
        tasks[target]
            .1
            .depends_on
            .push(TaskId::from(format!("t{}", n - 1).as_str()));
        let dag = make_dag(tasks);
        let result = validate_dag_typed(&dag);
        prop_assert!(
            matches!(result, Err(DagError::CycleDetected { .. })),
            "chain of {} with back-edge t{}->t{} must be CycleDetected, got {:?}",
            n, target, n - 1, result
        );
    }
}
