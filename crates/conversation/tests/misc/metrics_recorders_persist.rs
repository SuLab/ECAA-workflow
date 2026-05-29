//! Regression test for every `record_*` method on
//! `MetricsStore` must persist its mutation to the sidecar file so the
//! counter survives a server restart.
//!
//! Earlier, a subset of recorders mutated the in-memory map
//! without calling `persist_one`. After a restart the rehydrated
//! sidecar was missing those counters and the UI's Performance tab
//! silently under-reported.
//!
//! Test pattern (per recorder):
//! 1. Open a `MetricsStore::with_persist_dir(tmp)`.
//! 2. Call the recorder.
//! 3. Drop the store (server restart simulation).
//! 4. Re-open at the same dir; either snapshot via the public surface
//!    or read the sidecar JSON directly when the counter isn't on
//!    `SessionMetrics`.
//! 5. Assert the counter survived.

use ecaa_workflow_conversation::{MetricsStore, SessionId};
use std::path::Path;
use tempfile::TempDir;
use uuid::Uuid;

/// Read the on-disk sidecar JSON for a session and return it as a
/// `serde_json::Value`. Used for counters that don't surface on
/// `SessionMetrics` (e.g. iteration telemetry).
fn read_sidecar(dir: &Path, id: SessionId) -> serde_json::Value {
    let path = dir.join(format!("{}.metrics.json", id));
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("sidecar missing for {} at {:?}: {}", id, path, e));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("sidecar at {:?} not valid JSON: {}", path, e))
}

#[tokio::test]
async fn record_high_water_exceeded_persists() {
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    store.record_high_water_exceeded(id).await;
    store.record_high_water_exceeded(id).await;
    drop(store);

    let store2 = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let snap = store2
        .snapshot(id)
        .await
        .expect("snapshot after rehydrate must succeed");
    assert_eq!(
        snap.high_water_exceeded_count, 2,
        "high_water_exceeded_count lost across restart"
    );
}

#[tokio::test]
async fn record_batch_dropped_persists() {
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    store.record_batch_dropped(id, 5).await;
    store.record_batch_dropped(id, 3).await;
    drop(store);

    let store2 = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let snap = store2
        .snapshot(id)
        .await
        .expect("snapshot after rehydrate must succeed");
    assert_eq!(
        snap.batch_dropped_events, 8,
        "batch_dropped_events lost across restart"
    );
}

#[tokio::test]
async fn record_tool_loop_iterations_persists() {
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    store.record_tool_loop_iterations(id, 3).await;
    store.record_tool_loop_iterations(id, 3).await;
    store.record_tool_loop_iterations(id, 7).await;
    drop(store);

    let store2 = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let snap = store2
        .snapshot(id)
        .await
        .expect("snapshot after rehydrate must succeed");
    assert_eq!(
        snap.tool_loop_iterations_histogram.get(&3),
        Some(&2),
        "tool_loop_iterations_histogram[3] lost across restart"
    );
    assert_eq!(
        snap.tool_loop_iterations_histogram.get(&7),
        Some(&1),
        "tool_loop_iterations_histogram[7] lost across restart"
    );
}

#[tokio::test]
async fn record_composer_run_persists() {
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    store.record_composer_run(id, 120, 50, 3, 1).await;
    store.record_composer_run(id, 80, 30, 2, 0).await;
    drop(store);

    let store2 = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let snap = store2
        .snapshot(id)
        .await
        .expect("snapshot after rehydrate must succeed");
    assert_eq!(snap.composer_runs, 2, "composer_runs lost across restart");
    assert_eq!(
        snap.composer_total_duration_ms, 200,
        "composer_total_duration_ms lost across restart"
    );
    assert_eq!(
        snap.composer_atoms_considered, 80,
        "composer_atoms_considered lost across restart"
    );
    assert_eq!(
        snap.composer_backtracks, 5,
        "composer_backtracks lost across restart"
    );
    assert_eq!(
        snap.composer_exclusion_hits, 1,
        "composer_exclusion_hits lost across restart"
    );
}

#[tokio::test]
async fn record_iteration_run_persists() {
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    store.record_iteration_run(id, 100).await;
    store.record_iteration_run(id, 250).await;
    drop(store);

    let val = read_sidecar(tmp.path(), id);
    assert_eq!(
        val["iterations_run_total"].as_u64(),
        Some(2),
        "iterations_run_total lost across restart: sidecar = {:#?}",
        val
    );
    assert_eq!(
        val["iteration_total_duration_ms"].as_u64(),
        Some(350),
        "iteration_total_duration_ms lost across restart: sidecar = {:#?}",
        val
    );
}

#[tokio::test]
async fn record_iteration_converged_persists() {
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    // 1 → bucket 0; 2 → bucket 1; 5 → bucket 2.
    store.record_iteration_converged(id, 1).await;
    store.record_iteration_converged(id, 2).await;
    store.record_iteration_converged(id, 5).await;
    drop(store);

    let val = read_sidecar(tmp.path(), id);
    let buckets = val["converged_at_iter"]
        .as_array()
        .expect("converged_at_iter must be an array");
    assert_eq!(
        buckets.first().and_then(|v| v.as_u64()),
        Some(1),
        "converged_at_iter[0] lost across restart: sidecar = {:#?}",
        val
    );
    assert_eq!(
        buckets.get(1).and_then(|v| v.as_u64()),
        Some(1),
        "converged_at_iter[1] lost across restart: sidecar = {:#?}",
        val
    );
    assert_eq!(
        buckets.get(2).and_then(|v| v.as_u64()),
        Some(1),
        "converged_at_iter[2] lost across restart: sidecar = {:#?}",
        val
    );
}

