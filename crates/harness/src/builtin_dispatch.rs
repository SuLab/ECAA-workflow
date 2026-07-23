//! In-process dispatch for `builtin`-tagged DAG tasks.
//!
//! Some synthesized DAG nodes carry a `spec.builtin` marker instead of
//! being agent-executed. The composer emits synthesized nodes — downstream
//! of every schema-bearing analytical stage, upstream of the reporting
//! terminals — that the harness runs deterministically IN PROCESS (no
//! agent subprocess) by calling the committed core assemblers:
//! [`assemble_report_data`] (single-artifact reporting rollup),
//! [`assemble_statistical_distribution`] (cross-method robustness rollup
//! over one terminal stage's K statistical-method variants), and
//! [`assemble_ensemble_distribution`] (cross-axis method×lens rollup over
//! the multi-analyst interpretation cells).
//!
//! Completion is recorded through the EXACT same per-task
//! `runtime/outputs/<task_id>/state.patch.json` protocol a normal agent
//! uses (see [`crate::dag_patch`]): a `completed` (or `failed`) patch
//! carrying the dispatch identity the harness stamped at pre-mark, plus the
//! sibling `result.json` a real agent always writes and a refreshed
//! `.heartbeat`. The harness's existing strict patch merge
//! ([`crate::dag_patch::apply_pending_patches_strict`]) then drives the
//! task to its terminal state, the silent-completion / required-artifact
//! guards see a normal Completed task with a `result.json`, and the
//! scheduler advances the downstream task with zero special-casing. The
//! builtin task never reaches the executor, so `Executor::run_iteration`
//! is never invoked for it.

use std::collections::BTreeMap;
use std::path::Path;

use ecaa_workflow_core::clock::Clock;
use ecaa_workflow_core::dag::{Task, TaskState};
use ecaa_workflow_core::reexecution_bounds::ModalityBounds;
use ecaa_workflow_core::report_contract::ensemble_assemble::{
    ENSEMBLE_DISTRIBUTION_STAGE_ID, STAT_DISTRIBUTION_STAGE_ID,
};
use ecaa_workflow_core::report_contract::{
    assemble_ensemble_distribution, assemble_report_data, assemble_statistical_distribution,
    ResultSchema,
};

use crate::dag_patch::{state_patch_schema_version, PickedDispatch, StatePatch};

/// Value of a task's `spec.builtin` attribute marking the report-data
/// assembler builtin (stamped by the composer's report-data synthesis
/// pass; surfaced into the lowered task spec by the workflow_json emitter).
/// No core const backs this one today — kept as a harness-local literal.
pub const ASSEMBLE_REPORT_DATA: &str = "assemble_report_data";

/// Value of a task's `spec.builtin` attribute marking the cross-method
/// statistical-distribution aggregator builtin (one per terminal
/// analytical stage that was multi-analyst-fanned across statistical
/// methods).
///
/// Re-exported from core's `report_contract::ensemble_assemble::STAT_DISTRIBUTION_STAGE_ID`
/// rather than redeclared as an independent literal: that is the single
/// source of truth the composer stamps into `spec.builtin`, so a core
/// rename can never silently desync this match value (see
/// `harness_builtin_ids_are_core_stage_ids` below).
pub const ASSEMBLE_STATISTICAL_DISTRIBUTION: &str = STAT_DISTRIBUTION_STAGE_ID;

/// Value of a task's `spec.builtin` attribute marking the cross-axis
/// (method × interpretive-lens) ensemble-distribution aggregator builtin.
///
/// Re-exported from core's `report_contract::ensemble_assemble::ENSEMBLE_DISTRIBUTION_STAGE_ID`
/// — see [`ASSEMBLE_STATISTICAL_DISTRIBUTION`] for the drift-safety
/// rationale.
pub const ASSEMBLE_ENSEMBLE_DISTRIBUTION: &str = ENSEMBLE_DISTRIBUTION_STAGE_ID;

/// Deserialized `spec` attributes for the [`ASSEMBLE_STATISTICAL_DISTRIBUTION`]
/// builtin: the K variant stage ids to pool, the shared [`ResultSchema`]
/// used to read each variant's artifact, and the modality re-execution
/// tolerance used for the concordance classification.
#[derive(Debug, Clone, PartialEq)]
pub struct StatBuiltinArgs {
    pub variant_stage_ids: Vec<String>,
    pub schema: ResultSchema,
    pub bounds: ModalityBounds,
}

/// One in-process builtin's decoded request, keyed by which core assembler
/// it dispatches to. Produced by [`builtin_request`]; consumed by
/// [`run_builtin`]. DRYs the dispatch-site snapshot/loop in
/// `main.rs::run_loop` over all three builtins.
#[derive(Debug, Clone, PartialEq)]
pub enum BuiltinRequest {
    ReportData(BTreeMap<String, ResultSchema>),
    StatDistribution(StatBuiltinArgs),
    EnsembleDistribution(Vec<String>),
}

impl BuiltinRequest {
    /// The `spec.builtin` value this request was decoded from — used for
    /// log/progress-line labeling at the dispatch site.
    pub fn builtin_name(&self) -> &'static str {
        match self {
            BuiltinRequest::ReportData(_) => ASSEMBLE_REPORT_DATA,
            BuiltinRequest::StatDistribution(_) => ASSEMBLE_STATISTICAL_DISTRIBUTION,
            BuiltinRequest::EnsembleDistribution(_) => ASSEMBLE_ENSEMBLE_DISTRIBUTION,
        }
    }
}

/// Returns the matched builtin id when `task.spec.builtin` names ANY of
/// the three in-process builtins — regardless of whether the rest of the
/// payload parses. Distinguishes "not a builtin task at all" (this
/// returns `None`, so the caller's normal agent dispatch is correct) from
/// "a known builtin marker whose payload is missing/unparseable" (this
/// returns `Some`, but [`builtin_request`]/[`assemble_statistical_distribution_request`]/etc.
/// also return `None` for the same task) — that second case must fail
/// loud rather than silently falling through to an agent dispatch, since
/// no agent has an atom that can run a builtin id. See
/// `main.rs::run_loop`'s bad-builtin-spec guard, which uses this to mark
/// such a task `Failed` instead of dispatching it.
pub fn known_builtin_marker(task: &Task) -> Option<&'static str> {
    let spec = task.spec.as_ref()?;
    let builtin = spec.get("builtin").and_then(|v| v.as_str())?;
    match builtin {
        ASSEMBLE_REPORT_DATA => Some(ASSEMBLE_REPORT_DATA),
        ASSEMBLE_STATISTICAL_DISTRIBUTION => Some(ASSEMBLE_STATISTICAL_DISTRIBUTION),
        ASSEMBLE_ENSEMBLE_DISTRIBUTION => Some(ASSEMBLE_ENSEMBLE_DISTRIBUTION),
        _ => None,
    }
}

