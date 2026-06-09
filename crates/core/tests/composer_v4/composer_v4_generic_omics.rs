//! `generic_omics` archetype reachability.
//!
//! Off-topic prose that mentions omics data but carries no specific
//! modality or goal phrase should compose into the universal
//! `data_acquisition → generic_summary` pipeline via the v4 dispatcher's
//! `generic_omics`-modality + `research`-project-class fallback path.
//! The fallback is MODALITY-AGNOSTIC: it must NOT include `raw_qc`
//! (sequencing FastQC/MultiQC, which requires FASTQ `raw_reads`), or it
//! hard-blocks on any non-sequencing input (e.g. tabular metabolomics).
//! Further steps surface as SME-driven amendments rather than
//! auto-emitted.

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer::compose_with_modalities_full;
use ecaa_workflow_core::goal_spec::GoalSpec;
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

/// Off-topic prose ("Run quality control on some omics data") — bare
/// modality, no specific goal phrase. v4 dispatch must resolve to the
/// `generic_omics` archetype's MODALITY-AGNOSTIC pipeline:
/// `data_acquisition → generic_summary`, with NO sequencing-specific
/// `raw_qc` (which would block on tabular/non-FASTQ inputs).
#[test]
fn generic_omics_off_topic_prose_emits_executable_dag() {
    let (atoms, archetypes) = workspace_config();

    let goal = GoalSpec {
        // `data:0006` is the generic "Data" EDAM parent; the
        // generic_omics archetype's `goal_data` matches exactly.
        edam_data: "data:0006".into(),
        // `format:3475` (Tabular text) is the archetype's `goal_format`;
        // mismatched formats won't trigger the fallback path.
        edam_format: Some("format:3475".into()),
        modifiers: Default::default(),
        source_prose: Some("Run quality control on some omics data".into()),
        confidence: 0.0,
    };

    let result = compose_with_modalities_full(
        &goal,
        "research",
        &atoms,
        &archetypes,
        &["generic_omics"],
        None,
        None,
        None,
    )
    .expect("generic_omics fallback should compose");

    let composition = &result.composition;
    assert!(
        !composition.atoms.is_empty(),
        "generic_omics archetype must emit at least one node; got empty composition"
    );

    let stage_ids: std::collections::BTreeSet<&str> = composition
        .atoms
        .iter()
        .map(|c| c.stage_id.as_str())
        .collect();

    // Reachability smoke only — the `research` path composes via the v4
    // search seed (not the archetype scaffold), so it surfaces many atoms.
    // The deterministic modality-agnostic guarantee is asserted on the
    // archetype-scaffold (bioinformatics) path in
    // `generic_omics_scaffold_is_modality_agnostic`.
    assert!(
        stage_ids.iter().any(|s| *s == "generic_summary"),
        "generic_omics must reach generic_summary; got stages={:?}",
        stage_ids
    );
}

/// The `generic_omics` archetype scaffold is the MODALITY-AGNOSTIC catch-all
/// fallback: it must run `data_acquisition → generic_summary` with NO
/// sequencing-specific `raw_qc`. `raw_qc` requires FASTQ `raw_reads`
/// (`data:2044`), so a tabular/non-sequencing input (e.g. metabolomics) makes
/// the agent hard-block (`MissingRawFastqInputs`), which stalls the whole
/// reporting chain. QC belongs in the modality-specific sequencing archetypes,
/// not the universal fallback.
#[test]
fn generic_omics_scaffold_is_modality_agnostic() {
    let (atoms, archetypes) = workspace_config();
    let workflow_dag = compose_generic_omics_bioinformatics(&atoms, &archetypes);
    let node_ids: std::collections::BTreeSet<&str> =
        workflow_dag.nodes.iter().map(|n| n.id.as_str()).collect();

    assert!(
        !node_ids.contains("raw_qc") && !node_ids.contains("validate_raw_qc"),
        "generic_omics scaffold must NOT include raw_qc (sequencing FASTQ QC); got {:?}",
        node_ids
    );
    assert!(
        node_ids.contains("data_acquisition") && node_ids.contains("generic_summary"),
        "generic_omics scaffold must run data_acquisition -> generic_summary; got {:?}",
        node_ids
    );
}

