//! Phase B.1 of the v3+v4 100-percent-closure plan (Task B.1).
//!
//! Before this task, the `time_series_forecast` archetype declared
//! `goal_data: data:0951` but no atom in the catalog produced it.
//! Dispatch fell back to `GoalUnreachable` (see
//! `composer_v4_project_class_archetype::time_series_does_not_use_bulk_rnaseq_archetype`).
//!
//! After this task, three new atoms — `time_series_decompose`,
//! `time_series_model_fit`, `time_series_forecast_evaluate` — close
//! the goal-producer gap and the archetype composes end-to-end via
//! the v4 planner.

use std::collections::BTreeMap;
use std::path::Path;

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::builder::build_dag_from_workflow_dag;
use ecaa_workflow_core::composer::compose_with_version_and_modalities_full;
use ecaa_workflow_core::goal_spec::GoalSpec;

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

#[test]
fn time_series_forecast_archetype_emits_executable_dag() {
    let atom_reg = AtomRegistry::load_from_dir(Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");

    let mut modifiers = BTreeMap::new();
    modifiers.insert("kind".into(), "forecast".into());
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers,
        source_prose: Some("time-series forecast".into()),
        confidence: 0.5,
    };

    let result = compose_with_version_and_modalities_full(
        &goal,
        "time_series_forecast",
        &atom_reg,
        &archetype_reg,
        4,
        &["generic_omics"],
        None,
        None,
        None,
    )
    .expect("v4 dispatch must succeed for time_series_forecast archetype");

    // The `time_series_forecast_evaluate` atom is the goal producer
    // (outputs data:0951). Its stage must appear in the composed DAG.
    let stage_ids: Vec<String> = result
        .composition
        .atoms
        .iter()
        .map(|c| c.stage_id.to_string())
        .collect();
    assert!(
        stage_ids
            .iter()
            .any(|id| id.contains("time_series_forecast_evaluate")),
        "time-series archetype must reach time_series_forecast_evaluate; \
         got stage ids: {stage_ids:?}"
    );

    // The composition must declare at least one inter-atom dependency
    // edge — the forecast pipeline is sequential
    // (decompose → model_fit → forecast_evaluate).
    let total_deps: usize = result
        .composition
        .atoms
        .iter()
        .map(|c| c.depends_on.len())
        .sum();
    assert!(
        total_deps > 0,
        "DAG must have at least one dependency edge; got atoms: {:?}",
        result
            .composition
            .atoms
            .iter()
            .map(|c| (c.stage_id.clone(), c.depends_on.clone()))
            .collect::<Vec<_>>()
    );

    let wf = result
        .workflow_dag
        .as_ref()
        .expect("v4 result must include a workflow DAG");
    let dag = build_dag_from_workflow_dag(wf, "wf-time-series")
        .expect("time-series workflow DAG must lower to executable tasks");
    let forecast_task = dag
        .tasks
        .iter()
        .find(|(task_id, _)| task_id.as_str() == "time_series_forecast_evaluate")
        .map(|(_, task)| task)
        .expect("forecast-evaluation task must be present");
    let spec = forecast_task
        .spec
        .as_ref()
        .expect("forecast-evaluation task must carry task spec");
    let figures = spec
        .get("required_figures")
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();
    assert_eq!(
        figures,
        vec!["forecast_ribbon"],
        "forecast-evaluation must require the SME-facing forecast ribbon"
    );
    assert_eq!(
        spec.get("plot_stage_id").and_then(|v| v.as_str()),
        Some("forecasting_inference"),
        "forecast-evaluation must route to the forecasting renderer"
    );
}