/// Decision predicate for the dispatch site, generalized over all three
/// in-process builtins. Returns `None` for every non-builtin task (and for
/// an unrecognized `spec.builtin` value) so the caller falls through to
/// the normal agent dispatch — regression-safe: tasks without the marker
/// are untouched.
pub fn builtin_request(task: &Task) -> Option<BuiltinRequest> {
    if let Some(schemas) = assemble_report_data_request(task) {
        return Some(BuiltinRequest::ReportData(schemas));
    }
    if let Some(args) = assemble_statistical_distribution_request(task) {
        return Some(BuiltinRequest::StatDistribution(args));
    }
    if let Some(cell_ids) = assemble_ensemble_distribution_request(task) {
        return Some(BuiltinRequest::EnsembleDistribution(cell_ids));
    }
    None
}

/// Runs whichever builtin `request` decodes to, in process, and records
/// the outcome through the normal `state.patch.json` protocol. Thin
/// dispatch wrapper over [`run_assemble_report_data`] /
/// [`run_assemble_statistical_distribution`] /
/// [`run_assemble_ensemble_distribution`] — see those for the
/// success/failure/postcondition contract, which is identical across all
/// three.
pub fn run_builtin(
    package_root: &Path,
    dispatch: &PickedDispatch,
    request: &BuiltinRequest,
    clock: &dyn Clock,
) -> anyhow::Result<TaskState> {
    match request {
        BuiltinRequest::ReportData(schemas) => {
            run_assemble_report_data(package_root, dispatch, schemas, clock)
        }
        BuiltinRequest::StatDistribution(args) => {
            run_assemble_statistical_distribution(package_root, dispatch, args, clock)
        }
        BuiltinRequest::EnsembleDistribution(cell_ids) => {
            run_assemble_ensemble_distribution(package_root, dispatch, cell_ids, clock)
        }
    }
}

/// Decision predicate for the dispatch site.
///
/// Returns `Some(schemas)` when `task.spec.builtin == "assemble_report_data"`,
/// deserializing `task.spec.report_schemas` into a `stage_id → ResultSchema`
/// map. A missing, null, empty, or unparseable `report_schemas` degrades to
/// an empty map — the assembler then writes an artifacts-empty (still valid)
/// report rather than failing. Returns `None` for every non-builtin task so
/// the caller falls through to the normal agent dispatch (regression-safe:
/// tasks without the marker are untouched).
pub fn assemble_report_data_request(task: &Task) -> Option<BTreeMap<String, ResultSchema>> {
    let spec = task.spec.as_ref()?;
    let builtin = spec.get("builtin").and_then(|v| v.as_str())?;
    if builtin != ASSEMBLE_REPORT_DATA {
        return None;
    }
    let schemas = spec
        .get("report_schemas")
        .and_then(|v| serde_json::from_value::<BTreeMap<String, ResultSchema>>(v.clone()).ok())
        .unwrap_or_default();
    Some(schemas)
}