/// Compose the generic_omics archetype seed via the production
/// (`bioinformatics` project-class) path. With `project_class =
/// bioinformatics` the generic_omics archetype matches definitively
/// (`modality_hint = generic_omics`) and its 16-node scaffold wins
/// scoring over the search seed — the same path the deterministic CLI
/// `intake` (and the corpus oracle) take.
fn compose_generic_omics_bioinformatics(
    atoms: &AtomRegistry,
    archetypes: &ArchetypeRegistry,
) -> ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag {
    let goal = GoalSpec {
        // generic_omics `goal_data`/`goal_format` — exact match makes the
        // archetype the definitive seed.
        edam_data: "data:0006".into(),
        edam_format: Some("format:3475".into()),
        modifiers: Default::default(),
        source_prose: Some("Run quality control on some omics data".into()),
        confidence: 0.0,
    };
    let result = compose_with_modalities_full(
        &goal,
        "bioinformatics",
        atoms,
        archetypes,
        &["generic_omics"],
        None,
        None,
        None,
    )
    .expect("generic_omics archetype should compose");
    assert_eq!(
        result.composition.matched_archetype.as_deref(),
        Some("generic_omics"),
        "generic_omics must be the definitive archetype seed on the bioinformatics path"
    );
    result
        .workflow_dag
        .expect("v4 path must populate ComposerOutput.workflow_dag")
}

/// B1: `generic_summary` carries `attributes.candidate_tools`, so the v4
/// discover-companion + survey synthesis passes must surface
/// `discover_generic_summary` and `survey_method_landscape` on the
/// generic_omics route — the same discovery layer every method-bearing
/// modality gets. Without `candidate_tools` on the catch-all analysis
/// atom there is no method-choice signal and the SME has no discovery
/// node to pin a method against.
#[test]
fn generic_omics_synthesizes_discover_and_survey() {
    let (atoms, archetypes) = workspace_config();
    let workflow_dag = compose_generic_omics_bioinformatics(&atoms, &archetypes);
    let node_ids: std::collections::BTreeSet<&str> =
        workflow_dag.nodes.iter().map(|n| n.id.as_str()).collect();

    assert!(
        node_ids.contains("discover_generic_summary"),
        "generic_omics must synthesize discover_generic_summary once generic_summary \
         carries candidate_tools; got node ids={:?}",
        node_ids
    );
    assert!(
        node_ids.contains("survey_method_landscape"),
        "generic_omics must synthesize survey_method_landscape gating the discover_* \
         nodes; got node ids={:?}",
        node_ids
    );
}

/// B2a: the generic_omics archetype declares the opt-in literature atoms
/// (`review_prior_work` + `contextualize_findings_with_literature`), so
/// the lifted seed carries them before the gate runs — mirroring the
/// bulk_rnaseq_de reference wiring. The gate (B2b) is what drops them
/// when no literature intent is present.
#[test]
fn generic_omics_archetype_declares_literature_atoms() {
    let (atoms, archetypes) = workspace_config();
    let workflow_dag = compose_generic_omics_bioinformatics(&atoms, &archetypes);
    let node_ids: std::collections::BTreeSet<&str> =
        workflow_dag.nodes.iter().map(|n| n.id.as_str()).collect();

    assert!(
        node_ids.contains("review_prior_work"),
        "generic_omics archetype must declare review_prior_work; got {:?}",
        node_ids
    );
    assert!(
        node_ids.contains("contextualize_findings_with_literature"),
        "generic_omics archetype must declare contextualize_findings_with_literature; got {:?}",
        node_ids
    );
}

/// B2b: literature contextualization is unconditional. The prune helper
/// is a no-op: the literature atoms (and their validate companions)
/// survive composition regardless of the `requested` flag — there is no
/// opt-in gate. The catch-all DAG always carries the literature family.
#[test]
fn generic_omics_literature_atoms_always_survive() {
    use ecaa_workflow_core::composer::prune_literature_atoms_from_workflow_dag;

    let (atoms, archetypes) = workspace_config();

    for requested in [true, false] {
        let mut dag = compose_generic_omics_bioinformatics(&atoms, &archetypes);
        let dropped = prune_literature_atoms_from_workflow_dag(&mut dag, requested);
        assert!(
            dropped.is_empty(),
            "prune must be a no-op (requested={requested}); dropped {:?}",
            dropped
        );
        let ids: std::collections::BTreeSet<&str> =
            dag.nodes.iter().map(|n| n.id.as_str()).collect();
        for lit in [
            "review_prior_work",
            "contextualize_findings_with_literature",
            "validate_review_prior_work",
            "validate_contextualize_findings_with_literature",
        ] {
            assert!(
                ids.contains(lit),
                "{lit} must survive unconditionally (requested={requested}); got {:?}",
                ids
            );
        }
        // The core catch-all terminals coexist with the literature family.
        assert!(
            ids.contains("generic_summary")
                && ids.contains("reporting")
                && ids.contains("final_reporting"),
            "core catch-all atoms must be present; got {:?}",
            ids
        );
    }
}
