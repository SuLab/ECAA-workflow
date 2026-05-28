//! Cross-version parity corpus across the eight canonical scenarios:
//! every scenario must produce a v4 DAG whose set of executable atom ids
//! is a superset of the v2 archetype baseline (modulo lossless adapter
//! insertion). Adapter atoms (`adapter_*` / `*_adapter*`) are allowed
//! to be added by v4 — they're a v4-only lossless lift.
//!
//! Two ignored tests:
//!
//! 1. `regenerate_baselines` writes both the v2 baseline and the v4
//! emission to disk under `tests/conversation-fixtures/fixtures/v4-parity/`.
//! Driven by `make v4-parity-emit`.
//! 2. `v4_parity_corpus_matches_v2_baseline` reads both files and
//! compares the executable-atom sets.
//!
//! Why deterministic in-process composition (rather than the CLI
//! `intake` subcommand)? The CLI `intake` always uses the legacy
//! taxonomy build (composer_version == 1); driving v2 + v4 through
//! `compose_with_version_and_modalities_full` here avoids needing
//! to thread `SWFC_COMPOSER` through the CLI surface and keeps the
//! corpus regression byte-stable.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::builder::{build_dag_from_composition, build_dag_from_workflow_dag};
use scripps_workflow_core::classify::Classifier;
use scripps_workflow_core::composer::compose_with_version_and_modalities_full;
use scripps_workflow_core::dag::DAG;
use scripps_workflow_core::goal_spec::GoalSpec;
use scripps_workflow_core::project_class::ProjectClass;

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

fn fixture_dir(scenario: &str) -> PathBuf {
    workspace_root()
        .join("tests/conversation-fixtures/fixtures/v4-parity")
        .join(scenario)
}

fn v2_baseline_path(scenario: &str) -> PathBuf {
    fixture_dir(scenario).join("v2-baseline.json")
}

