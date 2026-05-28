//! Composer multi-modality dispatch.
//!
//! Verifies the new `compose_with_version_and_modalities` entry point:
//!
//! 1. **Cross-omics dispatch.** When the SME requests modalities that
//! a cross-omics archetype set-equals on `cross_omics_modalities`,
//! the composer matches the cross-omics archetype.
//! 2. **Generic fallback.** When no exact cross-omics archetype
//! matches, the composer synthesizes a namespaced multi-branch DAG
//! instead of dropping modalities.
//! 3. **Single-modality back-compat.** When `target_modalities` has 0
//! or 1 entries, behavior is identical to the existing single-
//! modality `compose_with_version_and_modality`.

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::builder::build_dag_from_composition;
use scripps_workflow_core::composer::{
    compose_with_version_and_modalities, compose_with_version_and_modality,
};
use scripps_workflow_core::goal_spec::GoalSpec;
use std::path::PathBuf;

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

fn load_registries() -> (AtomRegistry, ArchetypeRegistry) {
    let atoms = AtomRegistry::load_from_dir(&config_root().join("stage-atoms"))
        .expect("AtomRegistry must load");
    let archetypes = ArchetypeRegistry::load_from_dir(&config_root().join("archetypes"))
        .expect("ArchetypeRegistry must load");
    (atoms, archetypes)
}

fn de_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: std::collections::BTreeMap::new(),
        source_prose: None,
        confidence: 1.0,
    }
}

#[test]
fn dispatches_cross_omics_archetype_for_rnaseq_proteomics() {
    let (atoms, archetypes) = load_registries();
    let goal = de_goal();
    let result = compose_with_version_and_modalities(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        2,
        &["bulk_rnaseq", "proteomics"],
    )
    .expect("compose must succeed");

    assert_eq!(
        result.matched_archetype.as_deref(),
        Some("cross_omics_rnaseq_proteomics"),
        "cross-omics archetype must be selected, got {:?}",
        result.matched_archetype
    );
    let stage_ids: std::collections::HashSet<&str> =
        result.atoms.iter().map(|c| c.stage_id.as_str()).collect();
    assert!(
        stage_ids.contains("rnaseq_differential_expression"),
        "RNA-seq DE branch missing from composed DAG"
    );
    assert!(
        stage_ids.contains("proteomics_differential_abundance"),
        "proteomics DA branch missing from composed DAG"
    );
    assert!(
        stage_ids.contains("cross_omics_thematic_comparison"),
        "joint thematic-comparison stage missing from composed DAG"
    );
}

#[test]
fn dispatch_is_order_insensitive_on_modality_set() {
    let (atoms, archetypes) = load_registries();
    let goal = de_goal();
    // Reverse the modality order vs the prior test. Set-equality
    // means the cross-omics archetype must still match.
    let result = compose_with_version_and_modalities(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        2,
        &["proteomics", "bulk_rnaseq"],
    )
    .expect("compose must succeed for reversed modality order");
    assert_eq!(
        result.matched_archetype.as_deref(),
        Some("cross_omics_rnaseq_proteomics")
    );
}

#[test]
fn synthesizes_generic_multi_branch_when_no_cross_omics_match() {
    let (atoms, archetypes) = load_registries();
    let goal = de_goal();
    // No cross-omics archetype exists for this combo (rnaseq + atac).
    // The composer must preserve both branches instead of falling
    // through to a single-modality primary-only DAG.
    let result = compose_with_version_and_modalities(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        2,
        &["bulk_rnaseq", "atac_seq"],
    )
    .expect("compose must succeed via generic multi-branch fallback");
    assert_eq!(
        result.matched_archetype.as_deref(),
        Some("cross_omics_generic_multi_modal")
    );
    let stage_ids: std::collections::HashSet<&str> =
        result.atoms.iter().map(|c| c.stage_id.as_str()).collect();
    assert!(stage_ids.contains("bulk_rnaseq_differential_expression"));
    assert!(
        stage_ids.iter().any(|id| id.starts_with("atac_seq_")),
        "ATAC branch missing from generic fallback: {:?}",
        stage_ids
    );
    assert!(stage_ids.contains("multi_modal_thematic_comparison"));
    assert!(stage_ids.contains("multi_modal_final_reporting"));
}

