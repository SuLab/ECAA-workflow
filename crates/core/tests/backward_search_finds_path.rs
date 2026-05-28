//! Integration test for Proposal C — backward type-directed A* search
//! over the real atom catalog. Verifies the search finds a chain of
//! atoms taking FASTQ (`data:2044`) intake to a peaks-shaped goal
//! (`data:1255` / BED `format:3003`) and that the chain contains the
//! load-bearing `alignment` + `peak_calling` steps.

#[test]
fn backward_search_finds_atac_peak_chain_from_fastq() {
    use ecaa_workflow_core::atom_registry::AtomRegistry;
    use ecaa_workflow_core::composer_v4::backward_search::{
        search_backward, BackwardSearchInput,
    };

    let atoms = AtomRegistry::load_from_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/stage-atoms")
            .as_path(),
    )
    .unwrap();

    let result = search_backward(BackwardSearchInput {
        goal_data: "data:1255".into(),              // Feature record (peaks)
        goal_format: Some("format:3003".into()),    // BED
        available_inputs: vec!["data:2044".into()], // FASTQ
        atom_registry: &atoms,
        max_depth: 10,
    });

    assert!(result.is_some(), "should find FASTQ → peaks chain");
    let chain = result.unwrap();
    let ids: Vec<_> = chain.iter().map(|c| c.atom.id.as_str()).collect();
    assert!(
        ids.contains(&"alignment"),
        "needs alignment step; got {ids:?}"
    );
    assert!(
        ids.contains(&"peak_calling"),
        "needs peak calling step; got {ids:?}"
    );
}
