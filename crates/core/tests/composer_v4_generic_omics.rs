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
use ecaa_workflow_core::composer::compose_with_version_and_modalities_full;
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

    let result = compose_with_version_and_modalities_full(
        &goal,
        "research",
        &atoms,
        &archetypes,
        4,
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
