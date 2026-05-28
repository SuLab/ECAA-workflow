//! Integration test: watchdog fires `WallClockExceeded` when the
//! dispatch WAL's `timeout_at` is in the past.
//!
//! Covers test-scaffolding item 3.5 (WallClockExceeded firing).
//!
//! Design notes:
//!
//! * The public `Watchdog::spawn` API is the sole entry point — the
//!   internal `run_watchdog_loop` and the `#[cfg(test)]` `poll_once`
//!   helper inside `watchdog.rs` are both private and inaccessible from
//!   integration tests.
//!
//! * Clamping is only enforced by `WatchdogConfig::from_env`; constructing
//!   `WatchdogConfig` directly bypasses the min-period guard, so we use
//!   `period_secs: 1` to keep the test fast (fires within ~2 real seconds).
//!
//! * `FrozenClock` is set to a moment 5 seconds after the dispatch WAL's
//!   `timeout_at`. This makes the watchdog's `now > timeout_at` check true
//!   on the first poll, guaranteeing `WallClockExceeded` fires without any
//!   real elapsed wall-clock budget being consumed.
//!
//! * Process killing and dispatch-slot release are responsibilities of the
//!   harness main loop (which reads from the `WatchdogEvent` channel and
//!   calls `ProgressClient::set_task_state` with `Blocked { WallClockExceeded }`).
//!   Those downstream behaviors are NOT tested here — this test is scoped to
//!   the watchdog's event-emission contract only.

use ecaa_workflow_core::clock::FrozenClock;
use ecaa_workflow_harness::watchdog::{Watchdog, WatchdogConfig, WatchdogEvent};
use serde_json::json;
use std::io::Write;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tempfile::TempDir;

/// RFC-3339 timestamps used across the test.
const TASK_STARTED_AT: &str = "2026-05-18T10:00:00Z";
/// Timeout 5 minutes after start.
const TIMEOUT_AT: &str = "2026-05-18T10:05:00Z";
/// Clock frozen 5 seconds after `TIMEOUT_AT` — guarantees timeout_exceeded.
const CLOCK_NOW: &str = "2026-05-18T10:05:05Z";

const TASK_ID: &str = "align_reads_01";

// ---- helpers ----------------------------------------------------------------

fn make_tmpdir() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// Write a minimal `WORKFLOW.json` with one Running task.
fn write_workflow_json(root: &std::path::Path, expected_wall_secs: Option<u64>) {
    let spec = expected_wall_secs.map(|s| json!({ "expected_wall_seconds": s }));
    let state = json!({ "status": "running", "started_at": TASK_STARTED_AT });
    let task_obj = json!({
        "kind": "computation",
        "state": state,
        "depends_on": [],
        "assignee": "agent",
        "description": "alignment task under test",
        "resource_class": "cpu_heavy",
        "spec": spec,
    });
    let dag = json!({
        "version": "1.0",
        "workflow_id": "wf-deadline-test",
        "tasks": { TASK_ID: task_obj }
    });
    std::fs::write(
        root.join("WORKFLOW.json"),
        serde_json::to_string(&dag).unwrap(),
    )
    .unwrap();
}

/// Write a dispatch WAL with one entry whose `timeout_at` is already past
/// (relative to `CLOCK_NOW`).
fn write_dispatch_wal(root: &std::path::Path) {
    let runtime = root.join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    // Use schema_version string the WAL reader accepts.
    let record = json!({
        "schema_version": "3.0.0",
        "task_id": TASK_ID,
        "epoch": 1,
        "harness_run_id": "test-run-deadline",
        "started_at": TASK_STARTED_AT,
        "timeout_at": TIMEOUT_AT,
    });
    let wal_path = runtime.join("dispatches.jsonl");
    let mut f = std::fs::File::create(&wal_path).unwrap();
    writeln!(f, "{}", record).unwrap();
}

/// Build a `FrozenClock` pinned to `CLOCK_NOW`.
fn frozen_clock_past_deadline() -> Arc<dyn ecaa_workflow_core::clock::Clock + Send + Sync> {
    let at: chrono::DateTime<chrono::Utc> = CLOCK_NOW.parse().expect("parse CLOCK_NOW");
    Arc::new(FrozenClock { at })
}

// ---- tests ------------------------------------------------------------------

