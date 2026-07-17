//! DE raw|normalized counts one-of: a satisfied one-of `InputGroup`
//! must not mark `required_contract_unsatisfied` as `Reject`, even
//! though the unbound sibling member still carries a weak
//! (`Unproven`) placeholder edge.
//!
//! Exercises `composer_v4::rescore_dag` — the public wrapper around
//! the v4 planner's private `score_dag` — directly against a
//! hand-built `WorkflowDag`. The fixture stands in for the shape a
//! composed DAG would carry once `differential_expression` declares a
//! `counts` one-of group over `raw_counts` / `normalized_counts`
//! (planned separately); this test exercises the scoring exemption in
//! isolation from the search pipeline that would eventually produce
//! such a DAG.

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom::InputGroup;
use ecaa_workflow_core::composer_v4::{rescore_dag, PlanningContext, ScoringTuple, ScoringValue};
use ecaa_workflow_core::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
use ecaa_workflow_core::workflow_contracts::evidence::AssumptionLedger;
use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};

const CONSUMER_ID: &str = "differential_expression";
const PRODUCER_ID: &str = "data_acquisition";

/// A `counts` one-of group over `raw_counts` / `normalized_counts`,
/// satisfied by a single bound member (mirrors the planned
/// `differential_expression` atom's method-neutral substrate choice:
/// count-GLM tools want raw, rank-based tools want normalized).
///
/// `InputGroup` is `#[non_exhaustive]`, so an external test crate
/// can't use struct-literal syntax; built via JSON deserialization
/// instead (round-trips through the same `Deserialize` impl the atom
/// YAML loader uses).
fn counts_one_of_group() -> InputGroup {
    serde_json::from_value(serde_json::json!({
        "name": "counts",
        "kind": "one_of",
        "members": ["raw_counts", "normalized_counts"],
        "min_bound": 1,
    }))
    .unwrap()
}

/// DE-like consumer node declaring the one-of group via the
/// `input_groups` attribute (`TaskNode::from_atom`'s preservation
/// convention — see `workflow_contracts::from_atom`).
fn de_consumer_node() -> TaskNode {
    let mut node = TaskNode::skeleton(CONSUMER_ID, "differential expression by condition");
    node.attributes.insert(
        "input_groups".into(),
        serde_json::to_value(vec![counts_one_of_group()]).unwrap(),
    );
    node
}

fn edge(to_port: &str, kind: EdgeKind) -> EdgeContract {
    EdgeContract {
        from_node: PRODUCER_ID.into(),
        from_port: "".into(),
        to_node: CONSUMER_ID.into(),
        to_port: to_port.into(),
        proof: CompatibilityProof::default(),
        kind,
        chain_of_custody: None,
        mutually_exclusive_group: None,
    }
}

fn score(dag: &WorkflowDag) -> ScoringTuple {
    rescore_dag(
        dag,
        &PlanningContext::default(),
        &ArchetypeRegistry::default(),
    )
}

/// DE binds `raw_counts` (proven `TypedDataFlow`); `normalized_counts`
/// carries an `Unproven` edge — the weak-match placeholder shape an
/// unbound one-of sibling leaves behind. The group's `min_bound` (1)
/// is already satisfied by `raw_counts` alone.
fn dag_with_bound_members(bound: &[&str]) -> WorkflowDag {
    let mut edges = vec![edge("normalized_counts", EdgeKind::Unproven)];
    if bound.contains(&"raw_counts") {
        edges.push(edge("raw_counts", EdgeKind::TypedDataFlow));
    }
    WorkflowDag {
        id: "test".into(),
        nodes: vec![
            TaskNode::skeleton(PRODUCER_ID, "producer"),
            de_consumer_node(),
        ],
        edges,
        assumptions: AssumptionLedger::default(),
        source_template: None,
    }
}

#[test]
fn satisfied_one_of_does_not_mark_required_contract_unsatisfied() {
    let dag = dag_with_bound_members(&["raw_counts"]);
    let score = score(&dag);
    assert_ne!(
        score.required_contract_unsatisfied,
        ScoringValue::Reject,
        "a satisfied one-of group must not Reject the candidate"
    );
}

/// Regression guard: with ZERO members bound, the one-of group is not
/// satisfied — the `Unproven` edge into `normalized_counts` must still
/// Reject exactly as it would have before the exemption existed.
#[test]
fn unsatisfied_one_of_group_still_rejects() {
    let dag = dag_with_bound_members(&[]);
    let score = score(&dag);
    assert_eq!(
        score.required_contract_unsatisfied,
        ScoringValue::Reject,
        "zero bound members means the group is unsatisfied; must still Reject"
    );
}

/// An `Unproven` edge into a port that belongs to NO declared group
/// must never be exempted, satisfied sibling elsewhere or not.
#[test]
fn unproven_edge_outside_any_group_still_rejects() {
    let mut dag = dag_with_bound_members(&["raw_counts"]);
    dag.edges
        .push(edge("experimental_design", EdgeKind::Unproven));
    let score = score(&dag);
    assert_eq!(
        score.required_contract_unsatisfied,
        ScoringValue::Reject,
        "an Unproven edge on a non-grouped port must still Reject"
    );
}
