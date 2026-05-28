//! Tier 18.1 scenario-10 regression — v4 dispatch must succeed when
//! the SME prose names a primary modality whose archetype catalog
//! does NOT include the classifier-extracted goal IRI, but other
//! modalities' archetypes do.
//!
//! Scenario: ATAC-seq prose with the secondary phrase "linked to
//! differentially expressed genes from a paired RNA-seq dataset".
//! The classifier picks `modality=atac_seq` (primary modality
//! keywords win the score) plus `goal.edam_data=data:0951` (the
//! DE goal phrase fires). The atac_seq archetype catalog only
//! contains `atac_seq_peaks` (goal_data=data:1255). DE archetypes
//! exist for other modalities (`bulk_rnaseq_de`, `single_cell_vdj`,
//! `ribo_seq_translation`, …), so the `any_goal_data_match` check
//! in `compose_v4_dispatch_full` sees a positive signal and skips
//! the bare-modality rewrite. The unmatched goal then propagates
//! into the planner which fails with
//! `GoalUnreachable { goal: "data:0951 (format:3475)" }`.
//!
//! The rewrite must consider only modality-matching archetypes when
//! deciding whether the goal is reachable; ATAC-seq gets rewritten
//! to the atac_seq_peaks goal pair and dispatch succeeds. Without
//! the modality filter the dispatch errors with `GoalUnreachable`.

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::composer::compose_with_version_and_modalities_full;
use scripps_workflow_core::goal_spec::GoalSpec;
use std::collections::BTreeMap;
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

/// Synthesize the exact classifier output for Tier 18.1 scenario 10:
/// modality=atac_seq, but the goal is the DE goal because the
/// classifier picked up "differentially expressed" in the prose.
fn atac_with_de_goal() -> GoalSpec {
    let mut modifiers = BTreeMap::new();
    modifiers.insert("kind".to_string(), "differential_expression".to_string());
    GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers,
        source_prose: Some(
            "ATAC-seq of human T cells before and after activation. Identify open \
             chromatin regions, infer transcription factor footprints, and link peaks \
             to differentially expressed genes from a paired RNA-seq dataset."
                .into(),
        ),
        confidence: 0.67,
    }
}

#[test]
fn atac_seq_with_de_goal_does_not_fail_with_goal_unreachable() {
    let (atoms, archetypes) = workspace_config();
    let goal = atac_with_de_goal();
    let result = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        4,
        &["atac_seq"],
        None,
        None,
        None,
    );
    match result {
        Ok(out) => {
            assert!(
                !out.composition.atoms.is_empty(),
                "atac_seq DE-goal dispatch must produce a non-empty composition; \
                 got matched_archetype={:?}",
                out.composition.matched_archetype,
            );
            let stage_ids: std::collections::BTreeSet<&str> = out
                .composition
                .atoms
                .iter()
                .map(|c| c.stage_id.as_str())
                .collect();
            // The primary atac_seq archetype is atac_seq_peaks; its
            // canonical workflow must include peak_calling.
            assert!(
                stage_ids.contains("peak_calling"),
                "atac_seq dispatch must include peak_calling; got {:?}",
                stage_ids,
            );
        }
        Err(e) => {
            panic!(
                "atac_seq + DE goal dispatch must NOT return GoalUnreachable; \
                 the bare-modality rewrite should fire because no atac_seq \
                 archetype matches data:0951. Got error: {:?}",
                e,
            );
        }
    }
}