/// Run the report-data assembler in process for a `builtin`-tagged task and
/// record its outcome through the normal `state.patch.json` protocol.
///
/// On assembler success writes a `completed` patch; on assembler failure
/// writes a `failed` patch carrying the error text (mirroring how a real
/// agent reports a failure — the terminal state travels in the patch, not
/// in the process exit code). In both cases the harness's existing strict
/// patch merge drives the task to its terminal state and advances (or
/// halts) dependents. Never panics; never silently swallows an assembler
/// error.
///
/// Returns the terminal [`TaskState`] recorded in the patch (`Completed` on
/// assembler success, `Failed` on assembler error). `Err` is reserved for a
/// catastrophic failure to write the patch file itself — the caller logs it
/// and the task stays Running, recovered by the heartbeat watchdog exactly
/// as when a real agent cannot write its patch.
/// Postcondition for the report-data assembler builtin: the assembler must
/// have left a non-empty `runtime/outputs/reporting/report-data.json`. The
/// assembler writes it on Ok, so this only returns `false` on a filesystem
/// anomaly (a truncated write, or the file removed between assembly and the
/// check) — turning a silent empty/absent report into a hard Failed.
fn report_data_present_and_non_empty(package_root: &Path) -> bool {
    let report_path = package_root
        .join("runtime")
        .join("outputs")
        .join("reporting")
        .join("report-data.json");
    std::fs::metadata(&report_path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

pub fn run_assemble_report_data(
    package_root: &Path,
    dispatch: &PickedDispatch,
    schemas: &BTreeMap<String, ResultSchema>,
    clock: &dyn Clock,
) -> anyhow::Result<TaskState> {
    let (state, result_json) = match assemble_report_data(package_root, schemas, clock) {
        Ok(report) => {
            // Postcondition (belt-and-suspenders): the assembler writes
            // report-data.json on Ok, so a missing/empty file here means a
            // filesystem anomaly (truncated write, removed between assembly and
            // this check). Convert that silent empty-report into a hard Failed
            // rather than a false Completed — the synthesized builtin node
            // carries no required_artifacts, so nothing else asserts it.
            if report_data_present_and_non_empty(package_root) {
                let n = report.artifacts.len();
                let result = serde_json::json!({
                    "status": "completed",
                    "builtin": ASSEMBLE_REPORT_DATA,
                    "report_data": "runtime/outputs/reporting/report-data.json",
                    "n_artifacts": n,
                    "summary": format!("assembled report-data.json from {n} result artifact(s)"),
                });
                (TaskState::Completed { result: result.clone() }, result)
            } else {
                let reason = "[builtin_assemble_report_data_failed] assembler returned Ok but \
                     runtime/outputs/reporting/report-data.json is missing or empty"
                    .to_string();
                let result = serde_json::json!({
                    "status": "failed",
                    "builtin": ASSEMBLE_REPORT_DATA,
                    "summary": reason,
                });
                (TaskState::Failed { reason: reason.clone() }, result)
            }
        }
        Err(e) => {
            let reason = format!("[builtin_assemble_report_data_failed] {e:#}");
            let result = serde_json::json!({
                "status": "failed",
                "builtin": ASSEMBLE_REPORT_DATA,
                "summary": reason,
            });
            (
                TaskState::Failed {
                    reason: reason.clone(),
                },
                result,
            )
        }
    };

    write_builtin_completion(package_root, dispatch, state, &result_json)
}

/// Shared completion write-up for every in-process builtin: `result.json`
/// (the reliable deliverable a normal agent always writes, feeding
/// `status_reconciliation`'s completed-detection and the patch-merge
/// recovery path), `state.patch.json` (carrying the dispatch identity so
/// the strict merge accepts it exactly as it would a real agent's patch),
/// and a refreshed `.heartbeat` (best-effort — the patch is the
/// load-bearing signal). Identical across all three builtins; only the
/// caller's `state`/`result_json` differ.
fn write_builtin_completion(
    package_root: &Path,
    dispatch: &PickedDispatch,
    state: TaskState,
    result_json: &serde_json::Value,
) -> anyhow::Result<TaskState> {
    let task_id = dispatch.task_id.as_str();
    let task_dir = package_root.join("runtime").join("outputs").join(task_id);
    std::fs::create_dir_all(&task_dir)
        .map_err(|e| anyhow::anyhow!("creating {}: {e}", task_dir.display()))?;

    let result_pretty = serde_json::to_string_pretty(result_json)
        .map_err(|e| anyhow::anyhow!("serializing result.json for {task_id}: {e}"))?;
    std::fs::write(task_dir.join("result.json"), result_pretty)
        .map_err(|e| anyhow::anyhow!("writing result.json for {task_id}: {e}"))?;

    let patch = StatePatch {
        schema_version: state_patch_schema_version(),
        from: Some("running".to_string()),
        harness_run_id: Some(dispatch.harness_run_id.clone()),
        dispatch_epoch: Some(dispatch.epoch),
        to: state.clone(),
        note: None,
    };
    let patch_pretty = serde_json::to_string_pretty(&patch)
        .map_err(|e| anyhow::anyhow!("serializing state.patch.json for {task_id}: {e}"))?;
    std::fs::write(task_dir.join("state.patch.json"), patch_pretty)
        .map_err(|e| anyhow::anyhow!("writing state.patch.json for {task_id}: {e}"))?;

    let hb = task_dir.join(".heartbeat");
    let _ = std::fs::write(&hb, ecaa_workflow_core::time_helpers::now_rfc3339());

    Ok(state)
}

/// Decision predicate for the [`ASSEMBLE_STATISTICAL_DISTRIBUTION`]
/// builtin. Returns `Some(args)` when `task.spec.builtin ==
/// "assemble_statistical_distribution"` AND `task.spec.result_schema`
/// deserializes into a [`ResultSchema`] — the schema is load-bearing (there
/// is no sensible default), so an unparseable/absent schema returns `None`
/// (falls through to the normal agent dispatch) rather than fabricating
/// one. `variant_stage_ids` degrades to an empty vec when missing or
/// unparseable (the assembler then reports zero methods rather than
/// failing). `relative_tolerance`/`absolute_tolerance` default to
/// [`ModalityBounds::default`] (±5% relative) when absent.
pub fn assemble_statistical_distribution_request(task: &Task) -> Option<StatBuiltinArgs> {
    let spec = task.spec.as_ref()?;
    let builtin = spec.get("builtin").and_then(|v| v.as_str())?;
    if builtin != ASSEMBLE_STATISTICAL_DISTRIBUTION {
        return None;
    }
    let schema = spec
        .get("result_schema")
        .and_then(|v| serde_json::from_value::<ResultSchema>(v.clone()).ok())?;
    let variant_stage_ids = spec
        .get("variant_stage_ids")
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default();
    let mut bounds = ModalityBounds::default();
    if let Some(r) = spec.get("relative_tolerance").and_then(|v| v.as_f64()) {
        bounds.relative_tolerance = r;
    }
    if let Some(a) = spec.get("absolute_tolerance").and_then(|v| v.as_f64()) {
        bounds.absolute_tolerance = a;
    }
    Some(StatBuiltinArgs {
        variant_stage_ids,
        schema,
        bounds,
    })
}

/// Postcondition for the statistical-distribution aggregator builtin: the
/// assembler must have left a non-empty
/// `runtime/outputs/assemble_statistical_distribution/stat-distribution.json`.
/// See [`report_data_present_and_non_empty`] for the rationale (only
/// returns `false` on a filesystem anomaly).
fn stat_distribution_present_and_non_empty(package_root: &Path) -> bool {
    let path = package_root
        .join("runtime")
        .join("outputs")
        .join(ASSEMBLE_STATISTICAL_DISTRIBUTION)
        .join("stat-distribution.json");
    std::fs::metadata(&path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

/// Run the cross-method statistical-distribution aggregator in process for
/// a `builtin`-tagged task, mirroring [`run_assemble_report_data`]'s
/// success/failure/postcondition contract exactly.
pub fn run_assemble_statistical_distribution(
    package_root: &Path,
    dispatch: &PickedDispatch,
    args: &StatBuiltinArgs,
    clock: &dyn Clock,
) -> anyhow::Result<TaskState> {
    let (state, result_json) = match assemble_statistical_distribution(
        package_root,
        &args.variant_stage_ids,
        &args.schema,
        &args.bounds,
        clock,
    ) {
        Ok(dist) => {
            if stat_distribution_present_and_non_empty(package_root) {
                let n_entities = dist.entities.len();
                let n_methods = dist.methods.len();
                let result = serde_json::json!({
                    "status": "completed",
                    "builtin": ASSEMBLE_STATISTICAL_DISTRIBUTION,
                    "stat_distribution": format!(
                        "runtime/outputs/{ASSEMBLE_STATISTICAL_DISTRIBUTION}/stat-distribution.json"
                    ),
                    "n_methods": n_methods,
                    "n_entities": n_entities,
                    "n_robust": dist.n_robust,
                    "n_concordant": dist.n_concordant,
                    "n_fragile": dist.n_fragile,
                    "n_discordant": dist.n_discordant,
                    "summary": format!(
                        "assembled stat-distribution.json across {n_methods} method(s), {n_entities} entities"
                    ),
                });
                (TaskState::Completed { result: result.clone() }, result)
            } else {
                let reason = format!(
                    "[builtin_assemble_statistical_distribution_failed] assembler returned Ok \
                     but runtime/outputs/{ASSEMBLE_STATISTICAL_DISTRIBUTION}/stat-distribution.json \
                     is missing or empty"
                );
                let result = serde_json::json!({
                    "status": "failed",
                    "builtin": ASSEMBLE_STATISTICAL_DISTRIBUTION,
                    "summary": reason,
                });
                (TaskState::Failed { reason: reason.clone() }, result)
            }
        }
        Err(e) => {
            let reason = format!("[builtin_assemble_statistical_distribution_failed] {e:#}");
            let result = serde_json::json!({
                "status": "failed",
                "builtin": ASSEMBLE_STATISTICAL_DISTRIBUTION,
                "summary": reason,
            });
            (TaskState::Failed { reason: reason.clone() }, result)
        }
    };

    write_builtin_completion(package_root, dispatch, state, &result_json)
}

/// Decision predicate for the [`ASSEMBLE_ENSEMBLE_DISTRIBUTION`] builtin.
/// Returns `Some(cell_ids)` when `task.spec.builtin ==
/// "assemble_ensemble_distribution"`, deserializing
/// `task.spec.interpretation_cell_ids` into a `Vec<String>`. A missing,
/// null, or unparseable list degrades to an empty vec — the assembler then
/// writes a cells-empty (still valid) distribution rather than failing.
/// Returns `None` for every non-matching task.
pub fn assemble_ensemble_distribution_request(task: &Task) -> Option<Vec<String>> {
    let spec = task.spec.as_ref()?;
    let builtin = spec.get("builtin").and_then(|v| v.as_str())?;
    if builtin != ASSEMBLE_ENSEMBLE_DISTRIBUTION {
        return None;
    }
    let cell_ids = spec
        .get("interpretation_cell_ids")
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default();
    Some(cell_ids)
}

/// Postcondition for the ensemble-distribution aggregator builtin: the
/// assembler must have left a non-empty
/// `runtime/outputs/assemble_ensemble_distribution/ensemble-distribution.json`.
/// See [`report_data_present_and_non_empty`] for the rationale.
fn ensemble_distribution_present_and_non_empty(package_root: &Path) -> bool {
    let path = package_root
        .join("runtime")
        .join("outputs")
        .join(ASSEMBLE_ENSEMBLE_DISTRIBUTION)
        .join("ensemble-distribution.json");
    std::fs::metadata(&path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

/// Run the cross-axis (method × interpretive-lens) ensemble-distribution
/// aggregator in process for a `builtin`-tagged task, mirroring
/// [`run_assemble_report_data`]'s success/failure/postcondition contract
/// exactly.
pub fn run_assemble_ensemble_distribution(
    package_root: &Path,
    dispatch: &PickedDispatch,
    cell_ids: &[String],
    clock: &dyn Clock,
) -> anyhow::Result<TaskState> {
    let (state, result_json) =
        match assemble_ensemble_distribution(package_root, cell_ids, clock) {
            Ok(dist) => {
                if ensemble_distribution_present_and_non_empty(package_root) {
                    let n_cells = dist.cells.len();
                    let result = serde_json::json!({
                        "status": "completed",
                        "builtin": ASSEMBLE_ENSEMBLE_DISTRIBUTION,
                        "ensemble_distribution": format!(
                            "runtime/outputs/{ASSEMBLE_ENSEMBLE_DISTRIBUTION}/ensemble-distribution.json"
                        ),
                        "n_cells": n_cells,
                        "agreement": dist.agreement,
                        "summary": format!(
                            "assembled ensemble-distribution.json across {n_cells} interpretation cell(s)"
                        ),
                    });
                    (TaskState::Completed { result: result.clone() }, result)
                } else {
                    let reason = format!(
                        "[builtin_assemble_ensemble_distribution_failed] assembler returned Ok \
                         but runtime/outputs/{ASSEMBLE_ENSEMBLE_DISTRIBUTION}/ensemble-distribution.json \
                         is missing or empty"
                    );
                    let result = serde_json::json!({
                        "status": "failed",
                        "builtin": ASSEMBLE_ENSEMBLE_DISTRIBUTION,
                        "summary": reason,
                    });
                    (TaskState::Failed { reason: reason.clone() }, result)
                }
            }
            Err(e) => {
                let reason = format!("[builtin_assemble_ensemble_distribution_failed] {e:#}");
                let result = serde_json::json!({
                    "status": "failed",
                    "builtin": ASSEMBLE_ENSEMBLE_DISTRIBUTION,
                    "summary": reason,
                });
                (TaskState::Failed { reason: reason.clone() }, result)
            }
        };

    write_builtin_completion(package_root, dispatch, state, &result_json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dag_patch::apply_pending_patches_strict;
    use ecaa_workflow_core::clock::WallClock;
    use ecaa_workflow_core::dag::{Assignee, ResourceClass, TaskId, TaskKind, DAG};
    use ecaa_workflow_core::report_contract::{Comparator, Significance};

    fn de_schema() -> ResultSchema {
        ResultSchema {
            artifact: "de_results.tsv".into(),
            entity_column: "gene".into(),
            entity_column_aliases: Vec::new(),
            significance: Some(Significance {
                column: "padj".into(),
                threshold: 0.05,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: Some("log2FoldChange".into()),
            signed_effect_aliases: Vec::new(),
            grouping_column: None,
        }
    }

    fn stage_de_results(outputs: &Path) {
        let dir = outputs.join("differential_expression");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("de_results.tsv"),
            "gene\tlog2FoldChange\tpadj\n\
             ENSG1\t5.0\t0.001\n\
             ENSG2\t-4.8\t0.002\n\
             ENSG3\t0.1\t0.9\n",
        )
        .unwrap();
    }

    /// Build a Task carrying the `assemble_report_data` builtin spec with the
    /// given `report_schemas`, in the given state.
    fn builtin_task(state: TaskState, schemas: &BTreeMap<String, ResultSchema>) -> Task {
        let spec = serde_json::json!({
            "builtin": ASSEMBLE_REPORT_DATA,
            "report_schemas": schemas,
        });
        Task {
            kind: TaskKind::Computation,
            state,
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "assemble report data".into(),
            spec: Some(spec),
            resolution: None,
            result_ref: None,
            resource_class: ResourceClass::CpuHeavy,
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: None,
            safety: Default::default(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            edam_operation: None,
            execution_index: None,
        }
    }

    fn single_task_dag(id: &str, task: Task) -> DAG {
        let mut tasks = BTreeMap::new();
        tasks.insert(TaskId::from(id), task);
        DAG {
            version: "1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "w".into(),
            current_task: None,
            tasks,
            reverse_deps: BTreeMap::new(),
            run_id: None,
            execution_order: Vec::new(),
        }
    }

    fn write_workflow(dir: &Path, dag: &DAG) {
        std::fs::write(
            dir.join("WORKFLOW.json"),
            serde_json::to_string_pretty(dag).unwrap(),
        )
        .unwrap();
    }

    // ---- decision predicate -------------------------------------------

    #[test]
    fn predicate_detects_builtin_and_extracts_schemas() {
        let mut schemas = BTreeMap::new();
        schemas.insert("differential_expression".to_string(), de_schema());
        let task = builtin_task(TaskState::Ready, &schemas);
        let got = assemble_report_data_request(&task)
            .expect("builtin task must be detected");
        assert!(got.contains_key("differential_expression"));
        assert_eq!(got["differential_expression"].artifact, "de_results.tsv");
    }

    /// Regression guard: a normal (non-builtin) task returns None so the
    /// dispatch site falls through to the executor exactly as before.
    #[test]
    fn predicate_returns_none_for_normal_task() {
        // No spec at all.
        let mut t = builtin_task(TaskState::Ready, &BTreeMap::new());
        t.spec = None;
        assert!(assemble_report_data_request(&t).is_none());

        // Spec present but no builtin marker (a real analytical task).
        let t2 = Task {
            spec: Some(serde_json::json!({ "atom_id": "differential_expression" })),
            ..builtin_task(TaskState::Ready, &BTreeMap::new())
        };
        assert!(assemble_report_data_request(&t2).is_none());

        // Spec present with a DIFFERENT builtin value.
        let t3 = Task {
            spec: Some(serde_json::json!({ "builtin": "something_else" })),
            ..builtin_task(TaskState::Ready, &BTreeMap::new())
        };
        assert!(assemble_report_data_request(&t3).is_none());
    }

    #[test]
    fn predicate_empty_schemas_when_missing_or_unparseable() {
        // builtin present, report_schemas absent → empty map (still Some).
        let t = Task {
            spec: Some(serde_json::json!({ "builtin": ASSEMBLE_REPORT_DATA })),
            ..builtin_task(TaskState::Ready, &BTreeMap::new())
        };
        assert_eq!(assemble_report_data_request(&t), Some(BTreeMap::new()));

        // builtin present, report_schemas unparseable → empty map.
        let t2 = Task {
            spec: Some(serde_json::json!({
                "builtin": ASSEMBLE_REPORT_DATA,
                "report_schemas": "not-a-map",
            })),
            ..builtin_task(TaskState::Ready, &BTreeMap::new())
        };
        assert_eq!(assemble_report_data_request(&t2), Some(BTreeMap::new()));
    }

    // ---- in-process run drives Completed through the normal protocol --

    #[test]
    fn in_process_run_produces_report_and_drives_completed_via_patch_merge() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outputs = root.join("runtime").join("outputs");
        stage_de_results(&outputs);

        let mut schemas = BTreeMap::new();
        schemas.insert("differential_expression".to_string(), de_schema());

        // A Running (pre-marked) builtin task on disk — as the harness
        // leaves it right before dispatch.
        let dag = single_task_dag(
            "assemble_report_data",
            builtin_task(
                TaskState::Running {
                    started_at: "2026-01-01T00:00:00Z".into(),
                    remote: None,
                },
                &schemas,
            ),
        );
        write_workflow(root, &dag);

        let dispatch = PickedDispatch {
            task_id: TaskId::from("assemble_report_data"),
            harness_run_id: "run-1".into(),
            epoch: 3,
        };

        let clock = WallClock;
        let state =
            run_assemble_report_data(root, &dispatch, &schemas, &clock).expect("no write failure");
        assert!(
            matches!(state, TaskState::Completed { .. }),
            "assembler success must record Completed, got {state:?}"
        );

        // The core assembler produced the report.
        assert!(
            root.join("runtime/outputs/reporting/report-data.json").is_file(),
            "report-data.json must exist after the in-process run"
        );
        // The same completion markers a normal agent writes.
        assert!(root.join("runtime/outputs/assemble_report_data/result.json").is_file());
        assert!(root.join("runtime/outputs/assemble_report_data/state.patch.json").is_file());
        assert!(root.join("runtime/outputs/assemble_report_data/.heartbeat").is_file());

        // Drive completion through the EXACT strict merge the harness uses.
        let merged = apply_pending_patches_strict(root, &[dispatch]).unwrap();
        match &merged.tasks.get("assemble_report_data").unwrap().state {
            TaskState::Completed { result } => {
                assert_eq!(result["builtin"], ASSEMBLE_REPORT_DATA);
                assert_eq!(result["n_artifacts"], 1);
            }
            other => panic!("expected Completed after strict merge, got {other:?}"),
        }
        // Patch consumed (renamed to .applied) — the normal merge contract.
        assert!(!root
            .join("runtime/outputs/assemble_report_data/state.patch.json")
            .exists());
        assert!(root
            .join("runtime/outputs/assemble_report_data/state.patch.applied.json")
            .exists());
    }

    /// Empty schemas: the assembler still writes a valid (artifacts-empty)
    /// report and the task completes — never a failure.
    #[test]
    fn in_process_run_empty_schemas_still_completes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("runtime/outputs")).unwrap();
        let dispatch = PickedDispatch {
            task_id: TaskId::from("assemble_report_data"),
            harness_run_id: "run-1".into(),
            epoch: 1,
        };
        let clock = WallClock;
        let state =
            run_assemble_report_data(root, &dispatch, &BTreeMap::new(), &clock).unwrap();
        assert!(matches!(state, TaskState::Completed { .. }));
        assert!(root.join("runtime/outputs/reporting/report-data.json").is_file());
    }

    /// F9 postcondition: the report-data.json existence/non-empty check that
    /// converts a silent empty/absent report (assembler Ok but no file) into a
    /// Failed task. The assembler always writes the file on Ok, so the failure
    /// arm is exercised directly against the helper.
    #[test]
    fn report_data_postcondition_detects_missing_and_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Missing entirely → false.
        assert!(!report_data_present_and_non_empty(root));
        // Present but empty (zero bytes) → false.
        let reporting = root.join("runtime/outputs/reporting");
        std::fs::create_dir_all(&reporting).unwrap();
        std::fs::write(reporting.join("report-data.json"), "").unwrap();
        assert!(!report_data_present_and_non_empty(root));
        // Non-empty → true.
        std::fs::write(reporting.join("report-data.json"), "{}").unwrap();
        assert!(report_data_present_and_non_empty(root));
    }

    /// F9 (happy path): a successful in-process run leaves a non-empty
    /// report-data.json, so the postcondition holds and the task Completes.
    #[test]
    fn in_process_run_postcondition_holds_on_success() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        stage_de_results(&root.join("runtime").join("outputs"));
        let mut schemas = BTreeMap::new();
        schemas.insert("differential_expression".to_string(), de_schema());
        let dispatch = PickedDispatch {
            task_id: TaskId::from("assemble_report_data"),
            harness_run_id: "run-1".into(),
            epoch: 1,
        };
        let clock = WallClock;
        let state = run_assemble_report_data(root, &dispatch, &schemas, &clock).unwrap();
        assert!(matches!(state, TaskState::Completed { .. }));
        assert!(report_data_present_and_non_empty(root));
    }

    /// Assembler error path: a report_schema pointing at an artifact that
    /// exists but is unreadable (invalid UTF-8 → csv read error) makes
    /// `assemble_report_data` return Err. The runner records Failed — not a
    /// panic, not a false Completed — and the strict merge drives the task
    /// to Failed.
    #[test]
    fn in_process_run_records_failed_on_assembler_error() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outputs = root.join("runtime").join("outputs");
        let de_dir = outputs.join("differential_expression");
        std::fs::create_dir_all(&de_dir).unwrap();
        // Header row is clean; a data row carries invalid UTF-8 so the csv
        // StringRecord parse in read_table errors (the file EXISTS, so the
        // assembler's presence check passes and it attempts the read).
        let mut bytes = b"gene\tlog2FoldChange\tpadj\n".to_vec();
        bytes.extend_from_slice(&[0xff, 0xfe]);
        bytes.extend_from_slice(b"\t1.0\t0.01\n");
        std::fs::write(de_dir.join("de_results.tsv"), &bytes).unwrap();

        let mut schemas = BTreeMap::new();
        schemas.insert("differential_expression".to_string(), de_schema());

        let dag = single_task_dag(
            "assemble_report_data",
            builtin_task(
                TaskState::Running {
                    started_at: "2026-01-01T00:00:00Z".into(),
                    remote: None,
                },
                &schemas,
            ),
        );
        write_workflow(root, &dag);

        let dispatch = PickedDispatch {
            task_id: TaskId::from("assemble_report_data"),
            harness_run_id: "run-1".into(),
            epoch: 2,
        };
        let clock = WallClock;
        let state = run_assemble_report_data(root, &dispatch, &schemas, &clock)
            .expect("a task failure is not a harness write failure");
        match &state {
            TaskState::Failed { reason } => {
                assert!(
                    reason.contains("builtin_assemble_report_data_failed"),
                    "failed reason must carry the builtin marker, got {reason}"
                );
            }
            other => panic!("expected Failed on assembler error, got {other:?}"),
        }

        // The strict merge drives the task to Failed through the normal path.
        let merged = apply_pending_patches_strict(root, &[dispatch]).unwrap();
        assert!(
            matches!(
                merged.tasks.get("assemble_report_data").unwrap().state,
                TaskState::Failed { .. }
            ),
            "task must merge to Failed"
        );
    }

    // ---- assemble_statistical_distribution builtin --------------------

    fn write_variant(outputs: &Path, vid: &str, body: &str) {
        let dir = outputs.join(vid);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("de_results.tsv"), body).unwrap();
    }

    const DESEQ2: &str = "differential_expression__v_deseq2";
    const EDGER: &str = "differential_expression__v_edger";

    /// Two method tables agreeing on every gene (both significant, same
    /// sign) — everything classifies Robust.
    fn write_two_robust_variants(outputs: &Path) {
        write_variant(
            outputs,
            DESEQ2,
            "gene\tlog2FoldChange\tpadj\n\
             ENSG1\t2.0\t0.001\n\
             ENSG2\t-3.0\t0.002\n",
        );
        write_variant(
            outputs,
            EDGER,
            "gene\tlog2FoldChange\tpadj\n\
             ENSG1\t2.1\t0.002\n\
             ENSG2\t-2.9\t0.003\n",
        );
    }

    /// Build a Task carrying the `assemble_statistical_distribution`
    /// builtin spec.
    fn stat_builtin_task(state: TaskState, args: &StatBuiltinArgs) -> Task {
        let spec = serde_json::json!({
            "builtin": ASSEMBLE_STATISTICAL_DISTRIBUTION,
            "variant_stage_ids": args.variant_stage_ids,
            "result_schema": args.schema,
            "relative_tolerance": args.bounds.relative_tolerance,
            "absolute_tolerance": args.bounds.absolute_tolerance,
        });
        Task {
            spec: Some(spec),
            ..builtin_task(state, &BTreeMap::new())
        }
    }

    #[test]
    fn predicate_detects_stat_distribution_and_extracts_args() {
        let args = StatBuiltinArgs {
            variant_stage_ids: vec![DESEQ2.to_string(), EDGER.to_string()],
            schema: de_schema(),
            bounds: ModalityBounds::default(),
        };
        let task = stat_builtin_task(TaskState::Ready, &args);
        let got = assemble_statistical_distribution_request(&task)
            .expect("stat-distribution builtin task must be detected");
        assert_eq!(got, args);
    }

    /// A stat-distribution task with no `result_schema` (or an unparseable
    /// one) returns `None` — the schema is load-bearing and there is no
    /// sensible default, so the dispatch site falls through rather than
    /// fabricating one.
    #[test]
    fn predicate_stat_distribution_returns_none_without_schema() {
        let t = Task {
            spec: Some(serde_json::json!({
                "builtin": ASSEMBLE_STATISTICAL_DISTRIBUTION,
                "variant_stage_ids": [DESEQ2],
            })),
            ..builtin_task(TaskState::Ready, &BTreeMap::new())
        };
        assert!(assemble_statistical_distribution_request(&t).is_none());
    }

    /// Regression guard: normal / non-matching tasks fall through.
    #[test]
    fn predicate_stat_distribution_returns_none_for_normal_task() {
        let mut t = builtin_task(TaskState::Ready, &BTreeMap::new());
        t.spec = None;
        assert!(assemble_statistical_distribution_request(&t).is_none());

        let t2 = Task {
            spec: Some(serde_json::json!({ "builtin": ASSEMBLE_REPORT_DATA })),
            ..builtin_task(TaskState::Ready, &BTreeMap::new())
        };
        assert!(assemble_statistical_distribution_request(&t2).is_none());
    }

    #[test]
    fn stat_distribution_builtin_runs_in_process() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outputs = root.join("runtime").join("outputs");
        write_two_robust_variants(&outputs);

        let args = StatBuiltinArgs {
            variant_stage_ids: vec![DESEQ2.to_string(), EDGER.to_string()],
            schema: de_schema(),
            bounds: ModalityBounds::default(),
        };

        let dag = single_task_dag(
            ASSEMBLE_STATISTICAL_DISTRIBUTION,
            stat_builtin_task(
                TaskState::Running {
                    started_at: "2026-01-01T00:00:00Z".into(),
                    remote: None,
                },
                &args,
            ),
        );
        write_workflow(root, &dag);

        let dispatch = PickedDispatch {
            task_id: TaskId::from(ASSEMBLE_STATISTICAL_DISTRIBUTION),
            harness_run_id: "run-1".into(),
            epoch: 1,
        };
        let clock = WallClock;
        let state = run_assemble_statistical_distribution(root, &dispatch, &args, &clock)
            .expect("no write failure");
        assert!(
            matches!(state, TaskState::Completed { .. }),
            "assembler success must record Completed, got {state:?}"
        );

        assert!(
            outputs
                .join(ASSEMBLE_STATISTICAL_DISTRIBUTION)
                .join("stat-distribution.json")
                .is_file(),
            "stat-distribution.json must exist after the in-process run"
        );
        let task_dir = outputs.join(ASSEMBLE_STATISTICAL_DISTRIBUTION);
        assert!(task_dir.join("result.json").is_file());
        assert!(task_dir.join("state.patch.json").is_file());
        assert!(task_dir.join(".heartbeat").is_file());

        // Drive completion through the exact strict merge the harness uses.
        let merged = apply_pending_patches_strict(root, &[dispatch]).unwrap();
        match &merged
            .tasks
            .get(ASSEMBLE_STATISTICAL_DISTRIBUTION)
            .unwrap()
            .state
        {
            TaskState::Completed { result } => {
                assert_eq!(result["builtin"], ASSEMBLE_STATISTICAL_DISTRIBUTION);
                assert_eq!(result["n_robust"], 2);
            }
            other => panic!("expected Completed after strict merge, got {other:?}"),
        }
    }

    /// Through the generalized dispatch predicate/runner, exercised end to
    /// end alongside `assemble_report_data`.
    #[test]
    fn builtin_request_detects_stat_distribution_variant() {
        let args = StatBuiltinArgs {
            variant_stage_ids: vec![DESEQ2.to_string()],
            schema: de_schema(),
            bounds: ModalityBounds::default(),
        };
        let task = stat_builtin_task(TaskState::Ready, &args);
        match builtin_request(&task) {
            Some(BuiltinRequest::StatDistribution(got)) => assert_eq!(got, args),
            other => panic!("expected StatDistribution request, got {other:?}"),
        }
    }

    // ---- assemble_ensemble_distribution builtin ------------------------

    fn ensemble_cell_id(method: &str, lens: &str) -> String {
        format!("biological_interpretation__m_{method}__lens_{lens}")
    }

    fn write_cell(outputs: &Path, method: &str, lens: &str, support: bool) {
        let id = ensemble_cell_id(method, lens);
        let dir = outputs.join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({ "hypothesis_supported": support, "literature": [] });
        std::fs::write(dir.join("result.json"), serde_json::to_string_pretty(&body).unwrap())
            .unwrap();
    }

    /// Build a Task carrying the `assemble_ensemble_distribution` builtin
    /// spec with the given `interpretation_cell_ids`.
    fn ensemble_builtin_task(state: TaskState, cell_ids: &[String]) -> Task {
        let spec = serde_json::json!({
            "builtin": ASSEMBLE_ENSEMBLE_DISTRIBUTION,
            "interpretation_cell_ids": cell_ids,
        });
        Task {
            spec: Some(spec),
            ..builtin_task(state, &BTreeMap::new())
        }
    }

    #[test]
    fn predicate_detects_ensemble_distribution_and_extracts_cell_ids() {
        let ids = vec![
            ensemble_cell_id("deseq2", "molecular_mechanism"),
            ensemble_cell_id("edger", "molecular_mechanism"),
        ];
        let task = ensemble_builtin_task(TaskState::Ready, &ids);
        let got = assemble_ensemble_distribution_request(&task)
            .expect("ensemble-distribution builtin task must be detected");
        assert_eq!(got, ids);
    }

    /// Regression guard: normal / non-matching tasks fall through.
    #[test]
    fn predicate_ensemble_distribution_returns_none_for_normal_task() {
        let mut t = builtin_task(TaskState::Ready, &BTreeMap::new());
        t.spec = None;
        assert!(assemble_ensemble_distribution_request(&t).is_none());

        let t2 = Task {
            spec: Some(serde_json::json!({ "builtin": ASSEMBLE_REPORT_DATA })),
            ..builtin_task(TaskState::Ready, &BTreeMap::new())
        };
        assert!(assemble_ensemble_distribution_request(&t2).is_none());
    }

    #[test]
    fn ensemble_distribution_builtin_runs_in_process() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outputs = root.join("runtime").join("outputs");
        write_cell(&outputs, "deseq2", "molecular_mechanism", true);
        write_cell(&outputs, "edger", "molecular_mechanism", true);
        write_cell(&outputs, "limma", "molecular_mechanism", false);
        let ids = vec![
            ensemble_cell_id("deseq2", "molecular_mechanism"),
            ensemble_cell_id("edger", "molecular_mechanism"),
            ensemble_cell_id("limma", "molecular_mechanism"),
        ];

        let dag = single_task_dag(
            ASSEMBLE_ENSEMBLE_DISTRIBUTION,
            ensemble_builtin_task(
                TaskState::Running {
                    started_at: "2026-01-01T00:00:00Z".into(),
                    remote: None,
                },
                &ids,
            ),
        );
        write_workflow(root, &dag);

        let dispatch = PickedDispatch {
            task_id: TaskId::from(ASSEMBLE_ENSEMBLE_DISTRIBUTION),
            harness_run_id: "run-1".into(),
            epoch: 1,
        };
        let clock = WallClock;
        let state = run_assemble_ensemble_distribution(root, &dispatch, &ids, &clock)
            .expect("no write failure");
        assert!(
            matches!(state, TaskState::Completed { .. }),
            "assembler success must record Completed, got {state:?}"
        );

        assert!(
            outputs
                .join(ASSEMBLE_ENSEMBLE_DISTRIBUTION)
                .join("ensemble-distribution.json")
                .is_file(),
            "ensemble-distribution.json must exist after the in-process run"
        );
        let task_dir = outputs.join(ASSEMBLE_ENSEMBLE_DISTRIBUTION);
        assert!(task_dir.join("result.json").is_file());
        assert!(task_dir.join("state.patch.json").is_file());
        assert!(task_dir.join(".heartbeat").is_file());

        // Drive completion through the exact strict merge the harness uses.
        let merged = apply_pending_patches_strict(root, &[dispatch]).unwrap();
        match &merged
            .tasks
            .get(ASSEMBLE_ENSEMBLE_DISTRIBUTION)
            .unwrap()
            .state
        {
            TaskState::Completed { result } => {
                assert_eq!(result["builtin"], ASSEMBLE_ENSEMBLE_DISTRIBUTION);
                assert_eq!(result["n_cells"], 3);
            }
            other => panic!("expected Completed after strict merge, got {other:?}"),
        }
    }

    /// Through the generalized dispatch predicate/runner, exercised end to
    /// end alongside the other two builtins.
    #[test]
    fn builtin_request_detects_ensemble_distribution_variant() {
        let ids = vec![ensemble_cell_id("deseq2", "molecular_mechanism")];
        let task = ensemble_builtin_task(TaskState::Ready, &ids);
        match builtin_request(&task) {
            Some(BuiltinRequest::EnsembleDistribution(got)) => assert_eq!(got, ids),
            other => panic!("expected EnsembleDistribution request, got {other:?}"),
        }
    }

    /// The generalized runner dispatches to the correct assembler for each
    /// of the three builtin kinds.
    #[test]
    fn run_builtin_dispatches_to_report_data_variant() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        stage_de_results(&root.join("runtime").join("outputs"));
        let mut schemas = BTreeMap::new();
        schemas.insert("differential_expression".to_string(), de_schema());
        let dispatch = PickedDispatch {
            task_id: TaskId::from("assemble_report_data"),
            harness_run_id: "run-1".into(),
            epoch: 1,
        };
        let clock = WallClock;
        let state = run_builtin(
            root,
            &dispatch,
            &BuiltinRequest::ReportData(schemas),
            &clock,
        )
        .unwrap();
        assert!(matches!(state, TaskState::Completed { .. }));
    }

    // ---- cross-crate const contract + fail-loud bad-spec guard --------

    /// Pins the harness↔core builtin-id contract from Fix A: the harness's
    /// match-values are core's [`STAT_DISTRIBUTION_STAGE_ID`]/
    /// [`ENSEMBLE_DISTRIBUTION_STAGE_ID`]. The `use` import already makes
    /// this definitional (a core rename would be a compile error here, not
    /// a silent runtime desync) — this test documents the contract
    /// explicitly rather than relying solely on that structural guarantee.
    #[test]
    fn harness_builtin_ids_are_core_stage_ids() {
        assert_eq!(
            ASSEMBLE_STATISTICAL_DISTRIBUTION,
            ecaa_workflow_core::report_contract::ensemble_assemble::STAT_DISTRIBUTION_STAGE_ID
        );
        assert_eq!(
            ASSEMBLE_ENSEMBLE_DISTRIBUTION,
            ecaa_workflow_core::report_contract::ensemble_assemble::ENSEMBLE_DISTRIBUTION_STAGE_ID
        );
    }

    /// Fix B: a task carrying a KNOWN builtin marker
    /// (`spec.builtin == STAT_DISTRIBUTION_STAGE_ID`) but missing the
    /// load-bearing `result_schema` is recognized by
    /// [`known_builtin_marker`] even though [`builtin_request`] (and
    /// [`assemble_statistical_distribution_request`]) correctly return
    /// `None` for it — the dispatch-site gap that lets such a task fall
    /// through to an agent with no atom to run. `main.rs::run_loop`'s
    /// bad-builtin-spec guard uses exactly this asymmetry to fail loud
    /// instead.
    #[test]
    fn known_builtin_with_bad_spec_fails_loud() {
        let t = Task {
            spec: Some(serde_json::json!({
                "builtin": ASSEMBLE_STATISTICAL_DISTRIBUTION,
                // no "result_schema" — load-bearing, no default.
            })),
            ..builtin_task(TaskState::Ready, &BTreeMap::new())
        };

        // The normal decision predicates fall through (None) — this is the
        // silent-fallthrough half of the bug.
        assert!(
            assemble_statistical_distribution_request(&t).is_none(),
            "missing result_schema must not fabricate a request"
        );
        assert!(
            builtin_request(&t).is_none(),
            "generalized predicate must also fall through"
        );

        // But the marker IS recognized — the loud-fail path can trigger.
        assert_eq!(
            known_builtin_marker(&t),
            Some(ASSEMBLE_STATISTICAL_DISTRIBUTION),
            "a known builtin id with a bad payload must still be recognized \
             as a builtin task, not treated as a normal agent task"
        );
    }

    /// Regression guard: a genuinely normal (non-builtin) task is neither a
    /// builtin request NOR a known-but-bad-spec marker — it must dispatch
    /// to the agent exactly as before.
    #[test]
    fn known_builtin_marker_returns_none_for_normal_task() {
        let mut t = builtin_task(TaskState::Ready, &BTreeMap::new());
        t.spec = None;
        assert!(known_builtin_marker(&t).is_none());

        let t2 = Task {
            spec: Some(serde_json::json!({ "atom_id": "differential_expression" })),
            ..builtin_task(TaskState::Ready, &BTreeMap::new())
        };
        assert!(known_builtin_marker(&t2).is_none());

        let t3 = Task {
            spec: Some(serde_json::json!({ "builtin": "something_else" })),
            ..builtin_task(TaskState::Ready, &BTreeMap::new())
        };
        assert!(known_builtin_marker(&t3).is_none());
    }

    /// A well-formed builtin task IS recognized by both the marker and the
    /// full decision predicate.
    #[test]
    fn known_builtin_marker_matches_well_formed_report_data_task() {
        let mut schemas = BTreeMap::new();
        schemas.insert("differential_expression".to_string(), de_schema());
        let t = builtin_task(TaskState::Ready, &schemas);
        assert_eq!(known_builtin_marker(&t), Some(ASSEMBLE_REPORT_DATA));
        assert!(assemble_report_data_request(&t).is_some());
    }
}
