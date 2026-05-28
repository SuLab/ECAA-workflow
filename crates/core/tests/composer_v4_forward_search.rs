//! Forward reachability search over the atom registry.
//!
//! Given a `WorkflowIntent`'s `available_data`, walk the atom registry
//! emitting every `(atom_id, port_index)` pair whose input ports unify
//! with a reachable producer. Bounded by `max_depth` (BFS layers) and
//! `max_branches` (per-depth emission cap). Output is sorted by
//! `(depth, atom_id, port_index)` so the frontier is byte-deterministic.

use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer_v4::forward_search::forward_search;
use ecaa_workflow_core::workflow_contracts::{
    data_product::DataProductContract, workflow_intent::WorkflowIntent,
};

#[test]
fn forward_search_yields_atoms_consuming_available_data() {
    let atom_reg =
        AtomRegistry::load_from_dir(std::path::Path::new("../../config/stage-atoms")).unwrap();
    let intent = WorkflowIntent {
        available_data: vec![DataProductContract::sample_paired_fastq()],
        desired_outputs: vec![],
        ..Default::default()
    };
    let frontier = forward_search(
        &intent, &atom_reg, 4,  /* max_depth */
        64, /* max_branches */
    );
    let reached_atom_ids: std::collections::BTreeSet<&str> =
        frontier.iter().map(|r| r.atom_id.as_str()).collect();
    // raw_qc and alignment both accept paired FASTQ — expect them in
    // the frontier. raw_qc consumes FASTQ directly; alignment consumes
    // FASTQ via the same `data:2044` / `format:1930` pair.
    assert!(
        reached_atom_ids.contains("raw_qc"),
        "raw_qc missing from forward frontier: {reached_atom_ids:?}"
    );
    assert!(
        reached_atom_ids.contains("alignment"),
        "alignment missing from forward frontier: {reached_atom_ids:?}"
    );
}

#[test]
fn forward_search_respects_max_depth() {
    let atom_reg =
        AtomRegistry::load_from_dir(std::path::Path::new("../../config/stage-atoms")).unwrap();
    let intent = WorkflowIntent {
        available_data: vec![DataProductContract::sample_paired_fastq()],
        desired_outputs: vec![],
        ..Default::default()
    };
    let depth_1 = forward_search(&intent, &atom_reg, 1, 64).len();
    let depth_4 = forward_search(&intent, &atom_reg, 4, 64).len();
    assert!(
        depth_4 > depth_1,
        "depth=4 ({depth_4}) should reach more atoms than depth=1 ({depth_1})"
    );
}

#[test]
fn forward_search_is_deterministic() {
    let atom_reg =
        AtomRegistry::load_from_dir(std::path::Path::new("../../config/stage-atoms")).unwrap();
    let intent = WorkflowIntent {
        available_data: vec![DataProductContract::sample_paired_fastq()],
        desired_outputs: vec![],
        ..Default::default()
    };
    let baseline = forward_search(&intent, &atom_reg, 4, 64);
    for _ in 0..20 {
        assert_eq!(
            forward_search(&intent, &atom_reg, 4, 64),
            baseline,
            "forward_search must be deterministic across repeated calls"
        );
    }
}