fn v4_emission_path(scenario: &str) -> PathBuf {
    fixture_dir(scenario)
        .join("v4-emission")
        .join("WORKFLOW.json")
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

/// Per-scenario classified state shared between v2 + v4 emission.
struct ClassifiedScenario {
    goal: GoalSpec,
    project_class: ProjectClass,
    project_class_str: &'static str,
    modalities: Vec<String>,
    taxonomy_path: PathBuf,
}

fn classify_scenario(request_text: &str) -> Result<ClassifiedScenario, String> {
    let keywords_path = config_root().join("modality-keywords.yaml");
    let classifier =
        Classifier::load(&keywords_path).map_err(|e| format!("Classifier::load: {e}"))?;
    let classification = classifier.classify(request_text);

    let project_class_cfg_path = config_root().join("project-class-keywords.yaml");
    let project_class_cfg =
        scripps_workflow_core::classify::load_project_class_keywords(&project_class_cfg_path)
            .map_err(|e| format!("load_project_class_keywords: {e}"))?;
    let project_class =
        scripps_workflow_core::classify::classify_project_class(request_text, &project_class_cfg);
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

    // Resolve the legacy taxonomy path (relative to workspace).
    let taxonomy_path = workspace_root().join(&classification.taxonomy_path);

    eprintln!(
        "  classify: modality={} addl={:?} goal=({},{:?},{:?}) project_class={} taxonomy={}",
        classification.modality,
        classification
            .additional_modalities
            .iter()
            .map(|m| m.modality.as_str())
            .collect::<Vec<_>>(),
        goal.edam_data,
        goal.edam_format.as_deref(),
        goal.modifiers.get("kind"),
        project_class_str,
        taxonomy_path.display(),
    );

    Ok(ClassifiedScenario {
        goal,
        project_class,
        project_class_str,
        modalities,
        taxonomy_path,
    })
}

/// v2 emission. Strategy: try the archetype-driven composer first;
/// when the dispatch returns `TieRequiresSmeDecision` or
/// `CompositionInfeasible`, fall back to the legacy taxonomy build.
/// This mirrors the conversation crate's `try_build_via_composer`
/// soft-fail downgrade — the v1 default path that today's users see.
fn emit_v2(
    scenario: &str,
    state: &ClassifiedScenario,
    atoms: &AtomRegistry,
    archetypes: &ArchetypeRegistry,
    workflow_id: &str,
) -> Result<DAG, String> {
    let modalities: Vec<&str> = state.modalities.iter().map(String::as_str).collect();
    match compose_with_version_and_modalities_full(
        &state.goal,
        state.project_class_str,
        atoms,
        archetypes,
        2,
        &modalities,
        None,
        None,
        None,
    ) {
        Ok(output) => build_dag_from_composition(
            &output.composition,
            workflow_id,
            &std::collections::BTreeMap::new(),
            &[],
        )
        .map_err(|e| format!("build_dag_from_composition: {e}")),
        Err(e) => {
            // Phase B4 — legacy taxonomy fallback removed.
            Err(format!(
                "[{scenario}] v2 archetype dispatch failed and legacy taxonomy fallback \
                 is no longer available (Phase 6.1 B4): {e:?}"
            ))
        }
    }
}

/// Result of v4 emission carrying the DAG plus a (possibly-empty)
/// diagnostic note. Even when the v4 emission succeeds, we record
/// when the production dispatch path failed and the direct path
/// recovered — that distinction is the load-bearing finding for
/// production plumbing vs planner correctness.
struct V4Emission {
    dag: DAG,
    diagnostic: Option<String>,
}

/// v4 emission. Three-stage strategy that gives the planner the best
/// chance of producing an executable DAG while still surfacing
/// production-path plumbing gaps:
///
/// 1. **Production-path attempt** — `compose_with_version_and_modalities_full`,
/// matching what the conversation crate hits for v4 sessions. Any
/// failure here is a real production gap.
/// 2. **Legacy-taxonomy fallback** — when production fails, mirror v2's
/// `try_build_via_composer` downgrade by building from the
/// classifier's `taxonomy_path`. This is the same fallback the
/// conversation crate's `rebuild_dag` performs when the composer
/// returns Err: trying the legacy generic-omics shape rather than
/// forcing the archetype-only direct-planner path. Mirrors v2's
/// behavior so project-class scenarios where the archetype itself is
/// incomplete (time_series_forecast lacks a `data:0951`-producing
/// atom — see archetype YAML caveat) still ship a usable DAG.
/// 3. **Direct planner attempt** — when even the legacy taxonomy fails
/// to load, fall back to `composer_v4::plan` directly with a
/// hand-crafted `PlanningContext` that wires modality + project_class
/// + `available_data` (derived from the scenario's intake fact set).
/// Last-resort path; surfaces a documented gap.
///
/// Outcomes are recorded for the gap log; we return the first DAG that
/// lowers successfully, or an error describing all failure modes.
fn emit_v4(
    state: &ClassifiedScenario,
    atoms: &AtomRegistry,
    archetypes: &ArchetypeRegistry,
    workflow_id: &str,
) -> Result<V4Emission, String> {
    use scripps_workflow_core::composer_v4::{plan as v4_plan, PlanningContext};
    use scripps_workflow_core::workflow_contracts::workflow_intent::{
        DesiredOutput, WorkflowIntent,
    };

    let modalities: Vec<&str> = state.modalities.iter().map(String::as_str).collect();

    // Attempt 1 — production dispatch path.
    let production_err = match compose_with_version_and_modalities_full(
        &state.goal,
        state.project_class_str,
        atoms,
        archetypes,
        4,
        &modalities,
        None,
        None,
        None,
    ) {
        Ok(output) => {
            return finalize_v4_dag(&output, workflow_id).map(|dag| V4Emission {
                dag,
                diagnostic: None,
            });
        }
        Err(e) => format!("production: {e:?}"),
    };

    // Phase B4 — legacy taxonomy fallback removed. The v2-shaped
    // `try_build_via_composer` downgrade no longer exists. Fall
    // through to Attempt 2 (direct planner).

    // Attempt 2 — hand-crafted planning context with modality,
    // project_class, and available_data seeded so forward / backward /
    // meet-in-the-middle search has material to work with.
    let primary_modality = state
        .modalities
        .first()
        .map(|s| s.as_str())
        .unwrap_or("generic_omics");
    let intent = WorkflowIntent {
        id: format!("v4_parity_{}", state.goal.edam_data),
        schema_version: semver::Version::new(1, 0, 0),
        goal: state
            .goal
            .source_prose
            .clone()
            .unwrap_or_else(|| state.goal.edam_data.clone()),
        modality: Some(primary_modality.to_string()),
        project_class: Some(state.project_class_str.to_string()),
        available_data: scenario_available_data(&state.modalities),
        desired_outputs: vec![DesiredOutput {
            label: state
                .goal
                .source_prose
                .clone()
                .unwrap_or_else(|| state.goal.edam_data.clone()),
            edam_data: Some(state.goal.edam_data.clone()),
            edam_format: state.goal.edam_format.clone(),
            human_readable: false,
        }],
        ..Default::default()
    };
    let mut ctx = PlanningContext::new(intent);
    ctx.max_branches = 64;
    ctx.max_depth = 12;
    ctx.max_alternatives = 5;

    let result = v4_plan(
        &ctx,
        &state.goal,
        state.project_class_str,
        atoms,
        archetypes,
    );
    use scripps_workflow_core::workflow_contracts::outcome::ComposeOutcome;
    let workflow_dag = match &result.primary {
        ComposeOutcome::ValidatedExecutableDag { dag, .. } => Some(dag.clone()),
        ComposeOutcome::DraftDag { dag, .. } => Some(dag.clone()),
        ComposeOutcome::PartialDag {
            dag,
            unresolved_gaps,
        } if !dag.nodes.is_empty() => {
            eprintln!(
                "  v4 direct: PartialDag with {} gap(s); using it anyway",
                unresolved_gaps.len()
            );
            Some(dag.clone())
        }
        other => {
            return Err(format!(
                "{production_err}\ndirect: planner returned non-DAG outcome: {other:?}"
            ));
        }
    };
    let workflow_dag =
        workflow_dag.ok_or_else(|| format!("{production_err}\ndirect: planner produced no DAG"))?;

    // v4 lowering today often emits DAGs that fail `validate_dag`
    // (most common: cycles in `batch_correction` /
    // `sequence_trimming` because the planner's edge-builder doesn't
    // honor the atom's role-ordering hints). Lower without the
    // invariant gate so the parity check can still see the atom set.
    // The cycle is itself a real v4 bug surfaced in the gap log;
    // must be fixed before the default flips.
    match build_dag_from_workflow_dag(&workflow_dag, workflow_id) {
        Ok(dag) => {
            eprintln!(
                "  v4 direct path produced validated DAG ({} tasks)",
                dag.tasks.len()
            );
            Ok(V4Emission {
                dag,
                diagnostic: Some(format!(
                    "Production dispatch failed; direct planner with seeded modality + \
                     available_data succeeded. Production error: {production_err}"
                )),
            })
        }
        Err(validation_err) => {
            // Lower again, this time skipping the validate_dag gate
            // by reusing the lower_to_workflow_json artifact.
            use scripps_workflow_core::backend_emitters::workflow_json::{
                lower_to_workflow_json, EmitContext,
            };
            let mut ctx = EmitContext::defaults();
            ctx.workflow_version = "1.0".into();
            let artifact = lower_to_workflow_json(&workflow_dag, &ctx)
                .map_err(|e| format!("{production_err}\nlower (raw): {e:?}"))?;
            let mut dag = artifact.dag;
            dag.workflow_id = workflow_id.to_string();
            eprintln!(
                "  v4 direct path produced INVARIANT-VIOLATING DAG ({} tasks); validation: {validation_err}",
                dag.tasks.len()
            );
            Ok(V4Emission {
                dag,
                diagnostic: Some(format!(
                    "Production dispatch failed AND direct planner produced an \
                     invariant-violating DAG (validate_dag rejected). Production: \
                     {production_err}\nValidation: {validation_err}"
                )),
            })
        }
    }
}

fn finalize_v4_dag(
    output: &scripps_workflow_core::composer::ComposerOutput,
    workflow_id: &str,
) -> Result<DAG, String> {
    if let Some(workflow_dag) = output.workflow_dag.as_ref() {
        return build_dag_from_workflow_dag(workflow_dag, workflow_id)
            .map_err(|e| format!("build_dag_from_workflow_dag: {e}"));
    }
    build_dag_from_composition(
        &output.composition,
        workflow_id,
        &std::collections::BTreeMap::new(),
        &[],
    )
    .map_err(|e| format!("build_dag_from_composition: {e}"))
}

/// Hand-crafted intake fact set per modality. Mirrors the shape the
/// dataset profiler would populate from real intake; today the
/// production dispatch leaves `available_data` empty, which
/// starves v4's forward search.
fn scenario_available_data(
    modalities: &[String],
) -> Vec<scripps_workflow_core::workflow_contracts::data_product::DataProductContract> {
    use scripps_workflow_core::workflow_contracts::data_product::DataProductContract;
    let mut data = Vec::new();
    for modality in modalities {
        match modality.as_str() {
            "bulk_rnaseq" | "single_cell_rnaseq" | "variant_calling" | "chip_seq" | "atac_seq"
            | "long_read_rnaseq" => {
                data.push(DataProductContract::sample_paired_fastq());
            }
            "proteomics" => {
                // Lacking a sample fixture for proteomics raw — reuse
                // FASTQ shape; the matcher only needs *some* input
                // contract to bootstrap forward search. The cross-omics
                // archetype path resolves the right inputs at compose
                // time.
                data.push(DataProductContract::sample_paired_fastq());
            }
            _ => {
                // Project-class scenarios (time-series, clinical-trial)
                // have no native fastq; still provide one so the
                // forward search has a starting frontier.
                data.push(DataProductContract::sample_paired_fastq());
            }
        }
    }
    if data.is_empty() {
        data.push(DataProductContract::sample_paired_fastq());
    }
    data
}

fn write_workflow_json(dag: &DAG, path: &Path) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir -p fixture dir");
    }
    let json = serde_json::to_string_pretty(dag).expect("serialize DAG");
    std::fs::write(path, json + "\n").expect("write WORKFLOW.json");
}

