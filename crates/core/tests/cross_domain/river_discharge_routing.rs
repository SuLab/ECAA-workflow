//! Cross-domain proof (CD1): a non-bioinformatics intake routes to the new
//! modality and composes a valid DAG through the SAME classifier + composer,
//! with zero engine-logic edits. Falsifies "schema-gating only works because
//! bioinformatics is special."

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::builder::build_dag_from_workflow_dag;
use ecaa_workflow_core::classify::Classifier;
use ecaa_workflow_core::composer::compose_with_modalities_full;
use ecaa_workflow_core::goal_spec::GoalSpec;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn config_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

fn load_classifier() -> Classifier {
    Classifier::load(&config_root().join("modality-keywords.yaml")).expect("load classifier")
}

fn workspace_config() -> (AtomRegistry, ArchetypeRegistry) {
    let config = config_root();
    let atoms = AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atoms");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config.join("archetypes")).expect("load archetypes");
    (atoms, archetypes)
}

#[test]
fn hydrology_intake_routes_to_river_discharge_forecast() {
    let clf = load_classifier();
    // Hydrology-heavy prose: river discharge / streamflow / hydrology /
    // gauge station / catchment / runoff — all river_discharge_forecast
    // keywords, none shared with a bio modality.
    let intake = "Forecast river discharge (streamflow) for the gauge station at \
                  catchment X. This is a hydrology / rainfall-runoff problem: \
                  predict watershed runoff with prediction intervals over a \
                  12-month horizon.";
    let r = clf.classify(intake);
    assert_eq!(
        r.modality, "river_discharge_forecast",
        "hydrology intake misrouted: modality={} tie_candidates={:?}",
        r.modality, r.tie_candidates
    );
}

#[test]
fn river_discharge_composes_executable_dag_via_same_planner() {
    let (atoms, archetypes) = workspace_config();
    // Drive the v4 planner the same way the bio time-series archetype test
    // does: the hydrology archetype shares goal_data:0951 / format:3475.
    // The cross-domain proof is that a NON-bio domain composes + lowers to
    // an executable DAG through the SAME composer + builder — zero engine
    // edits — falsifying "schema-gating only works for bioinformatics".
    let mut modifiers = BTreeMap::new();
    modifiers.insert("kind".into(), "forecast".into());
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers,
        source_prose: Some("river discharge forecast".into()),
        confidence: 0.5,
    };
    let result = compose_with_modalities_full(
        &goal,
        "time_series_forecast",
        &atoms,
        &archetypes,
        &["river_discharge_forecast"],
        None,
        None,
        None,
    )
    .expect("dispatch should succeed for river_discharge_forecast");
    let composition = &result.composition;
    assert!(
        !composition.atoms.is_empty(),
        "river_discharge_forecast must emit a non-empty composition"
    );
    let stage_ids: std::collections::BTreeSet<&str> = composition
        .atoms
        .iter()
        .map(|c| c.stage_id.as_str())
        .collect();
    // The goal-producer forecast-evaluation stage is reached.
    assert!(
        stage_ids
            .iter()
            .any(|id| id.contains("time_series_forecast_evaluate")),
        "composition must reach the forecast-evaluation goal producer; got {stage_ids:?}"
    );
    // Sequential pipeline → at least one inter-atom dependency edge.
    let total_deps: usize = composition.atoms.iter().map(|c| c.depends_on.len()).sum();
    assert!(total_deps > 0, "DAG must declare at least one dependency edge");

    // The composed WorkflowDag lowers to an executable, acyclic task DAG
    // through the SAME builder bio archetypes use.
    let wf = result
        .workflow_dag
        .as_ref()
        .expect("v4 result must include a workflow DAG");
    let dag = build_dag_from_workflow_dag(wf, "wf-river-discharge")
        .expect("hydrology workflow DAG must lower to executable tasks");
    assert!(
        !dag.tasks.is_empty(),
        "lowered hydrology DAG must carry executable tasks"
    );
}

#[test]
fn bio_corpus_modalities_unchanged() {
    use ecaa_workflow_core::strata::StrataRegistry;
    let reg = StrataRegistry::from_embedded();
    // The bio strata still resolve their canonical modalities.
    assert_eq!(reg.stratum_for("bulk_rnaseq"), Some("transcriptomics"));
    assert_eq!(reg.stratum_for("variant_calling"), Some("genomics"));
    // The new domain is additive, not a replacement.
    assert_eq!(
        reg.stratum_for("river_discharge_forecast"),
        Some("physical_sciences")
    );
}
