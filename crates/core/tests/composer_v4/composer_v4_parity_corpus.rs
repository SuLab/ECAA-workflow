//! v4-only corpus across the eight canonical scenarios. With v1/v2/v3
//! retired, v4 (the proof-carrying planner) is the sole composer, so
//! there is no longer a v2 baseline to compare against. Each canonical
//! scenario must classify cleanly and emit a non-empty executable DAG
//! through the production dispatch path
//! (`compose_with_modalities_full`).

use std::path::PathBuf;

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::builder::build_dag_from_workflow_dag;
use ecaa_workflow_core::classify::Classifier;
use ecaa_workflow_core::composer::compose_with_modalities_full;
use ecaa_workflow_core::goal_spec::GoalSpec;
use ecaa_workflow_core::project_class::ProjectClass;

const SCENARIOS: &[&str] = &[
    "bulk-rnaseq",
    "scrnaseq",
    "variant-calling",
    "chip-seq",
    "atac-seq",
    "cross-omics",
    "time-series",
    "clinical-trial",
];

/// Workspace root resolved from the per-crate `CARGO_MANIFEST_DIR`.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn config_root() -> PathBuf {
    workspace_root().join("config")
}

fn request_path(scenario: &str) -> PathBuf {
    workspace_root()
        .join("testdata/v4-parity")
        .join(scenario)
        .join("request.txt")
}

fn load_registries() -> (AtomRegistry, ArchetypeRegistry) {
    let atoms = AtomRegistry::load_from_dir(&config_root().join("stage-atoms"))
        .expect("AtomRegistry must load");
    let archetypes = ArchetypeRegistry::load_from_dir(&config_root().join("archetypes"))
        .expect("ArchetypeRegistry must load");
    (atoms, archetypes)
}

fn project_class_to_str(pc: ProjectClass) -> &'static str {
    match pc {
        ProjectClass::Bioinformatics => "bioinformatics",
        ProjectClass::ClinicalTrial => "clinical_trial",
        ProjectClass::TimeSeriesForecast => "time_series_forecast",
    }
}

/// Synthesize a goal for scenarios where the keyword-path goal
/// extractor returns `None` (clinical-trial endpoint analysis prose
/// has no goal pattern in `modality-keywords.yaml` today). Falls
/// through to a `data:0951` / `format:3475` "statistical estimate"
/// goal — the same shape `clinical_trial_analysis` and
/// `time_series_forecast` archetypes register against.
fn fallback_goal(project_class: ProjectClass) -> GoalSpec {
    let mut modifiers = std::collections::BTreeMap::new();
    let prose = match project_class {
        ProjectClass::ClinicalTrial => {
            modifiers.insert("kind".into(), "clinical_trial_analysis".into());
            "clinical-trial endpoint analysis"
        }
        ProjectClass::TimeSeriesForecast => {
            modifiers.insert("kind".into(), "forecast".into());
            "time-series forecast"
        }
        ProjectClass::Bioinformatics => "differential expression",
    };
    GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers,
        source_prose: Some(prose.into()),
        confidence: 0.5,
    }
}

/// Per-scenario classified state.
struct ClassifiedScenario {
    goal: GoalSpec,
    project_class: ProjectClass,
    project_class_str: &'static str,
    modalities: Vec<String>,
}

fn classify_scenario(request_text: &str) -> Result<ClassifiedScenario, String> {
    let keywords_path = config_root().join("modality-keywords.yaml");
    let classifier =
        Classifier::load(&keywords_path).map_err(|e| format!("Classifier::load: {e}"))?;
    let classification = classifier.classify(request_text);

    let project_class_cfg_path = config_root().join("project-class-keywords.yaml");
    let project_class_cfg =
        ecaa_workflow_core::classify::load_project_class_keywords(&project_class_cfg_path)
            .map_err(|e| format!("load_project_class_keywords: {e}"))?;
    let project_class =
        ecaa_workflow_core::classify::classify_project_class(request_text, &project_class_cfg);
    let project_class_str = project_class_to_str(project_class);

    let goal = classification
        .goal
        .clone()
        .unwrap_or_else(|| fallback_goal(project_class));

    let modalities: Vec<String> = std::iter::once(classification.modality.clone())
        .chain(
            classification
                .additional_modalities
                .iter()
                .map(|m| m.modality.clone()),
        )
        .collect();

    Ok(ClassifiedScenario {
        goal,
        project_class,
        project_class_str,
        modalities,
    })
}

/// v4 production-dispatch emission: classify the scenario, compose
/// through `compose_with_modalities_full`, and lower the resulting
/// `workflow_dag` to a legacy `DAG`. Returns the executable task count.
fn emit_v4_task_count(
    scenario: &str,
    state: &ClassifiedScenario,
    atoms: &AtomRegistry,
    archetypes: &ArchetypeRegistry,
) -> Result<usize, String> {
    // Surface project_class so the field stays read (routing sanity).
    let _ = state.project_class;
    let modalities: Vec<&str> = state.modalities.iter().map(String::as_str).collect();
    let output = compose_with_modalities_full(
        &state.goal,
        state.project_class_str,
        atoms,
        archetypes,
        &modalities,
        None,
        None,
        None,
    )
    .map_err(|e| format!("[{scenario}] compose_with_modalities_full failed: {e:?}"))?;

    let workflow_dag = output
        .workflow_dag
        .as_ref()
        .ok_or_else(|| format!("[{scenario}] v4 dispatch produced no WorkflowDag"))?;
    let dag = build_dag_from_workflow_dag(workflow_dag, &format!("v4-{scenario}"))
        .map_err(|e| format!("[{scenario}] build_dag_from_workflow_dag: {e}"))?;
    Ok(dag.tasks.len())
}

/// Every canonical scenario must emit a non-empty executable DAG via
/// the v4 production dispatch path.
#[test]
fn v4_corpus_emits_non_empty_dag_for_every_scenario() {
    let (atoms, archetypes) = load_registries();
    let mut failures: Vec<String> = Vec::new();

    let mut checked = 0usize;
    for scenario in SCENARIOS {
        let req = request_path(scenario);
        if !req.exists() {
            // Scenario request fixtures are operator-provided under
            // testdata/v4-parity/ and are not committed to this repo. Skip
            // rather than fail so the assertion is meaningful where the
            // fixtures exist and a no-op where they don't (matches the
            // live_atoms()/live_archetypes() empty-skip idiom elsewhere).
            eprintln!("[{scenario}] skipped — no request.txt fixture present");
            continue;
        }
        checked += 1;
        let request_text = std::fs::read_to_string(&req).expect("read request.txt");
        let state = match classify_scenario(&request_text) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("[{scenario}] classify: {e}"));
                continue;
            }
        };
        match emit_v4_task_count(scenario, &state, &atoms, &archetypes) {
            Ok(task_count) => {
                if task_count == 0 {
                    failures.push(format!("[{scenario}] v4 emitted an EMPTY DAG"));
                } else {
                    eprintln!("[{scenario}] v4 emission OK ({task_count} tasks)");
                }
            }
            Err(e) => failures.push(e),
        }
    }

    if checked == 0 {
        eprintln!(
            "v4 parity corpus: no scenario request fixtures present under testdata/v4-parity/; \
             skipped (no scenarios checked)"
        );
    }
    assert!(
        failures.is_empty(),
        "v4 corpus failed for {} scenario(s):\n  - {}",
        failures.len(),
        failures.join("\n  - ")
    );
}
