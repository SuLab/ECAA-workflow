//! Integration test for Proposal C — backward type-directed A* search
//! as the v4 planner's final fallback. Asserts that a `data:0006`
//! (generic summary) goal whose IRI doesn't appear in any archetype's
//! `goal_data` field still produces a composition containing the
//! load-bearing `generic_summary` atom, surfaced under either an
//! `adhoc_*` ad-hoc archetype id (the Proposal C synthesis path) or
//! the `generic_omics` weak-match fallback already in place.

#[test]
fn backward_search_synthesizes_chain_for_mr_goal() {
    use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
    use scripps_workflow_core::atom_registry::AtomRegistry;
    use scripps_workflow_core::composer::compose_with_version_and_modalities_full;
    use scripps_workflow_core::goal_spec::GoalSpec;

    let archetypes = ArchetypeRegistry::load_from_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/archetypes")
            .as_path(),
    )
    .unwrap();
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

    // Goal: a generic summary table (data:0006) from FASTQ inputs.
    // No archetype declares this `goal_data` exactly; backward_search
    // must find raw_qc → generic_summary or similar chain.
    let goal = GoalSpec {
        edam_data: "data:0006".into(),
        edam_format: Some("format:3475".into()),
        modifiers: std::collections::BTreeMap::new(),
        source_prose: Some("two-sample mendelian randomization".into()),
        confidence: 0.5,
    };
    let result = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        4,
        &[],
        None,
        None,
        None,
    )
    .unwrap();
    assert!(
        result
            .composition
            .atoms
            .iter()
            .any(|a| a.atom.id == "generic_summary"),
        "should synthesize generic_summary in chain",
    );
    let arch_id = result
        .composition
        .matched_archetype
        .as_deref()
        .unwrap_or("");
    assert!(
        arch_id.starts_with("adhoc_") || arch_id == "generic_omics",
        "matched archetype should be ad-hoc or generic_omics fallback; got {arch_id:?}",
    );
}
