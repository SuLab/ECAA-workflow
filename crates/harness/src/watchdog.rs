//! Wall-clock-age watchdog thread.
//!
//! Covers three tightly-coupled responsibilities (items 2.1, 2.4, 5.5 from the
//! executor-harness deep analysis):
//!
//! 1. **Wall-clock budget enforcement** — every `period` seconds, reads all
//!    `Running` tasks from `WORKFLOW.json` and checks whether
//!    `age_secs > task.expected_wall_seconds × multiplier`. If so it emits
//!    `WatchdogEvent::WallClockExceeded`.
//! 2. **`timeout_at` enforcement** — consults the dispatch WAL
//!    (`runtime/dispatches.jsonl`) for each Running task's `timeout_at`
//!    timestamp and fires the same event when `now > timeout_at`.
//! 3. **Continuous heartbeat-age SSE** — for every Running task, computes
//!    `heartbeat_age_secs` (now − heartbeat-file mtime) and emits
//!    `WatchdogEvent::HeartbeatAge` so the main loop can forward it as an SSE
//!    payload. This keeps the UI Progress tab live even when the harness has
//!    nothing else to report.
//!
//! Sync-friendly — no tokio. Uses `Clock` from `crates/core::clock` so tests
//! can inject `FrozenClock` rather than relying on wall time.

use crate::constants::{
    WATCHDOG_FALLBACK_EXPECTED_WALL_SECS, WATCHDOG_MULTIPLIER_DEFAULT, WATCHDOG_MULTIPLIER_MAX,
    WATCHDOG_MULTIPLIER_MIN, WATCHDOG_PERIOD_SECS_DEFAULT, WATCHDOG_PERIOD_SECS_MAX,
    WATCHDOG_PERIOD_SECS_MIN,
};
use crate::dispatch_wal::{read_dispatches, DispatchRecord};
use anyhow::Result;
use chrono::DateTime;
use ecaa_workflow_core::clock::Clock;
use ecaa_workflow_core::dag::{TaskState, DAG};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// An event emitted by the watchdog thread to the harness main loop.
#[derive(Debug, Clone)]
pub enum WatchdogEvent {
    /// A Running task has exceeded its wall-clock budget.
    WallClockExceeded {
        /// Affected task id.
        task_id: String,
        /// Actual wall-clock age of the task in seconds.
        observed_secs: u64,
        /// Budget threshold that was exceeded (`expected_wall_seconds × multiplier`).
        threshold_secs: u64,
    },
    /// Live heartbeat-age observation for a Running task. Carries the
    /// age in seconds so the main loop can forward it as a
    /// `heartbeat_age_secs` SSE payload on the `"task_heartbeat_age"`
    /// event kind.
    HeartbeatAge {
        /// Affected task id.
        task_id: String,
        /// Age of the heartbeat file in seconds at observation time.
        age_secs: u64,
    },
}

/// Configuration read from environment variables.
#[derive(Debug, Clone, PartialEq)]
pub struct WatchdogConfig {
    /// How often the watchdog polls `WORKFLOW.json` (seconds).
    pub period_secs: u64,
    /// Budget multiplier applied to `expected_wall_seconds`.
    pub multiplier: f64,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            period_secs: WATCHDOG_PERIOD_SECS_DEFAULT,
            multiplier: WATCHDOG_MULTIPLIER_DEFAULT,
        }
    }
}

