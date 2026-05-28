//! Meet-in-the-middle proof matching for v4.
//!
//! For every backward-required atom + each of its input ports, find
//! the best forward-reachable producer whose output port unifies via
//! `CompatibilityEngine::prove`. Lossless adapters are auto-inserted;
//! risky adapters become assumptions on the edge. Returns one of three
//! `MeetResult` variants — fully `Connected`, `PartiallyConnected`, or
//! `Disconnected` — and a `WorkflowDag` carrying proof-bearing edges.

use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer_v4::{
    backward_search::backward_search,
    forward_search::forward_search,
    meet_in_middle::{meet_in_the_middle, MeetResult},
};
use ecaa_workflow_core::workflow_contracts::data_product::DataProductContract;
use ecaa_workflow_core::workflow_contracts::workflow_intent::{DesiredOutput, WorkflowIntent};

/// Meet-in-the-middle must produce a `WorkflowDag` with proof-carrying
/// edges. We don't pin specific atom names — atom-registry coverage and
/// known facet-unification quirks (privacy class, statistical state)
/// can vary the exact set the planner picks. The structural shape is
/// what's load-bearing: ≥1 nodes, ≥1 edges, every edge carries a
/// non-empty proof.
#[test]
fn meet_connects_paired_fastq_to_de_table() {
    let atom_reg =
        AtomRegistry::load_from_dir(std::path::Path::new("../../config/stage-atoms")).unwrap();

    // Source the DE-table goal IRI from the canonical fixture so the
    // test stays in sync with `sample_de_table`'s curated EDAM type.
    let de_goal = DataProductContract::sample_de_table();
    let intent = WorkflowIntent {
        available_data: vec![DataProductContract::sample_paired_fastq()],
        desired_outputs: vec![DesiredOutput {
            label: "differential expression table".into(),
            edam_data: Some(de_goal.semantic_type.stable_id()),
            edam_format: Some("format:3475".into()),
            human_readable: false,
        }],
        ..Default::default()
    };

    let forward = forward_search(&intent, &atom_reg, 8, 64);
    let backward = backward_search(&intent, &atom_reg, 8, 64);
    assert!(
        !forward.is_empty(),
        "forward_search returned empty frontier — meet cannot connect"
    );
    assert!(
        !backward.is_empty(),
        "backward_search returned empty requirements — meet cannot connect"
    );

    let result = meet_in_the_middle(&forward, &backward, &atom_reg);
    match result {
        MeetResult::Connected { dag, .. } | MeetResult::PartiallyConnected { dag, .. } => {
            assert!(!dag.nodes.is_empty(), "expected at least 1 node, got 0");
            assert!(!dag.edges.is_empty(), "expected at least 1 edge, got 0");

            // Every edge must carry a non-empty proof — at minimum, the
            // producer and consumer type stable ids must be populated by
            // the engine, even when facet unification produced warnings.
            for edge in &dag.edges {
                assert!(
                    !edge.proof.producer_type.is_empty(),
                    "edge {} -> {} missing producer_type in proof",
                    edge.from_node,
                    edge.to_node
                );
                assert!(
                    !edge.proof.consumer_type.is_empty(),
                    "edge {} -> {} missing consumer_type in proof",
                    edge.from_node,
                    edge.to_node
                );
            }

            // Determinism: two runs with the same inputs produce the same
            // DAG. Compare via JSON serialization since `WorkflowDag`
            // doesn't derive `Eq` (an `IterationDeclaration` carries
            // `f64`, but the meet path doesn't introduce iterations).
            let again = meet_in_the_middle(&forward, &backward, &atom_reg);
            let dag_again = match again {
                MeetResult::Connected { dag, .. } | MeetResult::PartiallyConnected { dag, .. } => {
                    dag
                }
                MeetResult::Disconnected { gaps, .. } => {
                    panic!("second meet returned Disconnected; gaps={gaps:?}")
                }
            };
            let lhs = serde_json::to_string(&dag).unwrap();
            let rhs = serde_json::to_string(&dag_again).unwrap();
            assert_eq!(lhs, rhs, "meet_in_the_middle is non-deterministic");
        }
        MeetResult::Disconnected { gaps, .. } => {
            panic!("meet returned Disconnected; gaps={gaps:?}");
        }
    }
}

/// A backward requirement with no forward producer must surface as a
/// gap, never as a phantom edge. We synthesize the scenario by passing
/// a backward requirement for an atom that lives in the registry but
/// whose inputs aren't reachable from the (empty) forward frontier.
#[test]
fn meet_with_no_forward_returns_disconnected_or_gaps() {
    let atom_reg =
        AtomRegistry::load_from_dir(std::path::Path::new("../../config/stage-atoms")).unwrap();
    let de_goal = DataProductContract::sample_de_table();
    let intent_no_inputs = WorkflowIntent {
        available_data: vec![],
        desired_outputs: vec![DesiredOutput {
            label: "differential expression table".into(),
            edam_data: Some(de_goal.semantic_type.stable_id()),
            edam_format: Some("format:3475".into()),
            human_readable: false,
        }],
        ..Default::default()
    };
    let forward = forward_search(&intent_no_inputs, &atom_reg, 8, 64);
    let backward = backward_search(&intent_no_inputs, &atom_reg, 8, 64);
    assert!(forward.is_empty(), "expected empty forward frontier");
    let result = meet_in_the_middle(&forward, &backward, &atom_reg);
    match result {
        MeetResult::Disconnected { gaps, .. } => {
            assert!(
                !gaps.is_empty(),
                "expected gaps for unsatisfied requirements"
            );
        }
        MeetResult::PartiallyConnected { gaps, .. } => {
            assert!(!gaps.is_empty(), "expected gaps when forward is empty");
        }
        MeetResult::Connected { .. } => {
            panic!("expected Disconnected/PartiallyConnected when forward is empty")
        }
    }
}
