//! In-process dispatch for `builtin`-tagged DAG tasks.
//!
//! Some synthesized DAG nodes carry a `spec.builtin` marker instead of
//! being agent-executed. The composer emits one `assemble_report_data`
//! node — downstream of every schema-bearing analytical stage, upstream of
//! the reporting terminals — that the harness runs deterministically IN
//! PROCESS (no agent subprocess) by calling the committed core assembler
//! [`ecaa_workflow_core::report_contract::assemble_report_data`].
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
//! scheduler advances the downstream `reporting` task with zero
//! special-casing. The builtin task never reaches the executor, so
//! `Executor::run_iteration` is never invoked for it.

use std::collections::BTreeMap;
use std::path::Path;

use ecaa_workflow_core::clock::Clock;
use ecaa_workflow_core::dag::{Task, TaskState};
use ecaa_workflow_core::report_contract::{assemble_report_data, ResultSchema};

use crate::dag_patch::{state_patch_schema_version, PickedDispatch, StatePatch};

/// Value of a task's `spec.builtin` attribute marking the report-data
/// assembler builtin (stamped by the composer's report-data synthesis
/// pass; surfaced into the lowered task spec by the workflow_json emitter).
pub const ASSEMBLE_REPORT_DATA: &str = "assemble_report_data";

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
pub fn run_assemble_report_data(
    package_root: &Path,
    dispatch: &PickedDispatch,
    schemas: &BTreeMap<String, ResultSchema>,
    clock: &dyn Clock,
) -> anyhow::Result<TaskState> {
    let task_id = dispatch.task_id.as_str();

    let (state, result_json) = match assemble_report_data(package_root, schemas, clock) {
        Ok(report) => {
            let n = report.artifacts.len();
            let result = serde_json::json!({
                "status": "completed",
                "builtin": ASSEMBLE_REPORT_DATA,
                "report_data": "runtime/outputs/reporting/report-data.json",
                "n_artifacts": n,
                "summary": format!("assembled report-data.json from {n} result artifact(s)"),
            });
            (TaskState::Completed { result: result.clone() }, result)
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

    let task_dir = package_root.join("runtime").join("outputs").join(task_id);
    std::fs::create_dir_all(&task_dir)
        .map_err(|e| anyhow::anyhow!("creating {}: {e}", task_dir.display()))?;

    // result.json — the reliable deliverable a normal agent always writes.
    // Feeds status_reconciliation's completed-detection and the patch-merge
    // recovery path.
    let result_pretty = serde_json::to_string_pretty(&result_json)
        .map_err(|e| anyhow::anyhow!("serializing result.json for {task_id}: {e}"))?;
    std::fs::write(task_dir.join("result.json"), result_pretty)
        .map_err(|e| anyhow::anyhow!("writing result.json for {task_id}: {e}"))?;

    // state.patch.json — carries the dispatch identity so the strict merge
    // accepts it exactly as it would a real agent's patch.
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

    // Refresh the heartbeat so no stall/orphan watchdog false-fires between
    // the in-process run and the patch merge. Best-effort (the patch is the
    // load-bearing signal).
    let hb = task_dir.join(".heartbeat");
    let _ = std::fs::write(&hb, ecaa_workflow_core::time_helpers::now_rfc3339());

    Ok(state)
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
}
