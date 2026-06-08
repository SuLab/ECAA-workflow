//! `generic_omics` archetype reachability.
//!
//! Off-topic prose that mentions omics data but carries no specific
//! modality or goal phrase should compose into the universal `raw_qc →
//! generic_summary` pipeline via the v4 dispatcher's
//! `generic_omics`-modality + `research`-project-class fallback path.
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
/// `generic_omics` archetype's `raw_qc → generic_summary` pipeline.
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

    assert!(
        stage_ids.iter().any(|s| s.contains("raw_qc")),
        "generic_omics must reach raw_qc as starter node; got stages={:?}",
        stage_ids
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

/// B2b: the literature opt-in gate. With literature intent the atoms
/// survive the prune; without it they (and their validate companions)
/// are dropped so the catch-all DAG stays lean and byte-stable — the
/// same gate the deterministic CLI `intake` path applies before lowering.
#[test]
fn generic_omics_literature_gate_keeps_when_requested_drops_otherwise() {
    use ecaa_workflow_core::composer::prune_literature_atoms_from_workflow_dag;

    let (atoms, archetypes) = workspace_config();

    // Requested → atoms survive.
    let mut kept = compose_generic_omics_bioinformatics(&atoms, &archetypes);
    let dropped_when_requested = prune_literature_atoms_from_workflow_dag(&mut kept, true);
    assert!(
        dropped_when_requested.is_empty(),
        "literature-requested prune must drop nothing; dropped {:?}",
        dropped_when_requested
    );
    let kept_ids: std::collections::BTreeSet<&str> =
        kept.nodes.iter().map(|n| n.id.as_str()).collect();
    assert!(
        kept_ids.contains("review_prior_work")
            && kept_ids.contains("contextualize_findings_with_literature"),
        "literature atoms must survive when requested; got {:?}",
        kept_ids
    );

    // Not requested → atoms + their validate companions dropped.
    let mut gated = compose_generic_omics_bioinformatics(&atoms, &archetypes);
    let dropped = prune_literature_atoms_from_workflow_dag(&mut gated, false);
    let gated_ids: std::collections::BTreeSet<&str> =
        gated.nodes.iter().map(|n| n.id.as_str()).collect();
    for lit in [
        "review_prior_work",
        "contextualize_findings_with_literature",
        "validate_review_prior_work",
        "validate_contextualize_findings_with_literature",
    ] {
        assert!(
            !gated_ids.contains(lit),
            "{lit} must be dropped when literature not requested; got {:?}",
            gated_ids
        );
        assert!(
            dropped.contains(lit),
            "{lit} must be in the dropped set; got {:?}",
            dropped
        );
    }
    // The lean catch-all terminals survive and reporting keeps a parent.
    assert!(
        gated_ids.contains("generic_summary")
            && gated_ids.contains("reporting")
            && gated_ids.contains("final_reporting"),
        "core catch-all atoms must survive the literature prune; got {:?}",
        gated_ids
    );
    // No edge dangles to a dropped literature node.
    for e in &gated.edges {
        let base = |s: &str| s.split("__").next().unwrap_or(s).to_string();
        assert!(
            !dropped.contains(&base(&e.from_node)) && !dropped.contains(&base(&e.to_node)),
            "edge {} -> {} references a dropped literature node",
            e.from_node,
            e.to_node
        );
    }
}