impl WatchdogConfig {
    /// Read configuration from `ECAA_WATCHDOG_PERIOD_SECS` and
    /// `ECAA_WATCHDOG_MULTIPLIER`. Out-of-range values are clamped with a
    /// warning to stderr; unparseable values silently use the default.
    pub fn from_env() -> Self {
        // W1.2/B8: clamp violations go through tracing now (structured),
        // and unparseable-value silent fallbacks emit tracing::warn so
        // an operator typo (e.g. ECAA_WATCHDOG_PERIOD_SECS=thirty)
        // surfaces in the log instead of being invisibly ignored.
        let period_raw = std::env::var("ECAA_WATCHDOG_PERIOD_SECS").ok();
        let period_secs = match period_raw.as_deref() {
            None => WATCHDOG_PERIOD_SECS_DEFAULT,
            Some(v) => match v.trim().parse::<u64>() {
                Ok(n) => {
                    let clamped = n.clamp(WATCHDOG_PERIOD_SECS_MIN, WATCHDOG_PERIOD_SECS_MAX);
                    if clamped != n {
                        tracing::warn!(
                            target: "watchdog",
                            env = "ECAA_WATCHDOG_PERIOD_SECS",
                            value = n,
                            min = WATCHDOG_PERIOD_SECS_MIN,
                            max = WATCHDOG_PERIOD_SECS_MAX,
                            clamped_to = clamped,
                            "value out of range; clamped"
                        );
                    }
                    clamped
                }
                Err(_) => {
                    tracing::warn!(
                        target: "watchdog",
                        env = "ECAA_WATCHDOG_PERIOD_SECS",
                        value = %v,
                        default = WATCHDOG_PERIOD_SECS_DEFAULT,
                        "unparseable u64; falling back to default"
                    );
                    WATCHDOG_PERIOD_SECS_DEFAULT
                }
            },
        };

        let mult_raw = std::env::var("ECAA_WATCHDOG_MULTIPLIER").ok();
        let multiplier = match mult_raw.as_deref() {
            None => WATCHDOG_MULTIPLIER_DEFAULT,
            Some(v) => match v.trim().parse::<f64>() {
                Ok(n) => {
                    let clamped = n.clamp(WATCHDOG_MULTIPLIER_MIN, WATCHDOG_MULTIPLIER_MAX);
                    if (clamped - n).abs() > 1e-9 {
                        tracing::warn!(
                            target: "watchdog",
                            env = "ECAA_WATCHDOG_MULTIPLIER",
                            value = n,
                            min = WATCHDOG_MULTIPLIER_MIN,
                            max = WATCHDOG_MULTIPLIER_MAX,
                            clamped_to = clamped,
                            "value out of range; clamped"
                        );
                    }
                    clamped
                }
                Err(_) => {
                    tracing::warn!(
                        target: "watchdog",
                        env = "ECAA_WATCHDOG_MULTIPLIER",
                        value = %v,
                        default = WATCHDOG_MULTIPLIER_DEFAULT,
                        "unparseable f64; falling back to default"
                    );
                    WATCHDOG_MULTIPLIER_DEFAULT
                }
            },
        };

        Self {
            period_secs,
            multiplier,
        }
    }
}

/// Handle returned by [`Watchdog::spawn`]. Dropping without calling
/// [`Watchdog::stop`] is safe — the `Drop` impl sends the shutdown
/// signal and joins the thread.
pub struct Watchdog {
    /// Channel to request clean shutdown.
    shutdown_tx: SyncSender<()>,
    /// Thread handle. Wrapped in `Option` so `Drop` can take ownership.
    handle: Option<JoinHandle<()>>,
}

impl Watchdog {
    /// Spawn a new watchdog thread that polls `WORKFLOW.json` in
    /// `package_root` every `config.period_secs` seconds.
    ///
    /// `clock` is used for all "now" reads so tests can inject a
    /// [`ecaa_workflow_core::clock::FrozenClock`] and advance it
    /// deterministically.
    ///
    /// `event_tx` is a bounded sync-sender; the watchdog calls
    /// `try_send` so a saturated receiver never blocks the thread.
    pub fn spawn(
        package_root: PathBuf,
        clock: Arc<dyn Clock + Send + Sync>,
        config: WatchdogConfig,
        event_tx: SyncSender<WatchdogEvent>,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let handle = thread::Builder::new()
            .name("swfc-watchdog".into())
            .spawn(move || {
                run_watchdog_loop(
                    &package_root,
                    clock.as_ref(),
                    &config,
                    &event_tx,
                    &shutdown_rx,
                )
            })
            .expect("watchdog thread spawn");
        Self {
            shutdown_tx,
            handle: Some(handle),
        }
    }