/// Watchdog fires `WallClockExceeded` when the dispatch WAL's `timeout_at`
/// is in the past relative to the injected `FrozenClock`.
///
/// The fake agent subprocess (a `sleep 9999` that never exits on its own) is
/// not needed for this test because the watchdog only reads `WORKFLOW.json`
/// and the dispatch WAL — it does not interact with the process itself. The
/// Running task in `WORKFLOW.json` is sufficient to trigger the timeout path.
#[test]
fn watchdog_fires_wall_clock_exceeded_on_timeout_at() {
    let tmp = make_tmpdir();
    let root = tmp.path();

    write_workflow_json(root, Some(600));
    write_dispatch_wal(root);

    let (event_tx, event_rx) = mpsc::sync_channel::<WatchdogEvent>(64);

    // period_secs=1 bypasses the `from_env` minimum-clamp (10s) so the
    // watchdog fires within ~2 real seconds, keeping the test fast.
    let config = WatchdogConfig {
        period_secs: 1,
        multiplier: 6.0,
    };
    let clock = frozen_clock_past_deadline();
    let mut watchdog = Watchdog::spawn(root.to_path_buf(), clock, config, event_tx);

    // Wait up to 5 seconds for the WallClockExceeded event.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut wall_exceeded: Option<WatchdogEvent> = None;

    while Instant::now() < deadline {
        match event_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ev @ WatchdogEvent::WallClockExceeded { .. }) => {
                wall_exceeded = Some(ev);
                break;
            }
            Ok(WatchdogEvent::HeartbeatAge { .. }) => {
                // Heartbeat events may arrive; ignore them.
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    watchdog.stop();

    let event = wall_exceeded
        .expect("watchdog must emit WallClockExceeded within 5s for a task past its timeout_at");

    match event {
        WatchdogEvent::WallClockExceeded {
            task_id,
            observed_secs,
            threshold_secs,
        } => {
            assert_eq!(task_id, TASK_ID, "event must name the timed-out task");
            // observed_secs = now − started_at = 305 s (10:05:05 − 10:00:00).
            assert_eq!(
                observed_secs, 305,
                "observed_secs must equal wall age at CLOCK_NOW"
            );
            // threshold_secs = timeout_at − started_at = 300 s (5 min).
            // The watchdog reports the timeout_at duration when timeout_exceeded
            // is true and age_exceeded is false.
            assert_eq!(
                threshold_secs, 300,
                "threshold_secs must equal the timeout_at duration when fired via timeout_at path"
            );
        }
        other => panic!("expected WallClockExceeded, got {other:?}"),
    }
}

/// Downstream de-dup contract: process killing and dispatch-slot release are
/// NOT watchdog responsibilities — they belong to the harness main loop.
///
/// The watchdog emits `WatchdogEvent::WallClockExceeded` on its channel. The
/// main loop dequeues that event and calls
/// `ProgressClient::set_task_state(task_id, Blocked { WallClockExceeded })`.
/// The server's `Session::set_task_state` is monotonic (a second attempt to
/// transition an already-Blocked task is refused), so duplicate signals from
/// repeated watchdog polls are naturally de-duplicated at the server boundary.
///
/// This test confirms the event is emitted exactly once per poll cycle (not
/// batched or de-duped inside the watchdog itself), matching the stated design
/// that de-duplication lives server-side.
#[test]
fn watchdog_emits_per_poll_not_deduplicated_internally() {
    let tmp = make_tmpdir();
    let root = tmp.path();

    write_workflow_json(root, Some(600));
    write_dispatch_wal(root);

    // Use a large channel so we can count all events over two poll periods.
    let (event_tx, event_rx) = mpsc::sync_channel::<WatchdogEvent>(128);

    let config = WatchdogConfig {
        period_secs: 1,
        multiplier: 6.0,
    };
    let clock = frozen_clock_past_deadline();
    let mut watchdog = Watchdog::spawn(root.to_path_buf(), clock, config, event_tx);

    // Collect events for 3 seconds — should get at least 2 poll cycles.
    std::thread::sleep(Duration::from_secs(3));
    watchdog.stop();

    let events: Vec<_> = event_rx.try_iter().collect();
    let exceeded_count = events
        .iter()
        .filter(
            |e| matches!(e, WatchdogEvent::WallClockExceeded { task_id, .. } if task_id == TASK_ID),
        )
        .count();

    // The watchdog fires on EVERY poll while the condition holds — de-dup
    // is server-side, not watchdog-side. Two or more events expected across
    // 3 s at 1 s period.
    assert!(
        exceeded_count >= 2,
        "expected ≥2 WallClockExceeded events over 3s at 1s poll period (de-dup is server-side); got {exceeded_count}"
    );
}