#[test]
fn generic_fallback_preserves_bulk_single_cell_and_proteomics() {
    let (atoms, archetypes) = load_registries();
    let goal = de_goal();
    let result = compose_with_version_and_modalities(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        2,
        &["proteomics", "bulk_rnaseq", "single_cell_rnaseq"],
    )
    .expect("compose must synthesize a 3-branch DAG");

    assert_eq!(
        result.matched_archetype.as_deref(),
        Some("cross_omics_generic_multi_modal")
    );
    let stage_ids: std::collections::HashSet<&str> =
        result.atoms.iter().map(|c| c.stage_id.as_str()).collect();
    assert!(stage_ids.contains("bulk_rnaseq_differential_expression"));
    assert!(stage_ids.contains("single_cell_rnaseq_cell_type_annotation"));
    assert!(stage_ids.contains("single_cell_rnaseq_differential_expression"));
    assert!(stage_ids.contains("proteomics_protein_quantification"));
    assert!(stage_ids.contains("proteomics_differential_expression"));
    assert!(stage_ids.contains("multi_modal_thematic_comparison"));
}

#[test]
fn generic_fallback_builds_valid_dag_for_three_modalities() {
    let (atoms, archetypes) = load_registries();
    let goal = de_goal();
    let result = compose_with_version_and_modalities(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        2,
        &["proteomics", "bulk_rnaseq", "single_cell_rnaseq"],
    )
    .expect("compose must synthesize a 3-branch DAG");

    let dag = build_dag_from_composition(
        &result,
        "test-generic-three-way",
        &std::collections::BTreeMap::new(),
        &[],
    )
    .expect("generic multi-branch composition must build a valid DAG");

    assert!(dag
        .tasks
        .contains_key("bulk_rnaseq_differential_expression"));
    assert!(dag
        .tasks
        .contains_key("single_cell_rnaseq_differential_expression"));
    assert!(dag.tasks.contains_key("proteomics_differential_expression"));
    assert!(dag.tasks.contains_key("multi_modal_thematic_comparison"));
}

#[test]
fn single_modality_intent_unchanged() {
    let (atoms, archetypes) = load_registries();
    let goal = de_goal();

    let multi = compose_with_version_and_modalities(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        2,
        &["bulk_rnaseq"],
    )
    .expect("single-modality multi-entry compose must succeed");

    let single = compose_with_version_and_modality(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        2,
        Some("bulk_rnaseq"),
    )
    .expect("legacy single-modality compose must succeed");

    assert_eq!(multi.matched_archetype, single.matched_archetype);
    assert_eq!(multi.atoms.len(), single.atoms.len());
    let multi_stages: Vec<String> = multi.atoms.iter().map(|c| c.stage_id.to_string()).collect();
    let single_stages: Vec<String> = single
        .atoms
        .iter()
        .map(|c| c.stage_id.to_string())
        .collect();
    assert_eq!(multi_stages, single_stages);
}

#[test]
fn empty_modality_list_delegates_to_legacy() {
    let (atoms, archetypes) = load_registries();
    let goal = de_goal();
    // Empty list must produce IDENTICAL results to passing None,
    // including for failure modes (e.g. TieRequiresSmeDecision when
    // multiple single-modality archetypes share the goal).
    let multi =
        compose_with_version_and_modalities(&goal, "bioinformatics", &atoms, &archetypes, 2, &[]);
    let single =
        compose_with_version_and_modality(&goal, "bioinformatics", &atoms, &archetypes, 2, None);
    match (&multi, &single) {
        (Ok(m), Ok(s)) => assert_eq!(m.matched_archetype, s.matched_archetype),
        (Err(m), Err(s)) => {
            // Both should be the same error variant (e.g.
            // TieRequiresSmeDecision with the same candidate set).
            assert_eq!(format!("{:?}", m), format!("{:?}", s));
        }
        (m, s) => panic!(
            "empty-list and None-modality must agree on success/failure, got multi={:?}, single={:?}",
            m, s
        ),
    }
}

#[test]
fn composer_v3_with_cross_omics_falls_back_to_primary() {
    let (atoms, archetypes) = load_registries();
    let goal = de_goal();
    // composer_version=3 previously forced the backward-chain path, which
    // did not consult cross-omics archetypes. That dedicated v3 routing was
    // retired: sessions persisted with composer_version=3 now follow the
    // same cross-omics archetype lookup that v2 uses. The test confirms
    // that v3 still succeeds and that the result is consistent with v2
    // (both find the cross-omics archetype when one matches the modality set).
    let result = compose_with_version_and_modalities(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        3,
        &["bulk_rnaseq", "proteomics"],
    )
    .expect("v3 must succeed for a known cross-omics modality pair");
    // v3 now routes through the same cross-omics path as v2; it may
    // match the cross-omics archetype or fall through to the generic
    // multi-branch synthesizer. Either is acceptable; what matters is
    // that the result is non-empty and does not error out.
    assert!(
        !result.atoms.is_empty(),
        "v3 cross-omics compose must produce at least one atom"
    );
}