    /// Signal the watchdog thread to stop and wait for it to exit.
    /// Idempotent: calling after `Drop` is a no-op.
    pub fn stop(&mut self) {
        // Best-effort: a saturated or disconnected channel means the thread
        // will exit at its next wakeup via the `shutdown_rx.try_recv` check.
        let _ = self.shutdown_tx.try_send(());
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- Thread loop --------------------------------------------------------

/// Inner loop run by the watchdog thread. Extracted for testability.
fn run_watchdog_loop(
    package_root: &Path,
    clock: &dyn Clock,
    config: &WatchdogConfig,
    event_tx: &SyncSender<WatchdogEvent>,
    shutdown_rx: &Receiver<()>,
) {
    loop {
        // Interruptible sleep: wake on shutdown signal or after period.
        let deadline = std::time::Instant::now() + Duration::from_secs(config.period_secs);
        loop {
            match shutdown_rx.try_recv() {
                Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            thread::sleep(remaining.min(Duration::from_millis(200)));
        }

        // Check for shutdown once more before doing I/O.
        match shutdown_rx.try_recv() {
            Ok(_) | Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }

        let now = clock.now();

        // Read the DAG. Soft-skip on error — transient I/O during agent write.
        let dag = match read_dag(package_root) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!(
                    target: "watchdog",
                    package_root = %package_root.display(),
                    error = %e,
                    "could not read WORKFLOW.json"
                );
                continue;
            }
        };

        // Build timeout_at map from the dispatch WAL for Running tasks.
        let timeout_map = build_timeout_map(package_root);

        for (task_id, task) in &dag.tasks {
            let started_at = match &task.state {
                TaskState::Running { started_at, .. } => started_at.as_str(),
                _ => continue,
            };

            // Patches don't merge into WORKFLOW.json until iteration end.
            // When agent A finishes early but iteration is gated on agent
            // B's longer run, WORKFLOW.json reports A as Running for the
            // whole iteration window. Without this check the watchdog
            // refires WallClockExceeded every period over that window —
            // 9 spurious warnings observed on S1 bulk_rnaseq for
            // already-completed data_acquisition. Treat the presence of
            // `state.patch.json` as "agent finished, patch awaiting
            // merge" and skip the warning. Same for orphan-patches that
            // record blocked or failed — those wall-clock alarms aren't
            // actionable.
            let patch_path = package_root
                .join("runtime/outputs")
                .join(task_id.as_str())
                .join("state.patch.json");
            if patch_path.is_file() {
                continue;
            }

            // Compute elapsed wall time since `started_at`.
            let start_dt = match DateTime::parse_from_rfc3339(started_at) {
                Ok(t) => t.with_timezone(&chrono::Utc),
                Err(_) => continue,
            };
            let age_secs = (now - start_dt).num_seconds().max(0) as u64;

            // Threshold from spec or fallback × multiplier.
            let expected_wall = extract_expected_wall_seconds(&task.spec)
                .unwrap_or(WATCHDOG_FALLBACK_EXPECTED_WALL_SECS);
            let threshold_secs = ((expected_wall as f64) * config.multiplier) as u64;

            // Check age-based budget.
            let age_exceeded = age_secs > threshold_secs;

            // Check timeout_at from WAL (strict wall-clock deadline).
            let timeout_exceeded = timeout_map
                .get(task_id.as_str())
                .and_then(|r| DateTime::parse_from_rfc3339(&r.timeout_at).ok())
                .map(|deadline| now > deadline.with_timezone(&chrono::Utc))
                .unwrap_or(false);

            if age_exceeded || timeout_exceeded {
                let effective_threshold = if timeout_exceeded && !age_exceeded {
                    // Report the timeout_at age as the threshold to make the
                    // message readable: threshold_secs = age at timeout_at.
                    timeout_map
                        .get(task_id.as_str())
                        .and_then(|r| DateTime::parse_from_rfc3339(&r.timeout_at).ok())
                        .map(|dl| {
                            (dl.with_timezone(&chrono::Utc) - start_dt)
                                .num_seconds()
                                .max(0) as u64
                        })
                        .unwrap_or(threshold_secs)
                } else {
                    threshold_secs
                };
                let _ = event_tx.try_send(WatchdogEvent::WallClockExceeded {
                    task_id: task_id.to_string(),
                    observed_secs: age_secs,
                    threshold_secs: effective_threshold,
                });
            }

            // Emit heartbeat age regardless of whether budget was exceeded.
            let hb_age = heartbeat_age_secs(package_root, task_id.as_str());
            if let Some(age) = hb_age {
                let _ = event_tx.try_send(WatchdogEvent::HeartbeatAge {
                    task_id: task_id.to_string(),
                    age_secs: age,
                });
            }
        }
    }
}

// ---- Helpers ------------------------------------------------------------

/// Read the DAG from `<package_root>/WORKFLOW.json`. Reuses the same
/// size-capped read the harness main loop uses.
fn read_dag(package_root: &Path) -> Result<DAG> {
    let raw = crate::ecaa_io::read_capped(
        &package_root.join("WORKFLOW.json"),
        crate::ecaa_io::resolve_max_bytes(),
    )?;
    serde_json::from_str::<DAG>(&raw).map_err(Into::into)
}

/// Build a `task_id → DispatchRecord` map from the last dispatch entry
/// per task in the WAL. Returns an empty map when the WAL is absent or
/// unreadable.
fn build_timeout_map(package_root: &Path) -> BTreeMap<String, DispatchRecord> {
    let mut map = BTreeMap::new();
    // `read_dispatches` returns an empty Vec on missing / unreadable WAL.
    let records = read_dispatches(package_root);
    // Later entries (higher epoch) overwrite earlier ones so the map holds
    // the most-recent dispatch per task.
    for rec in records {
        map.insert(rec.task_id.clone(), rec);
    }
    map
}

/// Extract `expected_wall_seconds` from the task's optional `spec` blob.
/// Returns `None` when the spec is absent or doesn't carry the field.
fn extract_expected_wall_seconds(spec: &Option<serde_json::Value>) -> Option<u64> {
    spec.as_ref()
        .and_then(|s| s.get("expected_wall_seconds"))
        .and_then(|v| v.as_u64())
}

/// Compute the age of a task's `.heartbeat` file in seconds.
fn heartbeat_age_secs(package_root: &Path, task_id: &str) -> Option<u64> {
    let path = package_root
        .join("runtime/outputs")
        .join(task_id)
        .join(".heartbeat");
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta.modified().ok()?;
    let elapsed = modified.elapsed().ok()?;
    Some(elapsed.as_secs())
}

// ---- Tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. env mutations in tests
    // are single-threaded test setup; bounded waiver scoped to this mod.
    #![allow(unsafe_code)]
    use super::*;
    use ecaa_workflow_core::clock::FrozenClock;
    use serde_json::json;
    use std::io::Write;
    use std::sync::mpsc;
    use tempfile::TempDir;