#[tokio::test]
async fn record_iteration_max_hit_persists() {
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    store.record_iteration_max_hit(id).await;
    store.record_iteration_max_hit(id).await;
    store.record_iteration_max_hit(id).await;
    drop(store);

    let val = read_sidecar(tmp.path(), id);
    assert_eq!(
        val["max_iterations_hit_count"].as_u64(),
        Some(3),
        "max_iterations_hit_count lost across restart: sidecar = {:#?}",
        val
    );
}

#[tokio::test]
async fn record_opus_escalation_persists() {
    use ecaa_workflow_conversation::model_policy::EscalationReason;
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    store
        .record_opus_escalation(id, EscalationReason::Blocked)
        .await;
    store
        .record_opus_escalation(id, EscalationReason::Blocked)
        .await;
    store
        .record_opus_escalation(id, EscalationReason::CarefulMode)
        .await;
    drop(store);

    let store2 = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let snap = store2
        .snapshot(id)
        .await
        .expect("snapshot after rehydrate must succeed");
    assert_eq!(
        snap.opus_escalation_reasons.get("blocked"),
        Some(&2),
        "opus_escalation_reasons[blocked] lost across restart"
    );
    assert_eq!(
        snap.opus_escalation_reasons.get("careful_mode"),
        Some(&1),
        "opus_escalation_reasons[careful_mode] lost across restart"
    );
}

#[tokio::test]
async fn record_task_started_persists() {
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    store
        .record_task_started(id, "task_a", "m5.large", 1_000_000)
        .await;
    drop(store);

    let val = read_sidecar(tmp.path(), id);
    let starts = val["running_task_starts"]
        .as_object()
        .expect("running_task_starts must be an object");
    assert!(
        starts.contains_key("task_a"),
        "running_task_starts['task_a'] lost across restart: sidecar = {:#?}",
        val
    );
    // The per-task entry also gets stamped with started_at_ms.
    let per_task = val["per_task_agent"]
        .as_object()
        .expect("per_task_agent must be an object");
    let task_a = per_task
        .get("task_a")
        .expect("per_task_agent['task_a'] missing");
    assert_eq!(
        task_a["started_at_ms"].as_u64(),
        Some(1_000_000),
        "started_at_ms lost across restart: sidecar = {:#?}",
        val
    );
}

#[tokio::test]
async fn record_task_started_local_persists() {
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    store
        .record_task_started_local(id, "task_b", 2_500_000)
        .await;
    drop(store);

    let val = read_sidecar(tmp.path(), id);
    let per_task = val["per_task_agent"]
        .as_object()
        .expect("per_task_agent must be an object");
    let task_b = per_task
        .get("task_b")
        .expect("per_task_agent['task_b'] missing");
    assert_eq!(
        task_b["started_at_ms"].as_u64(),
        Some(2_500_000),
        "local started_at_ms lost across restart: sidecar = {:#?}",
        val
    );
}

#[tokio::test]
async fn record_task_completed_persists() {
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    store
        .record_task_started(id, "task_c", "c5.xlarge", 1_000_000)
        .await;
    store.record_task_completed(id, "task_c", 1_005_000).await;
    drop(store);

    let store2 = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let snap = store2
        .snapshot(id)
        .await
        .expect("snapshot after rehydrate must succeed");
    assert_eq!(
        snap.instance_type_seconds.get("c5.xlarge"),
        Some(&5),
        "instance_type_seconds['c5.xlarge'] lost across restart"
    );
}

// Sanity guard for the recorders the audit said were *already* persisting,
// to detect regressions if a future refactor strips the persist call.

#[tokio::test]
async fn record_blocker_entered_persists() {
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    store
        .record_blocker_entered(id, "ToolError".to_string())
        .await;
    drop(store);

    let store2 = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let snap = store2
        .snapshot(id)
        .await
        .expect("snapshot after rehydrate must succeed");
    assert_eq!(
        snap.blockers_encountered.len(),
        1,
        "blocker_entered record lost across restart"
    );
    assert_eq!(snap.blockers_encountered[0].blocker_kind, "ToolError");
}

#[tokio::test]
async fn record_blocker_recovered_persists() {
    let tmp = TempDir::new().unwrap();
    let store = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let id: SessionId = Uuid::new_v4();
    store
        .record_blocker_entered(id, "ToolError".to_string())
        .await;
    store
        .record_blocker_recovered(id, Some("rerun_task".to_string()))
        .await;
    drop(store);

    let store2 = MetricsStore::new().with_persist_dir(tmp.path().to_path_buf());
    let snap = store2
        .snapshot(id)
        .await
        .expect("snapshot after rehydrate must succeed");
    assert_eq!(snap.blockers_encountered.len(), 1);
    assert!(
        snap.blockers_encountered[0].recovered,
        "blocker_recovered flag lost across restart"
    );
    assert_eq!(
        snap.blockers_encountered[0].recovery_path.as_deref(),
        Some("rerun_task")
    );
}
