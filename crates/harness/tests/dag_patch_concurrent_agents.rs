//! Integration test for `dag_patch::apply_pending_patches_strict` under
//! concurrent agents racing to write `state.patch.json` files.
//!
//! Gap from §10.3 (systematic gap #1) and §8.3: the strict-mode
//! `(harness_run_id, dispatch_epoch)` identity match is the protocol's
//! load-bearing invariant, but is exercised only in single-threaded unit
//! tests. This test spawns 3 worker threads that each write distinct patch
//! files to the same package directory, then asserts that
//! `apply_pending_patches_strict` accepts only the patches matching the
//! "current" dispatch tokens and deterministically skips stale/mismatched
//! ones.
//!
//! Why this covers a real gap: under the old WORKFLOW.json direct-write
//! protocol two parallel agents would race on a single file (last writer
//! wins). The patch-file protocol is "a per-task lock by construction" —
//! each agent only touches its own task's output directory. The strict-mode
//! (run_id, epoch) guard is the second layer: it stops a patch from a
//! *previous* harness run (same task, different tokens) from being applied
//! by the new run. This test verifies that layer survives concurrent writes.

use scripps_workflow_core::dag::{Assignee, ResourceClass, Task, TaskId, TaskKind, TaskState, DAG};
use scripps_workflow_harness::dag_patch::{apply_pending_patches_strict, PickedDispatch};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Barrier};

// ── fixture helpers ──────────────────────────────────────────────────────────

fn make_running_task(task_id: &str) -> Task {
    Task {
        kind: TaskKind::Computation,
        state: TaskState::Running {
            started_at: "2026-01-01T00:00:00Z".into(),
            remote: None,
        },
        depends_on: vec![],
        assignee: Assignee::Agent,
        description: format!("test task {}", task_id),
        spec: None,
        resolution: None,
        result_ref: None,
        resource_class: ResourceClass::CpuHeavy,
        requires_sme_review: false,
        required_artifacts: vec![],
        container: None,
        source_atom_id: None,
        safety: Default::default(),
    }
}

/// Build a DAG with the given running tasks and write it to
/// `<dir>/WORKFLOW.json`.
fn seed_workflow(dir: &Path, task_ids: &[&str]) {
    let mut tasks = BTreeMap::new();
    for &tid in task_ids {
        tasks.insert(TaskId::from(tid), make_running_task(tid));
    }
    let dag = DAG {
        version: "1".into(),
        schema_version: scripps_workflow_core::dag::current_dag_schema_version(),
        workflow_id: "concurrent-agents-test".into(),
        current_task: None,
        tasks,
        reverse_deps: BTreeMap::new(),
        run_id: None,
    };
    std::fs::write(
        dir.join("WORKFLOW.json"),
        serde_json::to_string_pretty(&dag).unwrap(),
    )
    .unwrap();
}

/// Write `runtime/outputs/<task_id>/state.patch.json` with the given JSON
/// body.
fn write_patch(dir: &Path, task_id: &str, patch: &serde_json::Value) {
    let out = dir.join("runtime/outputs").join(task_id);
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(
        out.join("state.patch.json"),
        serde_json::to_string_pretty(patch).unwrap(),
    )
    .unwrap();
}

// ── tests ────────────────────────────────────────────────────────────────────

