//! Focused load test for the cross-omics `bulk_rnaseq + proteomics`
//! archetype YAML.
//!
//! Validates the specific shape downstream code (M3 composer dispatch,
//! M5 regression scenario) will consume:
//!
//! - `cross_omics_modalities` carries both `bulk_rnaseq` and
//! `proteomics`.
//! - The atom list contains both the RNA-seq branch (alignment,
//! quantification, normalisation, differential_expression) and the
//! proteomics branch (peptide_search, protein_quantification,
//! differential_abundance) with disjoint stage-id namespaces via
//! `alias:`.
//! - Both branches converge on the joint `cross_omics_thematic_comparison`
//! reporting stage.

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use std::path::PathBuf;

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

#[test]
fn cross_omics_rnaseq_proteomics_archetype_loads() {
    let reg = ArchetypeRegistry::load_from_dir(&config_root().join("archetypes"))
        .expect("ArchetypeRegistry::load_from_dir must succeed");

    let arch = reg
        .get("cross_omics_rnaseq_proteomics")
        .expect("cross_omics_rnaseq_proteomics archetype must be registered");

    assert_eq!(arch.id, "cross_omics_rnaseq_proteomics");
    assert_eq!(arch.project_class, "bioinformatics");
    assert_eq!(
        arch.modality_hint.as_deref(),
        Some("cross_omics_rnaseq_proteomics")
    );

    // M2 wire shape — cross_omics_modalities must list both branches.
    assert_eq!(
        arch.cross_omics_modalities.len(),
        2,
        "cross_omics_modalities must list both modalities, got {:?}",
        arch.cross_omics_modalities
    );
    let modality_set: std::collections::HashSet<&str> = arch
        .cross_omics_modalities
        .iter()
        .map(String::as_str)
        .collect();
    assert!(modality_set.contains("bulk_rnaseq"));
    assert!(modality_set.contains("proteomics"));
}

#[test]
fn cross_omics_archetype_has_both_branch_stages() {
    let reg = ArchetypeRegistry::load_from_dir(&config_root().join("archetypes"))
        .expect("ArchetypeRegistry::load_from_dir must succeed");
    let arch = reg.get("cross_omics_rnaseq_proteomics").unwrap();

    let stage_ids: std::collections::HashSet<String> = arch
        .atoms
        .iter()
        .map(|a| a.alias.clone().unwrap_or_else(|| a.atom_id.to_string()))
        .collect();

    // RNA-seq branch must be present (key stages).
    for required in [
        "rnaseq_alignment",
        "rnaseq_quantification",
        "rnaseq_differential_expression",
    ] {
        assert!(
            stage_ids.contains(required),
            "RNA-seq branch missing stage {}: have {:?}",
            required,
            stage_ids
        );
    }

    // Proteomics branch must be present (key stages).
    for required in [
        "proteomics_peptide_search",
        "proteomics_protein_quantification",
        "proteomics_differential_abundance",
    ] {
        assert!(
            stage_ids.contains(required),
            "Proteomics branch missing stage {}: have {:?}",
            required,
            stage_ids
        );
    }

    // Joint thematic comparison stage joins the two branches.
    assert!(
        stage_ids.contains("cross_omics_thematic_comparison"),
        "joint thematic comparison stage missing: have {:?}",
        stage_ids
    );

    // No name collisions: the archetype reuses atom ids
    // `data_acquisition` and `differential_expression` across the two
    // branches with `alias:`. Stage ids must be unique.
    assert_eq!(
        stage_ids.len(),
        arch.atoms.len(),
        "stage ids must be unique across both branches; duplicates: {} atoms vs {} unique ids",
        arch.atoms.len(),
        stage_ids.len()
    );
}

/// Production-exclusion gate (paper §10): cross-omics archetypes are
/// scaffolded (`production_ready: false`) and MUST NOT be selected by
/// the production matcher unless the `ECAA_ALLOW_SCAFFOLDED_ARCHETYPES`
/// opt-in is engaged. The default-policy registry (production) must
/// refuse to return a cross-omics archetype even for an intake whose
/// modality set exactly matches one.
#[test]
fn production_matcher_excludes_scaffolded_cross_omics_by_default() {
    // Default `load_from_dir` is the explicit-policy / dev constructor
    // and stays permissive; the production constructor denies scaffolded
    // archetypes. Exercise the production policy directly.
    let reg = ArchetypeRegistry::load_from_dir_with_policy(
        &config_root().join("archetypes"),
        /* allow_scaffolded = */ false,
    )
    .expect("ArchetypeRegistry::load_from_dir_with_policy must succeed");

    // The cross-omics archetype is still LOADED (so the catalog + UI can
    // see it), but it must be marked not-production-ready ...
    let arch = reg
        .get("cross_omics_rnaseq_proteomics")
        .expect("scaffolded archetype is still loaded into the catalog");
    assert!(
        !arch.production_ready,
        "cross-omics archetype must be marked production_ready: false"
    );

    // ... and the production matcher must NOT select it for an intake
    // whose modality set exactly matches.
    let matches = reg.find_match_cross_omics(
        "data:0951",
        Some("format:3475"),
        "bioinformatics",
        &["bulk_rnaseq", "proteomics"],
        None,
        false,
        "rna-seq and proteomics",
    );
    assert!(
        matches.is_empty(),
        "production matcher (allow_scaffolded=false) must not return a \
         scaffolded cross-omics archetype, got {:?}",
        matches.iter().map(|(a, _)| &a.id).collect::<Vec<_>>()
    );
}

/// With the explicit opt-in engaged, the production matcher DOES select
/// the scaffolded cross-omics archetype.
#[test]
fn production_matcher_includes_scaffolded_cross_omics_with_opt_in() {
    let reg = ArchetypeRegistry::load_from_dir_with_policy(
        &config_root().join("archetypes"),
        /* allow_scaffolded = */ true,
    )
    .expect("ArchetypeRegistry::load_from_dir_with_policy must succeed");

    let matches = reg.find_match_cross_omics(
        "data:0951",
        Some("format:3475"),
        "bioinformatics",
        &["bulk_rnaseq", "proteomics"],
        None,
        false,
        "rna-seq and proteomics",
    );
    assert!(
        matches
            .iter()
            .any(|(a, _)| a.id == "cross_omics_rnaseq_proteomics"),
        "with opt-in, matcher must return the cross-omics archetype, got {:?}",
        matches.iter().map(|(a, _)| &a.id).collect::<Vec<_>>()
    );
}

#[test]
fn cross_omics_thematic_comparison_depends_on_both_branches() {
    let reg = ArchetypeRegistry::load_from_dir(&config_root().join("archetypes"))
        .expect("ArchetypeRegistry::load_from_dir must succeed");
    let arch = reg.get("cross_omics_rnaseq_proteomics").unwrap();

    let join = arch
        .atoms
        .iter()
        .find(|a| a.alias.as_deref() == Some("cross_omics_thematic_comparison"))
        .expect("join stage must exist");

    let deps: std::collections::HashSet<&str> =
        join.depends_on.iter().map(String::as_str).collect();
    assert!(
        deps.contains("rnaseq_differential_expression"),
        "join must depend on RNA-seq DE branch, got {:?}",
        deps
    );
    assert!(
        deps.contains("proteomics_differential_abundance"),
        "join must depend on proteomics DA branch, got {:?}",
        deps
    );
}
