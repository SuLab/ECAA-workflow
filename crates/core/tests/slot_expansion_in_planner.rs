//! Integration test for v4 planner slot-expansion (Proposal B).
//!
//! Validates that `try_cross_omics_archetype_seed` picks up the
//! `integrator` slot from goal.modifiers and expands the picked
//! archetype's atoms with the slot value's extra_atoms.
//!
//! Depends on `config/archetypes/cross_omics_rnaseq_proteomics.slots.yaml`
//! authored in Task 1.5.

#[test]
fn cross_omics_planner_picks_diablo_via_slot() {
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

    // Goal carries integrator=diablo modifier from classifier.
    let mut goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: std::collections::BTreeMap::new(),
        source_prose: Some("differential expression".into()),
        confidence: 1.0,
    };
    goal.modifiers.insert("integrator".into(), "diablo".into());

    let result = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        4,
        &["bulk_rnaseq", "proteomics"],
        None,
        None,
        None,
    )
    .unwrap();
    let atom_ids: std::collections::HashSet<_> = result
        .composition
        .atoms
        .iter()
        .map(|a| a.atom.id.as_str())
        .collect();
    assert!(
        atom_ids.contains("integrate_multi_omics_diablo"),
        "DIABLO slot extra atom should be in composition; got {:?}",
        atom_ids
    );
    assert!(atom_ids.contains("validate_sample_alignment_n_way"));
}
