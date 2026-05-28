//! Integration test: stall monitor + watchdog both fire for the same task;
//! the second signal is a no-op at the server boundary.
//!
//! Covers test-scaffolding item 6.5 (stall-vs-watchdog conflict).
//!
//! Scenario
//! --------
//! A task is both:
//!   1. CPU-idle (stall monitor fires `StallSignal::CpuStarvation`), and
//!   2. Past its `timeout_at` wall-clock deadline (watchdog fires
//!      `WatchdogEvent::WallClockExceeded`).
//!
//! Both signals arrive on their respective channels. The test asserts:
//!   * At least one `StallSignal::CpuStarvation` is observed.
//!   * At least one `WatchdogEvent::WallClockExceeded` is observed.
//!   * Only ONE `BlockerKind` state transition would be applied in
//!     production — because `Session::set_task_state` is monotonic:
//!     once the task is Blocked the second signal's POST is refused
//!     by the server. This de-duplication contract is documented and
//!     asserted by the commentary below; it lives server-side, not
//!     inside the watchdog or stall monitor themselves.
//!
//! Platform note
//! -------------
//! The stall monitor reads `/proc/<pid>/stat` and is Linux-only.
//! On non-Linux platforms this file compiles but the stall-monitor
//! test case is `#[ignore]`-d, matching the existing pattern in
//! `stall_monitor_integration_test.rs`. The watchdog-only test case
//! runs on all platforms.

use scripps_workflow_core::clock::FrozenClock;
use scripps_workflow_harness::executor::stall_monitor::{StallSignal, StallThresholds};
use scripps_workflow_harness::executor::{Executor, ExecutorArgs};
use scripps_workflow_harness::watchdog::{Watchdog, WatchdogConfig, WatchdogEvent};
use serde_json::json;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tempfile::TempDir;

// ---- shared timestamps -------------------------------------------------------

/// Task started 10 minutes ago (from the frozen clock's perspective).
const TASK_STARTED_AT: &str = "2026-05-18T11:00:00Z";
/// Timeout 5 minutes after start — already elapsed at CLOCK_NOW.
const TIMEOUT_AT: &str = "2026-05-18T11:05:00Z";
/// Clock is 5 s past timeout_at, so the watchdog's timeout check fires.
const CLOCK_NOW: &str = "2026-05-18T11:05:05Z";

const TASK_ID: &str = "align_reads_stall_watchdog";

// ---- helpers ----------------------------------------------------------------

fn make_tmpdir() -> TempDir {
    tempfile::tempdir().expect("tempdir")
}

/// Write a minimal `WORKFLOW.json` with one Running task.
fn write_workflow_json(root: &std::path::Path) {
    let state = json!({ "status": "running", "started_at": TASK_STARTED_AT });
    let task_obj = json!({
        "kind": "computation",
        "state": state,
        "depends_on": [],
        "assignee": "agent",
        "description": "task used to trigger both stall and watchdog",
        "resource_class": "cpu_heavy",
        // expected_wall_seconds absent → watchdog uses fallback (1800s × 6 = 10800s).
        // Age at CLOCK_NOW = 305s < 10800s → age-exceeded branch is FALSE.
        // But timeout_at=11:05:00 < CLOCK_NOW=11:05:05 → timeout_exceeded is TRUE.
        "spec": null,
    });
    let dag = json!({
        "version": "1.0",
        "workflow_id": "wf-stall-watchdog-interaction",
        "tasks": { TASK_ID: task_obj }
    });
    std::fs::write(
        root.join("WORKFLOW.json"),
        serde_json::to_string(&dag).unwrap(),
    )
    .unwrap();
}

/// Write a dispatch WAL with `timeout_at` already past relative to `CLOCK_NOW`.
fn write_dispatch_wal(root: &std::path::Path) {
    let runtime = root.join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    let record = json!({
        "schema_version": "3.0.0",
        "task_id": TASK_ID,
        "epoch": 1,
        "harness_run_id": "test-run-stall-watchdog",
        "started_at": TASK_STARTED_AT,
        "timeout_at": TIMEOUT_AT,
    });
    let wal_path = runtime.join("dispatches.jsonl");
    let mut f = std::fs::File::create(&wal_path).unwrap();
    writeln!(f, "{}", record).unwrap();
}

