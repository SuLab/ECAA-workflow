//! Backward goal-decomposition over the atom registry.
//!
//! Given a `WorkflowIntent`'s desired outputs, walk the atom registry
//! finding every `(atom_id, port_index)` whose output port unifies with
//! a desired-output `DataProductContract`. Each visited atom's input
//! ports become new subgoals enqueued at `depth + 1`, until `max_depth`
//! is exhausted or the queue empties. Bounded by `max_branches`. Output
//! is sorted by `(atom_id, port_index, depth)` for byte-stable replay.

use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer_v4::backward_search::{backward_search, BackwardRequirement};
use ecaa_workflow_core::workflow_contracts::data_product::DataProductContract;
use ecaa_workflow_core::workflow_contracts::workflow_intent::{DesiredOutput, WorkflowIntent};

/// Backward search from a DE-table goal must surface the canonical
/// bulk RNA-seq chain: `differential_expression` is the immediate
/// goal-producer; `quantification` and `alignment` appear at deeper
/// layers as backward-decomposition pulls in the chain of
/// dependencies. The `raw_qc` atom lives behind `sequence_trimming`
/// which is behind `alignment`, so it shows up only at higher depths.
///
/// The brief asks for a `desired_outputs: vec![DataProductContract::sample_de_table()]`-style
/// intent, but today's `WorkflowIntent.desired_outputs` is
/// `Vec<DesiredOutput>` (label + edam_data + edam_format). We synthesize
/// a typed goal from the `sample_de_table()` fixture's EDAM info so the
/// test exercises the same matching logic the brief intended.
#[test]
fn backward_search_recovers_de_table_chain() {
    let atom_reg =
        AtomRegistry::load_from_dir(std::path::Path::new("../../config/stage-atoms")).unwrap();
    let de_goal = DataProductContract::sample_de_table();
    let intent = WorkflowIntent {
        available_data: vec![],
        desired_outputs: vec![DesiredOutput {
            label: "differential expression table".into(),
            edam_data: Some(de_goal.semantic_type.stable_id()),
            edam_format: Some("format:3475".into()),
            human_readable: false,
        }],
        ..Default::default()
    };
    let reqs = backward_search(&intent, &atom_reg, 6, 64);

    let chain: Vec<&str> = reqs.iter().map(|r| r.atom_id.as_str()).collect();
    // DE table requires differential_expression which requires
    // (transitively, via normalisation + qc_preprocessing)
    // quantification which requires alignment which requires
    // sequence_trimming which requires raw_qc. Each of these atoms'
    // output ports unify with a backward-derived DataProductContract
    // at some depth ≤ 6.
    assert!(
        chain.contains(&"differential_expression"),
        "expected differential_expression in chain: {chain:?}"
    );
    assert!(
        chain.contains(&"quantification"),
        "expected quantification in chain: {chain:?}"
    );
    assert!(
        chain.contains(&"alignment"),
        "expected alignment in chain: {chain:?}"
    );
}

#[test]
fn backward_search_is_deterministic() {
    let atom_reg =
        AtomRegistry::load_from_dir(std::path::Path::new("../../config/stage-atoms")).unwrap();
    let de_goal = DataProductContract::sample_de_table();
    let intent = WorkflowIntent {
        available_data: vec![],
        desired_outputs: vec![DesiredOutput {
            label: "differential expression table".into(),
            edam_data: Some(de_goal.semantic_type.stable_id()),
            edam_format: Some("format:3475".into()),
            human_readable: false,
        }],
        ..Default::default()
    };
    let baseline: Vec<BackwardRequirement> = backward_search(&intent, &atom_reg, 6, 64);
    for _ in 0..20 {
        assert_eq!(
            backward_search(&intent, &atom_reg, 6, 64),
            baseline,
            "backward_search must be deterministic across repeated calls"
        );
    }
}
