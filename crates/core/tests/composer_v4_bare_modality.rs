//! v4 must produce a DAG when the intent carries a modality but
//! no goal-bearing match.
//!
//! Prose like "single cell scRNA-seq from human IVD samples with 10x
//! Chromium" classifies to a modality but leaves
//! `Classification.goal = None`. v4 must still produce a DAG by
//! falling through to the primary archetype for the requested
//! modality (Option B in the gap-closure plan §Deferred).
//!
//! These tests exercise the v4 dispatcher directly with a goal whose
//! `edam_data` doesn't match any archetype's `goal_data`. The
//! modality-only fallback in `try_archetype_seed` must engage and
//! return the primary archetype for the requested modality.

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::composer::compose_with_version_and_modalities_full;
use scripps_workflow_core::goal_spec::GoalSpec;
use std::path::Path;

fn workspace_config() -> (AtomRegistry, ArchetypeRegistry) {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().unwrap().parent().unwrap();
    let atoms =
        AtomRegistry::load_from_dir(&workspace.join("config/stage-atoms")).expect("load atoms");
    let archetypes = ArchetypeRegistry::load_from_dir(&workspace.join("config/archetypes"))
        .expect("load archetypes");
    (atoms, archetypes)
}

/// Synthesize a goal whose `edam_data` is intentionally non-archetype
/// matching, mirroring the bare-modality case where the conversation
/// crate hasn't been able to infer a goal IRI either. The modality-only
/// fallback must still fire.
fn bare_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:9999".into(),
        edam_format: None,
        modifiers: Default::default(),
        source_prose: Some("bare-modality intake (no goal phrase classified)".into()),
        confidence: 0.0,
    }
}

#[test]
fn bare_modality_bulk_rnaseq_emits_de_archetype() {
    let (atoms, archetypes) = workspace_config();
    let goal = bare_goal();
    let result = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        4,
        &["bulk_rnaseq"],
        None,
        None,
        None,
    )
    .expect("dispatch should succeed for bare modality");
    let composition = &result.composition;
    assert!(
        !composition.atoms.is_empty(),
        "bare-modality bulk_rnaseq must emit non-empty composition"
    );
    assert_eq!(
        composition.matched_archetype.as_deref(),
        Some("bulk_rnaseq_de"),
        "primary archetype for bulk_rnaseq must be bulk_rnaseq_de; got {:?}",
        composition.matched_archetype
    );
    let stage_ids: std::collections::BTreeSet<&str> = composition
        .atoms
        .iter()
        .map(|c| c.stage_id.as_str())
        .collect();
    assert!(
        stage_ids.contains("differential_expression"),
        "primary archetype must include differential_expression; got {:?}",
        stage_ids
    );
}

#[test]
fn bare_modality_scrnaseq_emits_clustering_archetype() {
    let (atoms, archetypes) = workspace_config();
    let goal = bare_goal();
    let result = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        4,
        &["single_cell_rnaseq"],
        None,
        None,
        None,
    )
    .expect("dispatch should succeed for bare modality");
    let composition = &result.composition;
    assert!(
        !composition.atoms.is_empty(),
        "bare-modality scrnaseq must emit non-empty composition"
    );
    assert_eq!(
        composition.matched_archetype.as_deref(),
        Some("single_cell_de"),
        "primary archetype for single_cell_rnaseq must be single_cell_de; got {:?}",
        composition.matched_archetype
    );
}

#[test]
fn bare_modality_chip_seq_emits_peaks_archetype() {
    let (atoms, archetypes) = workspace_config();
    let goal = bare_goal();
    let result = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        4,
        &["chip_seq"],
        None,
        None,
        None,
    )
    .expect("dispatch should succeed for bare modality");
    let composition = &result.composition;
    assert!(
        !composition.atoms.is_empty(),
        "bare-modality chip_seq must emit non-empty composition"
    );
    assert_eq!(
        composition.matched_archetype.as_deref(),
        Some("chip_seq_peaks"),
        "primary archetype for chip_seq must be chip_seq_peaks; got {:?}",
        composition.matched_archetype
    );
}