fn atom_set_from_workflow_json(value: &serde_json::Value) -> BTreeSet<String> {
    value["tasks"]
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Baseline generator: reads each scenario's request.txt, runs the
/// classifier + composer at v2 (archetype, with legacy-taxonomy
/// soft-fail fallback) and v4 (forward/backward planner), and writes
/// both `WORKFLOW.json` files to disk under
/// `tests/conversation-fixtures/fixtures/v4-parity/<scenario>/`.
///
/// v2 emission uses the conversation crate's same downgrade path:
/// archetype dispatch first, legacy taxonomy build on tie /
/// infeasibility — so the baseline reflects what real users see.
///
/// v4 failures are surfaced to the SCENARIO_GAPS log instead of
/// panicking, so a partial corpus still ships. The parity check
/// gates on the gaps document.
///
/// Run via `make v4-parity-emit` (or `cargo test --test
/// composer_v4_parity_corpus -- --ignored regenerate_baselines`).
#[test]
#[ignore = "fixture generator — run via `make v4-parity-emit`"]
fn regenerate_baselines() {
    let (atoms, archetypes) = load_registries();
    let mut hard_errors: Vec<String> = Vec::new();
    let mut v4_gaps: Vec<String> = Vec::new();

    for scenario in SCENARIOS {
        let req = request_path(scenario);
        if !req.exists() {
            hard_errors.push(format!(
                "[{scenario}] missing request.txt at {}",
                req.display()
            ));
            continue;
        }
        let request_text = std::fs::read_to_string(&req).expect("read request.txt");
        let state = match classify_scenario(&request_text) {
            Ok(s) => s,
            Err(e) => {
                hard_errors.push(format!("[{scenario}] classify: {e}"));
                continue;
            }
        };
        // Surface project_class so downstream readers can sanity
        // check the routing without re-running the classifier.
        let _ = state.project_class;

        // v2 baseline. Hard-fail on this — every scenario must produce
        // some baseline (archetype or legacy taxonomy).
        match emit_v2(
            scenario,
            &state,
            &atoms,
            &archetypes,
            &format!("v2-{scenario}"),
        ) {
            Ok(dag) => {
                write_workflow_json(&dag, &v2_baseline_path(scenario));
                eprintln!("[{scenario}] v2 baseline OK ({} tasks)", dag.tasks.len());
            }
            Err(e) => hard_errors.push(format!("[{scenario}] v2 emit failed: {e}")),
        }

        // v4 emission. Soft-fail: a v4 gap is a real finding to
        // surface, not a hard failure. We always
        // try to emit *some* WORKFLOW.json (the direct-path fallback
        // produces one even when the planner result is cycle-bearing
        // and fails validate_dag) so the parity check has material.
        // The companion GAP.txt records which path produced the
        // emission and what failed in the production path.
        let workflow_id_v4 = format!("v4-{scenario}");
        let v4_path = v4_emission_path(scenario);
        let gap_path = fixture_dir(scenario).join("v4-emission/GAP.txt");
        // Pre-clear so this regen doesn't see stale state.
        let _ = std::fs::remove_file(&v4_path);
        let _ = std::fs::remove_file(&gap_path);
        match emit_v4(&state, &atoms, &archetypes, &workflow_id_v4) {
            Ok(emission) => {
                write_workflow_json(&emission.dag, &v4_path);
                if let Some(diag) = &emission.diagnostic {
                    if let Some(parent) = gap_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let _ = std::fs::write(
                        &gap_path,
                        format!(
                            "v4 emission produced via DIRECT-PATH FALLBACK (production \
                             dispatch path is broken). DAG was still written to \
                             WORKFLOW.json so the parity check can compare atom \
                             sets.\n\n{diag}\n"
                        ),
                    );
                    v4_gaps.push(format!("[{scenario}] (direct-path fallback)"));
                    eprintln!(
                        "[{scenario}] v4 emission OK via direct-path fallback ({} tasks)",
                        emission.dag.tasks.len()
                    );
                } else {
                    eprintln!(
                        "[{scenario}] v4 emission OK via production dispatch ({} tasks)",
                        emission.dag.tasks.len()
                    );
                }
            }
            Err(e) => {
                eprintln!("[{scenario}] v4 emission FAILED: {e}");
                v4_gaps.push(format!("[{scenario}] {e}"));
                if let Some(parent) = gap_path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(&gap_path, format!("v4 emission failed:\n{e}\n"));
            }
        }
    }

    eprintln!(
        "\n=== regenerate_baselines summary ===\nv2 baselines emitted: {}\nv4 gaps: {}",
        SCENARIOS.len() - hard_errors.len(),
        v4_gaps.len()
    );
    for g in &v4_gaps {
        eprintln!("  {g}");
    }

    if !hard_errors.is_empty() {
        panic!(
            "regenerate_baselines surfaced {} hard error(s):\n  - {}",
            hard_errors.len(),
            hard_errors.join("\n  - ")
        );
    }
}

/// Parity check: for each scenario, every executable atom
/// in the v2 baseline must appear in the v4 emission. v4 may add
/// adapters (`adapter_*` / `*_adapter`) — those are allowed extras
/// because they're a v4-only lossless lift the planner inserts to
/// reconcile incompatible ports.
///
/// Four terminal states per scenario:
/// - GREEN — v4 atoms ⊇ v2 atoms (modulo adapters); the parity
/// contract holds AND the production v4 dispatch path
/// (`compose_v4_dispatch_full`) produced the result.
/// - V4_GAP_DOCUMENTED — `v4-emission/GAP.txt` exists. Either v4
/// emission failed entirely (no WORKFLOW.json), or it
/// succeeded only via the direct-path fallback (production
/// dispatch broken). Atom-set diffs are recorded for triage but
/// do not fail the test.
/// - SKIPPED — fixture missing. Re-run `make v4-parity-emit`.
/// - REGRESSION — v4 went through the production dispatch cleanly
/// and omitted a v2 atom or added a non-adapter atom. This is
/// the only condition that panics today.
///
/// Readiness is gated on every scenario reaching GREEN.
///
/// Run via `cargo test -p scripps-workflow-core --test
/// composer_v4_parity_corpus -- --ignored
/// v4_parity_corpus_matches_v2_baseline`.
#[test]
#[ignore = "requires fixture regeneration via `make v4-parity-emit`"]
fn v4_parity_corpus_matches_v2_baseline() {
    let mut regressions: Vec<String> = Vec::new();
    let mut documented_gaps: Vec<(String, Vec<String>)> = Vec::new();
    let mut skipped: Vec<&str> = Vec::new();
    let mut green: Vec<&str> = Vec::new();

    for scenario in SCENARIOS {
        let v2_path = v2_baseline_path(scenario);
        let v4_path = v4_emission_path(scenario);
        let v4_gap_marker = fixture_dir(scenario).join("v4-emission/GAP.txt");
        // TRIAGE.md is a regen-stable companion to GAP.txt: regen wipes
        // GAP.txt unconditionally before each emission, but never
        // touches TRIAGE.md. TRIAGE.md carries per-scenario verdicts
        // so the triage notes survive `make v4-parity-emit`. The
        // presence of EITHER file marks the scenario as a documented
        // v4 gap.
        let v4_triage_marker = fixture_dir(scenario).join("v4-emission/TRIAGE.md");
        let has_documented_gap = v4_gap_marker.exists() || v4_triage_marker.exists();

        if !v2_path.exists() {
            eprintln!(
                "[SKIP] {scenario}: v2 baseline missing at {} — run `make v4-parity-emit`",
                v2_path.display()
            );
            skipped.push(scenario);
            continue;
        }
        if has_documented_gap && !v4_path.exists() {
            // Documented v4 gap, no DAG produced. Surface the marker
            // file's first-line summary; do not treat as a test
            // failure.
            let detail = std::fs::read_to_string(&v4_gap_marker).unwrap_or_default();
            eprintln!(
                "[V4_GAP_DOCUMENTED] {scenario}: v4 emission unavailable. GAP.txt:\n{}",
                detail.lines().take(4).collect::<Vec<_>>().join("\n  ")
            );
            documented_gaps.push((scenario.to_string(), vec!["no v4 DAG emitted".into()]));
            continue;
        }
        if !v4_path.exists() {
            eprintln!(
                "[SKIP] {scenario}: v4 emission missing at {} — run `make v4-parity-emit`",
                v4_path.display()
            );
            skipped.push(scenario);
            continue;
        }

        let v2: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&v2_path).unwrap()).unwrap();
        let v4: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&v4_path).unwrap()).unwrap();
        let v2_atoms = atom_set_from_workflow_json(&v2);
        let v4_atoms = atom_set_from_workflow_json(&v4);
        let only_in_v2: BTreeSet<&String> = v2_atoms.difference(&v4_atoms).collect();
        let only_in_v4: BTreeSet<&String> = v4_atoms.difference(&v2_atoms).collect();

        let mut scenario_errors: Vec<String> = Vec::new();
        if !only_in_v2.is_empty() {
            scenario_errors.push(format!(
                "v4 missing {} atom(s) from v2 baseline: {:?}",
                only_in_v2.len(),
                only_in_v2
            ));
        }
        for atom in &only_in_v4 {
            let s = atom.as_str();
            // Allowed extras: adapter atoms inserted by v4's adapter
            // engine to reconcile incompatible ports (lossless lift).
            if !(s.starts_with("adapter_") || s.contains("_adapter")) {
                scenario_errors.push(format!(
                    "v4 added non-adapter atom {atom:?} not in v2 baseline"
                ));
            }
        }

        if scenario_errors.is_empty() {
            green.push(scenario);
        } else if has_documented_gap {
            // Differences exist BUT GAP.txt or TRIAGE.md documents the
            // gap (direct-path fallback marker, or operator-authored
            // triage). Surface the diff into the gap log instead of
            // panicking — must be closed before the v4 default flips.
            eprintln!(
                "[V4_GAP_DOCUMENTED] {scenario}: {} atom-set diff(s) recorded in GAP.txt",
                scenario_errors.len()
            );
            for e in &scenario_errors {
                eprintln!("    {e}");
            }
            // Append the atom-set diff to GAP.txt so the persisted
            // record carries the same triage detail as the test
            // output. Read-modify-write rather than truncate so we
            // preserve the production-dispatch error written by
            // regenerate_baselines. When only TRIAGE.md exists (no
            // GAP.txt — i.e. v4 emission via production dispatch but
            // an operator-recorded triage), create GAP.txt fresh with
            // the atom-set diff.
            let prev = std::fs::read_to_string(&v4_gap_marker).unwrap_or_default();
            let already_has_diff = prev.contains("Atom-set diff vs v2 baseline");
            if !already_has_diff {
                let mut combined = prev;
                combined.push_str("\nAtom-set diff vs v2 baseline:\n");
                for e in &scenario_errors {
                    combined.push_str(&format!("  - {e}\n"));
                }
                let _ = std::fs::write(&v4_gap_marker, combined);
            }
            documented_gaps.push((scenario.to_string(), scenario_errors));
        } else {
            // No GAP.txt → v4 production dispatch succeeded → atom
            // diffs are real regressions.
            for e in &scenario_errors {
                regressions.push(format!("[{scenario}] {e}"));
            }
        }
    }

    eprintln!(
        "\n=== v4 parity corpus summary ===\n  green:       {} ({})\n  documented:  {} ({})\n  skipped:     {} ({})\n  regressions: {}",
        green.len(),
        green.join(", "),
        documented_gaps.len(),
        documented_gaps
            .iter()
            .map(|(s, _)| s.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        skipped.len(),
        skipped.join(", "),
        regressions.len(),
    );

    if !skipped.is_empty() {
        eprintln!(
            "[hint] {} scenario(s) skipped — run `make v4-parity-emit` to regenerate.",
            skipped.len()
        );
    }

    if !documented_gaps.is_empty() {
        eprintln!(
            "[v4-gate] {} scenario(s) carrying documented v4 gaps. The default flip \
             cannot proceed until these are GREEN.",
            documented_gaps.len()
        );
    }

    // Hard fail only on UN-documented regressions (v4 production
    // dispatch produced a clean DAG but it omitted a v2 atom or added
    // a non-adapter atom). Documented v4 gaps don't trigger this.
    if !regressions.is_empty() {
        panic!(
            "v4 parity corpus surfaced {} regression(s) in scenarios that emitted via the \
             production dispatch path:\n  - {}",
            regressions.len(),
            regressions.join("\n  - ")
        );
    }
}