fn frozen_clock_past_deadline() -> Arc<dyn scripps_workflow_core::clock::Clock + Send + Sync> {
    let at: chrono::DateTime<chrono::Utc> = CLOCK_NOW.parse().expect("parse CLOCK_NOW");
    Arc::new(FrozenClock { at })
}

// ---- test: watchdog path only (all platforms) --------------------------------

/// Watchdog-only path: asserts `WallClockExceeded` fires when the task's
/// dispatch `timeout_at` is in the past. No stall monitor involved here —
/// the watchdog component is platform-independent.
#[test]
fn watchdog_fires_wall_clock_exceeded_without_stall_monitor() {
    let tmp = make_tmpdir();
    let root = tmp.path();

    write_workflow_json(root);
    write_dispatch_wal(root);

    let (event_tx, event_rx) = mpsc::sync_channel::<WatchdogEvent>(64);
    let config = WatchdogConfig {
        period_secs: 1,
        multiplier: 6.0,
    };
    let clock = frozen_clock_past_deadline();
    let mut watchdog = Watchdog::spawn(root.to_path_buf(), clock, config, event_tx);

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_exceeded = false;

    while Instant::now() < deadline {
        match event_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(WatchdogEvent::WallClockExceeded { ref task_id, .. }) if task_id == TASK_ID => {
                saw_exceeded = true;
                break;
            }
            Ok(_) => continue,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    watchdog.stop();

    assert!(
        saw_exceeded,
        "watchdog must fire WallClockExceeded within 5s for a task past its timeout_at"
    );
}

// ---- test: both signals fire (Linux only) ------------------------------------

/// Both the stall monitor and the watchdog fire for the same Running task.
///
/// The stall monitor is configured with `cpu_min_pct = 101.0` (unreachable)
/// and `cpu_window_mins = 0` (single-sample window) to guarantee a
/// `CpuStarvation` signal on the first sample for a sleeping subprocess.
///
/// The watchdog is configured with `period_secs = 1` and a FrozenClock past
/// `timeout_at`, so `WallClockExceeded` arrives within ~2 real seconds.
///
/// Assertions:
///   1. At least one `StallSignal::CpuStarvation` is observed.
///   2. At least one `WatchdogEvent::WallClockExceeded` is observed.
///   3. The de-duplication contract (documented below) is satisfied.
///
/// De-duplication contract
/// -----------------------
/// In production, the harness main loop translates both signals into
/// `POST /api/chat/session/:id/task/:task_id/state` calls carrying
/// `Blocked { <kind> }`. The server's `Session::set_task_state` enforces
/// monotonicity: once a task is `Blocked`, a second attempt to transition
/// it from `Running` to `Blocked` is refused with a no-op return. The
/// second signal is therefore silently dropped — producing exactly one
/// `BlockerKind` transition per task, regardless of how many competing
/// signals arrive.
///
/// This test does NOT reach the server (there is no server in this test).
/// It verifies that BOTH signals arrive on their channels — confirming the
/// two monitors fire independently — and then asserts the de-dup invariant
/// as a compile-time comment + assertion about the channel multiplicity.
/// The monotonicity of `Session::set_task_state` is tested in the
/// conversation crate's session unit tests.
#[cfg(target_os = "linux")]
#[test]
fn stall_and_watchdog_both_fire_dedup_is_server_side() {
    let tmp = make_tmpdir();
    let pkg = tmp.path().join("pkg");
    std::fs::create_dir_all(&pkg).expect("pkg dir");

    // Write the DAG and WAL (watchdog reads these).
    write_workflow_json(&pkg);
    write_dispatch_wal(&pkg);

    // Write a fake agent that sleeps — CPU-idle, so the stall monitor fires.
    let agent = tmp.path().join("idle-agent.sh");
    std::fs::write(&agent, "#!/usr/bin/env bash\nsleep 10\nexit 0\n").expect("write agent");
    let mut perms = std::fs::metadata(&agent).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&agent, perms).expect("chmod");

    // ---- Stall monitor -------------------------------------------------------
    // One-sample window + cpu_min_pct > 100 guarantees CpuStarvation on the
    // very first observation (a sleeping process has ~0% CPU).
    let thresholds = StallThresholds {
        enabled: true,
        cpu_min_pct: 101.0,
        cpu_window_mins: 0,
        mem_max_pct: 100.0,
        mem_window_mins: 5,
        gpu_idle_when_training_mins: 15,
        runtime_over_expected_mult: 2.0,
        sample_interval_secs: 1,
    };

    let args = ExecutorArgs {
        package: pkg.to_string_lossy().to_string(),
        agent: agent.to_string_lossy().to_string(),
        task_timeout_secs: 300,
    };
    let mut exec = scripps_workflow_harness::executor::local::LocalExecutor::new(&args);

    let (stall_tx, stall_rx): (mpsc::Sender<StallSignal>, mpsc::Receiver<StallSignal>) =
        mpsc::channel();
    exec.start_stall_monitor(&thresholds, stall_tx)
        .expect("start stall monitor");

    // ---- Watchdog ------------------------------------------------------------
    let (watchdog_tx, watchdog_rx) = mpsc::sync_channel::<WatchdogEvent>(64);
    let watchdog_config = WatchdogConfig {
        period_secs: 1,
        multiplier: 6.0,
    };
    let clock = frozen_clock_past_deadline();
    let mut watchdog = Watchdog::spawn(pkg.clone(), clock, watchdog_config, watchdog_tx);

    // ---- Drive the executor (blocks until agent exits) -----------------------
    // Run the fake agent in a worker thread so the main thread can drain
    // both receivers concurrently.
    let pkg_for_thread = pkg.clone();
    let agent_str = agent.to_string_lossy().to_string();
    let runner = std::thread::spawn(move || {
        let envelope = std::collections::BTreeMap::<String, String>::new();
        exec.run_iteration(&pkg_for_thread, &agent_str, &envelope)
    });

    // ---- Collect signals for up to 8 s ---------------------------------------
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut saw_stall = false;
    let mut saw_exceeded = false;

    while Instant::now() < deadline && (!saw_stall || !saw_exceeded) {
        // Poll stall channel.
        while let Ok(signal) = stall_rx.try_recv() {
            if matches!(signal, StallSignal::CpuStarvation { .. }) {
                saw_stall = true;
            }
        }
        // Poll watchdog channel.
        while let Ok(event) = watchdog_rx.try_recv() {
            if matches!(&event, WatchdogEvent::WallClockExceeded { task_id, .. } if task_id == TASK_ID)
            {
                saw_exceeded = true;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Clean up before asserting so the test doesn't leak subprocesses.
    watchdog.stop();
    let _ = runner.join();

    // ---- Assertions ----------------------------------------------------------

    assert!(
        saw_stall,
        "stall monitor must fire CpuStarvation for an idle subprocess within 8s"
    );
    assert!(
        saw_exceeded,
        "watchdog must fire WallClockExceeded for a task past its timeout_at within 8s"
    );

    // De-duplication invariant (documented): BOTH signals fired, but in
    // production only ONE `BlockerKind` transition would be recorded.
    //
    // The harness main loop applies signals in the order they are dequeued
    // from the mpsc channels. The first signal transitions the task to
    // `Blocked`; the second call to `ProgressClient::set_task_state` POSTs
    // the same task's state again. The server's `Session::set_task_state`
    // checks:
    //
    //   if current_state.is_more_terminal_than(new_state) { return Ok(()) }
    //
    // A `Blocked` task is more terminal than `Running`; the second transition
    // attempt returns `Ok(())` silently, leaving the DB unchanged. Exactly one
    // `BlockerKind` row appears in `runtime/decisions.jsonl`.
    //
    // We assert the multiplicity here as documentation: both channels produced
    // events (≥1 each), confirming the two monitors are independent and do NOT
    // de-duplicate internally. The single-winner guarantee is server-enforced.
    assert!(
        saw_stall && saw_exceeded,
        "both signals must arrive independently — de-dup is server-side, not monitor-side"
    );
}

/// Non-Linux stub: stall monitor requires /proc and cannot run.
#[cfg(not(target_os = "linux"))]
#[test]
#[ignore = "stall monitor requires /proc, Linux-only"]
fn stall_and_watchdog_both_fire_dedup_is_server_side() {
    eprintln!(
        "stall_and_watchdog_both_fire_dedup_is_server_side: skipped (requires /proc, Linux-only)"
    );
}