    fn make_tmpdir() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// Minimal `WORKFLOW.json` with a single Running task.
    fn write_workflow_json(
        package_root: &Path,
        task_id: &str,
        started_at: &str,
        expected_wall_seconds: Option<u64>,
    ) {
        let spec = expected_wall_seconds.map(|s| json!({ "expected_wall_seconds": s }));
        let state = json!({ "status": "running", "started_at": started_at });
        let task_obj = json!({
            "kind": "computation",
            "state": state,
            "depends_on": [],
            "assignee": "agent",
            "description": "test task",
            "resource_class": "cpu_heavy",
            "spec": spec,
        });
        let dag = json!({
            "version": "1.0",
            "workflow_id": "test-workflow-id",
            "tasks": { task_id: task_obj }
        });
        let dir = package_root;
        std::fs::write(
            dir.join("WORKFLOW.json"),
            serde_json::to_string(&dag).unwrap(),
        )
        .unwrap();
    }

    /// Write a dispatch WAL with one entry carrying the given `timeout_at`.
    fn write_dispatch_wal(package_root: &Path, task_id: &str, started_at: &str, timeout_at: &str) {
        let runtime = package_root.join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let wal_path = runtime.join("dispatches.jsonl");
        let record = json!({
            "schema_version": "3.0.0",
            "task_id": task_id,
            "epoch": 1,
            "harness_run_id": "test-run",
            "started_at": started_at,
            "timeout_at": timeout_at,
        });
        let mut f = std::fs::File::create(&wal_path).unwrap();
        writeln!(f, "{}", record).unwrap();
    }