/// Three concurrent workers each write a distinct `state.patch.json` for
/// their own task. All patches carry the correct (run_id, epoch) token.
/// Every patch must be applied; the resulting WORKFLOW.json must parse
/// cleanly and reflect the expected transitions.
#[test]
fn all_matching_patches_applied_under_concurrent_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();

    let task_ids: &[&str] = &["task_alpha", "task_beta", "task_gamma"];
    seed_workflow(&dir, task_ids);

    let current_run_id = "run-concurrent-all-match";
    let dispatches: Vec<PickedDispatch> = task_ids
        .iter()
        .enumerate()
        .map(|(i, &tid)| PickedDispatch {
            task_id: TaskId::from(tid),
            harness_run_id: current_run_id.to_string(),
            epoch: (i as u64) + 1,
        })
        .collect();

    // Barrier ensures all 3 threads start writing simultaneously.
    let barrier = Arc::new(Barrier::new(task_ids.len()));
    let dir_arc = Arc::new(dir.clone());

    let handles: Vec<_> = dispatches
        .iter()
        .map(|d| {
            let dir2 = Arc::clone(&dir_arc);
            let tid = d.task_id.clone();
            let run_id = d.harness_run_id.clone();
            let epoch = d.epoch;
            let b = Arc::clone(&barrier);
            std::thread::spawn(move || {
                b.wait(); // synchronize start
                let patch = serde_json::json!({
                    "from": "running",
                    "harness_run_id": run_id,
                    "dispatch_epoch": epoch,
                    "to": {
                        "status": "completed",
                        "result": { "task": tid, "epoch": epoch }
                    }
                });
                write_patch(&dir2, tid.as_str(), &patch);
            })
        })
        .collect();

    for h in handles {
        h.join().expect("worker thread panicked");
    }

    let merged = apply_pending_patches_strict(&dir, &dispatches)
        .expect("apply_pending_patches_strict must succeed");

    // Every task must be Completed.
    for (i, &tid) in task_ids.iter().enumerate() {
        let epoch = (i as u64) + 1;
        match &merged.tasks.get(tid).expect("task must be in DAG").state {
            TaskState::Completed { result } => {
                assert_eq!(result["task"], tid, "task {} result mismatch", tid);
                assert_eq!(result["epoch"], epoch, "task {} epoch mismatch", tid);
            }
            other => panic!("task {} expected Completed, got {:?}", tid, other),
        }
    }

    // WORKFLOW.json must parse cleanly — no torn write.
    let raw = std::fs::read_to_string(dir.join("WORKFLOW.json"))
        .expect("WORKFLOW.json must exist after strict merge");
    let reparsed: serde_json::Value =
        serde_json::from_str(&raw).expect("WORKFLOW.json must be valid JSON after strict merge");
    assert!(
        reparsed.get("tasks").is_some(),
        "reparsed WORKFLOW.json must have a tasks key"
    );

    // Patch files must be consumed (renamed to .applied).
    for &tid in task_ids {
        let out = dir.join("runtime/outputs").join(tid);
        assert!(
            !out.join("state.patch.json").exists(),
            "state.patch.json for {} must be consumed after apply",
            tid
        );
        assert!(
            out.join("state.patch.applied.json").exists(),
            "state.patch.applied.json for {} must exist after consume",
            tid
        );
    }
}

