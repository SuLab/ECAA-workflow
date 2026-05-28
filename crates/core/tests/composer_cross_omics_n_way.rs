//! N-way (3+) cross-omics composer dispatch.
//!
//! Validates that the composer's `find_match_cross_omics` set-equality
//! matcher works at any cardinality (the M3 implementation was already
//! N-way in shape — pairwise scoping was a workload decision, not an
//! implementation constraint), and that the new ternary archetype
//! (`cross_omics_rnaseq_atac_chip`) composes via `compose:`
//! inheritance rather than inlining the three branches.

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::composer::{compose_with_version_and_modalities, resolve_inheritance};
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
fn ternary_archetype_loads_with_three_modalities() {
    let (_atoms, archetypes) = load_registries();
    let arch = archetypes
        .get("cross_omics_rnaseq_atac_chip")
        .expect("cross_omics_rnaseq_atac_chip must be registered");
    assert_eq!(arch.cross_omics_modalities.len(), 3);
    let mods: std::collections::HashSet<&str> = arch
        .cross_omics_modalities
        .iter()
        .map(String::as_str)
        .collect();
    assert!(mods.contains("bulk_rnaseq"));
    assert!(mods.contains("atac_seq"));
    assert!(mods.contains("chip_seq"));
}

#[test]
fn ternary_archetype_uses_compose_inheritance() {
    let (_atoms, archetypes) = load_registries();
    let arch = archetypes.get("cross_omics_rnaseq_atac_chip").unwrap();
    assert_eq!(
        arch.compose.len(),
        3,
        "ternary archetype must declare 3 inheritance refs (bulk_rnaseq_de, atac_seq_peaks, chip_seq_peaks), got {:?}",
        arch.compose
    );
    // Sanity: each compose ref carries a distinct id_prefix.
    let prefixes: std::collections::HashSet<Option<&str>> = arch
        .compose
        .iter()
        .map(|c| c.id_prefix.as_deref())
        .collect();
    assert_eq!(
        prefixes.len(),
        3,
        "each branch must have a distinct id_prefix"
    );
}

#[test]
fn flatten_ternary_archetype_includes_all_three_branches() {
    let (_atoms, archetypes) = load_registries();
    let arch = archetypes.get("cross_omics_rnaseq_atac_chip").unwrap();
    let flat = resolve_inheritance(arch, &archetypes).expect("flatten must succeed");

    let stage_ids: std::collections::HashSet<&str> = flat
        .atoms
        .iter()
        .map(|a| a.alias.as_deref().unwrap_or(a.atom_id.as_str()))
        .collect();

    // RNA-seq branch (rnaseq_*).
    assert!(
        stage_ids.iter().any(|s| s.starts_with("rnaseq_alignment")),
        "rnaseq_alignment missing from flattened ternary archetype"
    );
    // ATAC branch (atac_*).
    assert!(
        stage_ids.iter().any(|s| s.starts_with("atac_alignment")),
        "atac_alignment missing — branch not inherited via compose:"
    );
    // ChIP branch (chip_*).
    assert!(
        stage_ids.iter().any(|s| s.starts_with("chip_alignment")),
        "chip_alignment missing — branch not inherited via compose:"
    );
    // The archetype's own atoms (alignment validator + reporters).
    assert!(stage_ids.contains("cross_omics_alignment_check"));
    assert!(stage_ids.contains("cross_omics_thematic_comparison"));
    assert!(stage_ids.contains("cross_omics_final_reporting"));
}

#[test]
fn dispatches_ternary_archetype_for_rnaseq_atac_chip() {
    let (atoms, archetypes) = load_registries();
    let goal = de_goal();
    let result = compose_with_version_and_modalities(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        2,
        &["bulk_rnaseq", "atac_seq", "chip_seq"],
    )
    .expect("ternary dispatch must succeed");

    assert_eq!(
        result.matched_archetype.as_deref(),
        Some("cross_omics_rnaseq_atac_chip"),
        "ternary archetype must be selected"
    );
    let stage_ids: std::collections::HashSet<&str> =
        result.atoms.iter().map(|c| c.stage_id.as_str()).collect();

    // All three branch DEs / peak-callings must be present.
    assert!(
        stage_ids.iter().any(|s| s.starts_with("rnaseq_")),
        "rnaseq branch missing"
    );
    assert!(
        stage_ids.iter().any(|s| s.starts_with("atac_")),
        "atac branch missing"
    );
    assert!(
        stage_ids.iter().any(|s| s.starts_with("chip_")),
        "chip branch missing"
    );
    assert!(stage_ids.contains("cross_omics_alignment_check"));
    assert!(stage_ids.contains("cross_omics_thematic_comparison"));
}

#[test]
fn ternary_dispatch_is_order_insensitive() {
    let (atoms, archetypes) = load_registries();
    let goal = de_goal();
    let r1 = compose_with_version_and_modalities(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        2,
        &["chip_seq", "bulk_rnaseq", "atac_seq"],
    )
    .unwrap();
    let r2 = compose_with_version_and_modalities(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        2,
        &["bulk_rnaseq", "atac_seq", "chip_seq"],
    )
    .unwrap();
    assert_eq!(r1.matched_archetype, r2.matched_archetype);
}

#[test]
fn unregistered_triple_synthesizes_generic_multi_branch_dag() {
    let (atoms, archetypes) = load_registries();
    let goal = de_goal();
    // No cross-omics archetype covers (bulk_rnaseq, proteomics, metagenomics).
    let result = compose_with_version_and_modalities(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        2,
        &["bulk_rnaseq", "proteomics", "metagenomics"],
    )
    .expect("generic fallback must succeed");
    assert_eq!(
        result.matched_archetype.as_deref(),
        Some("cross_omics_generic_multi_modal")
    );
    let stage_ids: std::collections::HashSet<&str> =
        result.atoms.iter().map(|c| c.stage_id.as_str()).collect();
    assert!(stage_ids.iter().any(|id| id.starts_with("bulk_rnaseq_")));
    assert!(stage_ids.iter().any(|id| id.starts_with("proteomics_")));
    assert!(stage_ids.iter().any(|id| id.starts_with("metagenomics_")));
    assert!(stage_ids.contains("multi_modal_thematic_comparison"));
}