#[test]
fn bare_modality_variant_calling_emits_germline_archetype() {
    let (atoms, archetypes) = workspace_config();
    let goal = bare_goal();
    let result = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        4,
        &["variant_calling"],
        None,
        None,
        None,
    )
    .expect("dispatch should succeed for bare modality");
    let composition = &result.composition;
    assert!(
        !composition.atoms.is_empty(),
        "bare-modality variant_calling must emit non-empty composition"
    );
    assert_eq!(
        composition.matched_archetype.as_deref(),
        Some("variant_calling_germline"),
        "primary archetype for variant_calling must be variant_calling_germline; got {:?}",
        composition.matched_archetype
    );
}

#[test]
fn bare_modality_generic_omics_clinical_trial_emits_executable_dag() {
    // Project-class fallback: bare-modality classification with
    // project_class=clinical_trial should dispatch through the
    // `clinical_trial_analysis` archetype's goal. v4 may pick either
    // the archetype seed or the search seed as the primary; the
    // contract is that some DAG emits.
    let (atoms, archetypes) = workspace_config();
    let goal = bare_goal();
    let result = compose_with_version_and_modalities_full(
        &goal,
        "clinical_trial",
        &atoms,
        &archetypes,
        4,
        &["generic_omics"],
        None,
        None,
        None,
    )
    .expect("dispatch should succeed for bare-modality clinical_trial");
    let composition = &result.composition;
    assert!(
        !composition.atoms.is_empty(),
        "bare-modality clinical_trial must emit non-empty composition"
    );
}

#[test]
fn bare_modality_generic_omics_time_series_emits_executable_dag() {
    // Project-class fallback: bare-modality classification with
    // project_class=time_series_forecast should dispatch through
    // the `time_series_forecast` archetype's goal.
    let (atoms, archetypes) = workspace_config();
    // Mirror what `infer_goal_for_modalities` produces for bare
    // time_series intake: goal sourced from the archetype's metadata,
    // including the `kind: forecast` modifier from `goal_kind_hint`.
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: Default::default(),
        source_prose: Some("inferred from time_series_forecast archetype".into()),
        confidence: 0.5,
    };
    let result = compose_with_version_and_modalities_full(
        &goal,
        "time_series_forecast",
        &atoms,
        &archetypes,
        4,
        &["generic_omics"],
        None,
        None,
        None,
    )
    .expect("dispatch should succeed for bare-modality time_series_forecast");
    let composition = &result.composition;
    assert!(
        !composition.atoms.is_empty(),
        "bare-modality time_series_forecast must emit non-empty composition"
    );
}

#[test]
fn find_primary_for_modality_picks_canonical_archetype() {
    let (_atoms, archetypes) = workspace_config();
    // Spot-check the per-modality primary picks. Each must select the
    // canonical "do the modality" archetype, not a specialized variant.
    for (modality, expected_primary) in [
        ("bulk_rnaseq", "bulk_rnaseq_de"),
        ("single_cell_rnaseq", "single_cell_de"),
        ("chip_seq", "chip_seq_peaks"),
        ("atac_seq", "atac_seq_peaks"),
        ("variant_calling", "variant_calling_germline"),
        ("metagenomics", "metagenomics_taxonomic"),
    ] {
        let primary = archetypes.find_primary_for_modality(modality, "bioinformatics");
        assert_eq!(
            primary.map(|a| a.id.as_str()),
            Some(expected_primary),
            "primary archetype for {} must be {}",
            modality,
            expected_primary
        );
    }
}

#[test]
fn find_primary_for_modality_falls_through_to_project_class() {
    // Project-class fallback. When no archetype's
    // `modality_hint` matches, fall through to project-class-routed
    // archetypes (modality_hint unset). For an unknown modality with
    // project_class=clinical_trial, we should still find
    // `clinical_trial_analysis` because it's the canonical project-
    // class-routed archetype.
    let (_atoms, archetypes) = workspace_config();
    let primary = archetypes.find_primary_for_modality("unknown_modality", "clinical_trial");
    assert_eq!(
        primary.map(|a| a.id.as_str()),
        Some("clinical_trial_analysis"),
        "unknown modality with clinical_trial project_class falls through to clinical_trial_analysis"
    );

    // For an unknown project_class, no fallback exists.
    let primary = archetypes.find_primary_for_modality("unknown_modality", "unknown_class");
    assert!(
        primary.is_none(),
        "unknown modality + unknown project_class returns None; got {:?}",
        primary.map(|a| &a.id)
    );
}