/// Three concurrent workers write patches for 3 tasks. Workers for
/// task_beta and task_gamma carry stale/mismatched (run_id, epoch) tokens.
/// Only task_alpha's patch (matching current tokens) must be applied;
/// the other two must be skipped deterministically.
#[test]
fn mismatched_patches_skipped_deterministically_under_concurrent_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();

    let task_ids: &[&str] = &["task_alpha", "task_beta", "task_gamma"];
    seed_workflow(&dir, task_ids);

    let current_run_id = "run-mismatch-test";

    // Worker payloads: (task_id, harness_run_id_in_patch, epoch_in_patch,
    //                   dispatch_run_id_for_harness, dispatch_epoch_for_harness)
    // task_alpha: exact match → must be applied.
    // task_beta:  wrong run_id → must be skipped.
    // task_gamma: correct run_id but stale epoch → must be skipped.
    let worker_specs: Vec<(TaskId, &str, u64, &str, u64)> = vec![
        ("task_alpha".into(), current_run_id, 10, current_run_id, 10),
        ("task_beta".into(), "run-OLD-stale", 10, current_run_id, 10),
        ("task_gamma".into(), current_run_id, 99, current_run_id, 10),
    ];

    let barrier = Arc::new(Barrier::new(worker_specs.len()));
    let dir_arc = Arc::new(dir.clone());

    let handles: Vec<_> = worker_specs
        .iter()
        .map(|(tid, patch_run_id, patch_epoch, _, _)| {
            let dir2 = Arc::clone(&dir_arc);
            let tid2 = tid.clone();
            let prid = patch_run_id.to_string();
            let pepoch = *patch_epoch;
            let b = Arc::clone(&barrier);
            std::thread::spawn(move || {
                b.wait();
                let patch = serde_json::json!({
                    "from": "running",
                    "harness_run_id": prid,
                    "dispatch_epoch": pepoch,
                    "to": {
                        "status": "completed",
                        "result": { "written_by": tid2 }
                    }
                });
                write_patch(&dir2, tid2.as_str(), &patch);
            })
        })
        .collect();

    for h in handles {
        h.join().expect("worker thread panicked");
    }

    // Build the dispatch list with the "current" (harness-side) tokens.
    let dispatches: Vec<PickedDispatch> = worker_specs
        .iter()
        .map(
            |(tid, _, _, dispatch_run_id, dispatch_epoch)| PickedDispatch {
                task_id: tid.clone(),
                harness_run_id: dispatch_run_id.to_string(),
                epoch: *dispatch_epoch,
            },
        )
        .collect();

    let merged = apply_pending_patches_strict(&dir, &dispatches)
        .expect("apply_pending_patches_strict must not return Err on mismatch");

    // task_alpha (matching tokens): must be Completed.
    match &merged
        .tasks
        .get("task_alpha")
        .expect("task_alpha in DAG")
        .state
    {
        TaskState::Completed { .. } => {}
        other => panic!("task_alpha: expected Completed, got {:?}", other),
    }

    // task_beta (stale run_id): must remain Running.
    match &merged
        .tasks
        .get("task_beta")
        .expect("task_beta in DAG")
        .state
    {
        TaskState::Running { .. } => {}
        other => panic!("task_beta: expected Running (skipped), got {:?}", other),
    }

    // task_gamma (stale epoch): must remain Running.
    match &merged
        .tasks
        .get("task_gamma")
        .expect("task_gamma in DAG")
        .state
    {
        TaskState::Running { .. } => {}
        other => panic!("task_gamma: expected Running (skipped), got {:?}", other),
    }

    // The stale patches must still be on disk (not consumed).
    for &tid in &["task_beta", "task_gamma"] {
        assert!(
            dir.join("runtime/outputs")
                .join(tid)
                .join("state.patch.json")
                .exists(),
            "skipped patch for {} must remain on disk",
            tid
        );
    }

    // The applied patch must be consumed.
    assert!(
        !dir.join("runtime/outputs/task_alpha/state.patch.json")
            .exists(),
        "applied patch for task_alpha must be consumed"
    );
    assert!(
        dir.join("runtime/outputs/task_alpha/state.patch.applied.json")
            .exists(),
        "applied patch for task_alpha must be renamed to .applied"
    );

    // WORKFLOW.json must parse cleanly after the partial merge.
    let raw = std::fs::read_to_string(dir.join("WORKFLOW.json")).expect("WORKFLOW.json must exist");
    serde_json::from_str::<serde_json::Value>(&raw)
        .expect("WORKFLOW.json must be valid JSON after partial strict merge");
}