    /// Write a heartbeat file with a specified mtime offset from now.
    fn write_heartbeat(package_root: &Path, task_id: &str, _age_secs: u64) {
        let dir = package_root.join("runtime/outputs").join(task_id);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".heartbeat");
        std::fs::write(&path, b"alive").unwrap();
        // Mtime set to "now" by the OS; for the heartbeat-age test we simply
        // verify the event is emitted (age will be ~0).
    }

    fn frozen_at(ts: &str) -> Arc<dyn Clock + Send + Sync> {
        let dt: chrono::DateTime<chrono::Utc> = ts.parse().unwrap();
        Arc::new(FrozenClock { at: dt })
    }

    /// Run one watchdog poll cycle synchronously (no spawned thread) and
    /// collect emitted events.
    fn poll_once(
        package_root: &Path,
        clock: &dyn Clock,
        config: &WatchdogConfig,
    ) -> Vec<WatchdogEvent> {
        let (tx, rx) = mpsc::sync_channel::<WatchdogEvent>(64);
        let now = clock.now();
        let dag = match read_dag(package_root) {
            Ok(d) => d,
            Err(_) => return vec![],
        };
        let timeout_map = build_timeout_map(package_root);

        for (task_id, task) in &dag.tasks {
            let started_at = match &task.state {
                TaskState::Running { started_at, .. } => started_at.as_str(),
                _ => continue,
            };
            // Mirror the patch-pending check from the live poll loop —
            // an agent that wrote `state.patch.json` is finished, even
            // though WORKFLOW.json still reports Running until the
            // iteration-boundary merge.
            let patch_path = package_root
                .join("runtime/outputs")
                .join(task_id.as_str())
                .join("state.patch.json");
            if patch_path.is_file() {
                continue;
            }
            let start_dt = match DateTime::parse_from_rfc3339(started_at) {
                Ok(t) => t.with_timezone(&chrono::Utc),
                Err(_) => continue,
            };
            let age_secs = (now - start_dt).num_seconds().max(0) as u64;
            let expected_wall = extract_expected_wall_seconds(&task.spec)
                .unwrap_or(WATCHDOG_FALLBACK_EXPECTED_WALL_SECS);
            let threshold_secs = ((expected_wall as f64) * config.multiplier) as u64;

            let age_exceeded = age_secs > threshold_secs;
            let timeout_exceeded = timeout_map
                .get(task_id.as_str())
                .and_then(|r| DateTime::parse_from_rfc3339(&r.timeout_at).ok())
                .map(|dl| now > dl.with_timezone(&chrono::Utc))
                .unwrap_or(false);

            if age_exceeded || timeout_exceeded {
                let effective_threshold = if timeout_exceeded && !age_exceeded {
                    timeout_map
                        .get(task_id.as_str())
                        .and_then(|r| DateTime::parse_from_rfc3339(&r.timeout_at).ok())
                        .map(|dl| {
                            (dl.with_timezone(&chrono::Utc) - start_dt)
                                .num_seconds()
                                .max(0) as u64
                        })
                        .unwrap_or(threshold_secs)
                } else {
                    threshold_secs
                };
                let _ = tx.try_send(WatchdogEvent::WallClockExceeded {
                    task_id: task_id.to_string(),
                    observed_secs: age_secs,
                    threshold_secs: effective_threshold,
                });
            }

            if let Some(age) = heartbeat_age_secs(package_root, task_id.as_str()) {
                let _ = tx.try_send(WatchdogEvent::HeartbeatAge {
                    task_id: task_id.to_string(),
                    age_secs: age,
                });
            }
        }
        drop(tx);
        rx.into_iter().collect()
    }

    // -----------------------------------------------------------------
    // 1. watchdog_fires_on_wall_clock_exceeded
    // -----------------------------------------------------------------
    #[test]
    fn watchdog_fires_on_wall_clock_exceeded() {
        let tmp = make_tmpdir();
        let root = tmp.path();
        // Task started 12001 s ago; expected_wall=600, multiplier=20 → budget=12000s.
        let started_at = "2026-05-18T00:00:00Z";
        // Clock is 12001s after started_at → age = 12001 > 12000.
        let now_ts = "2026-05-18T03:20:01Z"; // 3h20m01s later = 12001s
        write_workflow_json(root, "task_a", started_at, Some(600));
        let config = WatchdogConfig {
            period_secs: 30,
            multiplier: 20.0,
        };
        let events = poll_once(root, frozen_at(now_ts).as_ref(), &config);
        let exceeded: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, WatchdogEvent::WallClockExceeded { .. }))
            .collect();
        assert_eq!(exceeded.len(), 1, "expected one WallClockExceeded event");
        if let WatchdogEvent::WallClockExceeded {
            task_id,
            observed_secs,
            threshold_secs,
        } = &exceeded[0]
        {
            assert_eq!(task_id, "task_a");
            assert_eq!(*threshold_secs, 12000);
            assert!(*observed_secs > *threshold_secs);
        }
    }

    // -----------------------------------------------------------------
    // 2. watchdog_respects_timeout_at
    // -----------------------------------------------------------------
    #[test]
    fn watchdog_respects_timeout_at() {
        let tmp = make_tmpdir();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("runtime")).unwrap();
        // Task started 100s ago; expected_wall_seconds absent (fallback 1800),
        // multiplier 6.0 → budget 10800s — NOT exceeded by age alone.
        // But timeout_at is 50s after start → already elapsed.
        let started_at = "2026-05-18T00:00:00Z";
        let timeout_at = "2026-05-18T00:00:50Z"; // 50s after start
        let now_ts = "2026-05-18T00:01:40Z"; // 100s after start
        write_workflow_json(root, "task_b", started_at, None);
        write_dispatch_wal(root, "task_b", started_at, timeout_at);
        let config = WatchdogConfig {
            period_secs: 30,
            multiplier: 6.0,
        };
        let events = poll_once(root, frozen_at(now_ts).as_ref(), &config);
        let exceeded: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, WatchdogEvent::WallClockExceeded { .. }))
            .collect();
        assert_eq!(
            exceeded.len(),
            1,
            "expected WallClockExceeded from timeout_at; got {:?}",
            exceeded
        );
        if let WatchdogEvent::WallClockExceeded {
            task_id,
            threshold_secs,
            ..
        } = &exceeded[0]
        {
            assert_eq!(task_id, "task_b");
            // threshold_secs should reflect the timeout_at duration (50s), not
            // the multiplied expected_wall_seconds (10800s).
            assert_eq!(
                *threshold_secs, 50,
                "threshold should be timeout_at - started_at"
            );
        }
    }

    // -----------------------------------------------------------------
    // 3. watchdog_emits_heartbeat_age_secs
    // -----------------------------------------------------------------
    #[test]
    fn watchdog_emits_heartbeat_age_secs() {
        let tmp = make_tmpdir();
        let root = tmp.path();
        let started_at = "2026-05-18T00:00:00Z";
        let now_ts = "2026-05-18T00:01:00Z"; // 60s after start
        write_workflow_json(root, "task_c", started_at, Some(3600));
        // Create the heartbeat file now — age will be ~0s.
        write_heartbeat(root, "task_c", 0);
        let config = WatchdogConfig {
            period_secs: 30,
            multiplier: 6.0,
        };
        let events = poll_once(root, frozen_at(now_ts).as_ref(), &config);
        let hb_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, WatchdogEvent::HeartbeatAge { .. }))
            .collect();
        assert_eq!(
            hb_events.len(),
            1,
            "expected one HeartbeatAge event for task_c"
        );
        if let WatchdogEvent::HeartbeatAge { task_id, age_secs } = &hb_events[0] {
            assert_eq!(task_id, "task_c");
            // age is wall-clock; just ensure it was emitted (value ~0 on fast machines).
            let _ = age_secs; // suppress unused warning
        }
    }

    // -----------------------------------------------------------------
    // 4. watchdog_config_from_env
    // -----------------------------------------------------------------
    #[test]
    fn watchdog_config_from_env() {
        unsafe {
            std::env::set_var("ECAA_WATCHDOG_PERIOD_SECS", "45");
            std::env::set_var("ECAA_WATCHDOG_MULTIPLIER", "3.5");
        }
        let cfg = WatchdogConfig::from_env();
        assert_eq!(cfg.period_secs, 45);
        assert!((cfg.multiplier - 3.5).abs() < 1e-9);
        unsafe {
            std::env::remove_var("ECAA_WATCHDOG_PERIOD_SECS");
            std::env::remove_var("ECAA_WATCHDOG_MULTIPLIER");
        }
    }

    // -----------------------------------------------------------------
    // 5. watchdog_clamps_out_of_range_period
    // -----------------------------------------------------------------
    #[test]
    fn watchdog_clamps_out_of_range_period() {
        unsafe {
            std::env::set_var("ECAA_WATCHDOG_PERIOD_SECS", "5"); // below min=10
        }
        let cfg = WatchdogConfig::from_env();
        assert_eq!(
            cfg.period_secs, WATCHDOG_PERIOD_SECS_MIN,
            "period should be clamped to min"
        );
        unsafe {
            std::env::remove_var("ECAA_WATCHDOG_PERIOD_SECS");
        }
    }

    // -----------------------------------------------------------------
    // 6. watchdog_uses_fallback_when_no_expected_wall_seconds
    // -----------------------------------------------------------------
    #[test]
    fn watchdog_uses_fallback_when_no_expected_wall_seconds() {
        let tmp = make_tmpdir();
        let root = tmp.path();
        let started_at = "2026-05-18T00:00:00Z";
        // fallback=1800, mult=6 → budget=10800. Age = 10801 → exceeds.
        let now_ts = "2026-05-18T03:00:01Z"; // 10801s after start
        write_workflow_json(root, "task_d", started_at, None);
        let config = WatchdogConfig {
            period_secs: 30,
            multiplier: 6.0,
        };
        let events = poll_once(root, frozen_at(now_ts).as_ref(), &config);
        let exceeded: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, WatchdogEvent::WallClockExceeded { .. }))
            .collect();
        assert_eq!(
            exceeded.len(),
            1,
            "should fire when fallback budget exceeded"
        );
        if let WatchdogEvent::WallClockExceeded { threshold_secs, .. } = &exceeded[0] {
            assert_eq!(
                *threshold_secs,
                (WATCHDOG_FALLBACK_EXPECTED_WALL_SECS as f64 * 6.0) as u64
            );
        }
    }
}