/// Three workers write for 3 distinct tasks simultaneously; all carry
/// matching tokens.  A fourth worker concurrently writes a *malformed*
/// patch for a fourth task. The merge must succeed for the 3 valid tasks,
/// skip the malformed patch, and leave WORKFLOW.json in a clean parseable
/// state.
#[test]
fn malformed_patch_among_valid_patches_does_not_corrupt_workflow_json() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();

    let all_task_ids: &[&str] = &["task_a", "task_b", "task_c", "task_bad"];
    seed_workflow(&dir, all_task_ids);

    let current_run_id = "run-malform-test";

    let barrier = Arc::new(Barrier::new(all_task_ids.len()));
    let dir_arc = Arc::new(dir.clone());

    // Spawn 3 valid workers + 1 malformed writer.
    let mut handles = Vec::new();
    for (idx, &tid) in all_task_ids.iter().enumerate() {
        let dir2 = Arc::clone(&dir_arc);
        let tid_owned = tid.to_string();
        let run_id_owned = current_run_id.to_string();
        let b = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            b.wait();
            let out = dir2.join("runtime/outputs").join(&tid_owned);
            std::fs::create_dir_all(&out).unwrap();
            if tid_owned == "task_bad" {
                // Write syntactically invalid JSON.
                std::fs::write(out.join("state.patch.json"), b"{ NOT VALID JSON !!!").unwrap();
            } else {
                let epoch = (idx as u64) + 1;
                let patch = serde_json::json!({
                    "from": "running",
                    "harness_run_id": run_id_owned,
                    "dispatch_epoch": epoch,
                    "to": {
                        "status": "completed",
                        "result": { "idx": idx }
                    }
                });
                std::fs::write(
                    out.join("state.patch.json"),
                    serde_json::to_string_pretty(&patch).unwrap(),
                )
                .unwrap();
            }
        }));
    }
    for h in handles {
        h.join().expect("worker thread panicked");
    }

    // Build dispatches for all 4 tasks including the bad one.
    let dispatches: Vec<PickedDispatch> = all_task_ids
        .iter()
        .enumerate()
        .map(|(i, &tid)| PickedDispatch {
            task_id: TaskId::from(tid),
            harness_run_id: current_run_id.to_string(),
            epoch: (i as u64) + 1,
        })
        .collect();

    let merged = apply_pending_patches_strict(&dir, &dispatches)
        .expect("apply must succeed even when one patch is malformed");

    // The 3 valid tasks must be Completed.
    for &tid in &["task_a", "task_b", "task_c"] {
        match &merged.tasks.get(tid).unwrap().state {
            TaskState::Completed { .. } => {}
            other => panic!("{} expected Completed, got {:?}", tid, other),
        }
    }

    // task_bad must transition to Blocked { PatchUnparseable } so the SME
    // sees the parse failure (per ff86ef35 "quarantine malformed
    // state.patch.json"); the previous behaviour silently left it Running.
    match &merged.tasks.get("task_bad").unwrap().state {
        TaskState::Blocked { record } => {
            assert!(
                record.reason.contains("patch_unparseable"),
                "task_bad expected Blocked with patch_unparseable reason, got {:?}",
                record,
            );
        }
        other => panic!(
            "task_bad expected Blocked with patch_unparseable, got {:?}",
            other,
        ),
    }

    // Malformed patch quarantined as `state.patch.json.rejected-<ts>`
    // (per ff86ef35); the original `state.patch.json` is renamed so the
    // harness doesn't re-pick it. Either form on disk satisfies the
    // "preserved for operator inspection" invariant.
    let outputs_dir = dir.join("runtime/outputs/task_bad");
    let preserved = std::fs::read_dir(&outputs_dir)
        .ok()
        .map(|entries| {
            entries.filter_map(|e| e.ok()).any(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                name == "state.patch.json" || name.starts_with("state.patch.json.rejected-")
            })
        })
        .unwrap_or(false);
    assert!(
        preserved,
        "malformed patch must remain on disk for operator inspection \
         (either at state.patch.json or quarantined as state.patch.json.rejected-<ts>)",
    );

    // WORKFLOW.json is clean.
    let raw = std::fs::read_to_string(dir.join("WORKFLOW.json")).unwrap();
    serde_json::from_str::<serde_json::Value>(&raw)
        .expect("WORKFLOW.json must be valid JSON after merge with one malformed patch");
}
