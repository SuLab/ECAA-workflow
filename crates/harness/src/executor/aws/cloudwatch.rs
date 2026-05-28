//! CloudWatch-driven stall monitor + metric parsing.
//!
//! Owns the `start_stall_monitor` / `stop_stall_monitor` trait bodies
//! (via `do_*` inherent impls on `AwsExecutor`), the `cloudwatch_max`
//! wrapper used by pilot, plus the pure-function parsers + sliding-window
//! helpers used by the polling thread.
//!
//! The stall-poll loop runs on a dedicated `std::thread` — it does not
//! borrow `&self`, so the shell-out helper here is a free `poll_cloudwatch_average`
//! rather than an inherent method.

use super::super::stall_monitor::{StallSignal, StallThresholds};
use super::{AwsExecutor, ProvisionedInstance};
use crate::constants::{STALL_LATCH_QUIESCENT_SECS_DEFAULT, STALL_RATE_LIMIT_SECS_DEFAULT};
use anyhow::Result;
use duct::cmd;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

impl AwsExecutor {
    /// AWS stall monitor entry. Spawns a `std::thread` that polls
    /// CloudWatch `CPUUtilization` + `MemoryUtilization` metrics on a
    /// sliding window and emits `StallSignal`s via `tx` when
    /// `evaluate_cpu_window` / `evaluate_memory_window` classify a
    /// breach. Exits when `stop_stall_monitor` flips the shutdown
    /// flag.
    ///
    /// P0-39 — the polling thread now reads `live_instance` (a shared
    /// mirror of `self.instance`) on every iteration. When the
    /// executor flips the instance — provision, spot-rebalance,
    /// release — the mirror updates synchronously and the monitor
    /// observes the new state on its next sample. When the instance
    /// is `None` (release fired and no replacement is up yet) the
    /// monitor exits gracefully instead of polling CloudWatch
    /// against a terminated instance id.
    ///
    /// The monitor gracefully handles the pre-provisioning window
    /// (no instance yet) by skipping samples until the mirror is
    /// populated. It uses `current_running_task_id` as the signal's
    /// task_id so multiple iterations can be correlated back to the
    /// stall blocker on the server.
    pub(super) fn do_start_stall_monitor(
        &mut self,
        thresholds: &StallThresholds,
        tx: std::sync::mpsc::Sender<StallSignal>,
    ) -> Result<()> {
        if !thresholds.enabled {
            return Ok(());
        }
        // Reset shutdown flag and clone the shared state for the
        // polling thread.
        *self.stall_shutdown.lock().unwrap() = false;
        let shutdown = self.stall_shutdown.clone();
        let task_id_cell = self.current_running_task_id.clone();
        let thresholds = thresholds.clone();
        let region = self.config.region.clone();
        // Make sure the mirror reflects the executor's current state
        // before the thread spawns — otherwise a race window between
        // `do_provision` finishing and `start_stall_monitor` running
        // could leave the mirror stale.
        self.sync_live_instance_mirror();
        let live_instance = self.live_instance.clone();

        std::thread::spawn(move || {
            cloudwatch_stall_loop(
                region,
                live_instance,
                task_id_cell,
                thresholds,
                tx,
                shutdown,
            );
        });
        Ok(())
    }

    pub(super) fn do_stop_stall_monitor(&mut self) {
        *self.stall_shutdown.lock().unwrap() = true;
    }

    /// Shell out to `aws cloudwatch get-metric-statistics` and return
    /// the maximum datapoint value for `metric_name` over the last
    /// 15 minutes as a `u64` (unit: percent for CPU, megabytes for
    /// memory — the caller decides which bucket).
    ///
    /// Returns an error when the shell-out fails or the JSON is
    /// unparseable. Returns `Ok(0)` when the Datapoints array is
    /// empty (no metrics published yet, or CloudWatch agent absent).
    /// Pilot + stall paths call this and treat `Ok(0)` as "no signal
    /// available" — graceful degradation.
    pub(super) fn cloudwatch_max(&self, metric_name: &str, instance_id: &str) -> Result<u64> {
        let end = chrono::Utc::now();
        let start = end - chrono::Duration::minutes(15);
        let start_s = start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let end_s = end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let dim = format!("Name=InstanceId,Value={}", instance_id);
        let stdout = self.run_aws(&[
            "cloudwatch",
            "get-metric-statistics",
            "--namespace",
            "AWS/EC2",
            "--metric-name",
            metric_name,
            "--dimensions",
            &dim,
            "--statistics",
            "Maximum",
            "--period",
            "60",
            "--start-time",
            &start_s,
            "--end-time",
            &end_s,
            "--output",
            "json",
        ])?;
        Ok(parse_cloudwatch_max(&stdout))
    }
}

/// Parse `aws cloudwatch get-metric-statistics` output and return the
/// maximum `Maximum` datapoint value, clamped to u64. Empty / malformed
/// responses return 0 so pilot + stall consumers can degrade gracefully.
pub(super) fn parse_cloudwatch_max(stdout: &str) -> u64 {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return 0;
    };
    let Some(points) = v["Datapoints"].as_array() else {
        return 0;
    };
    let mut peak: f64 = 0.0;
    for p in points {
        // CloudWatch response uses the statistic name as the key
        // (we requested Maximum), so read that directly. Fall through
        // to Average when Maximum is absent — some older metrics only
        // publish Average.
        let val = p["Maximum"]
            .as_f64()
            .or_else(|| p["Average"].as_f64())
            .unwrap_or(0.0);
        if val > peak {
            peak = val;
        }
    }
    if peak <= 0.0 {
        0
    } else {
        peak as u64
    }
}

/// Parse `aws cloudwatch get-metric-statistics` into the list of
/// `Average` datapoints, sorted ascending by timestamp. Used by the
/// stall monitor's sliding-window classifier. Empty / malformed
/// responses yield an empty vector.
pub(super) fn parse_cloudwatch_averages(stdout: &str) -> Vec<f32> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return Vec::new();
    };
    let Some(points) = v["Datapoints"].as_array() else {
        return Vec::new();
    };
    let mut typed: Vec<(String, f64)> = Vec::new();
    for p in points {
        let ts = p["Timestamp"].as_str().unwrap_or("").to_string();
        let avg = p["Average"]
            .as_f64()
            .or_else(|| p["Maximum"].as_f64())
            .unwrap_or(0.0);
        typed.push((ts, avg));
    }
    // CloudWatch does not guarantee ordering; sort by timestamp so the
    // sliding window reflects temporal order even when the shim hands
    // us back scrambled datapoints.
    typed.sort_by(|a, b| a.0.cmp(&b.0));
    typed.into_iter().map(|(_, v)| v as f32).collect()
}

/// Signal kind used as a rate-limit / latch-reset key. The wire
/// variant carries window metadata that varies per-evaluation, so the
/// monitor uses this slim enum as the dedup tag instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SignalKind {
    Cpu,
    Memory,
}

/// Per-(task, kind) bookkeeping used by the latch reset + rate-limit
/// (P0-40). `last_fired_at` powers the inter-emission gap; `healthy_streak`
/// counts the contiguous healthy samples seen since the latch fired so
/// the monitor knows when to clear it.
#[derive(Debug, Clone, Default)]
struct LatchState {
    last_fired_at: Option<Instant>,
    healthy_streak: u64,
}

/// Polling loop run on a dedicated `std::thread` by
/// `start_stall_monitor`. Samples CloudWatch at
/// `thresholds.sample_interval_secs`, maintains CPU + memory sliding
/// windows, and posts classified signals via `tx`. Exits when
/// `shutdown` flips OR when the live-instance mirror flips to `None`
/// (the executor has definitively released the host — P0-39). On the
/// next `start_stall_monitor` call the harness spawns a fresh thread
/// against the new instance.
///
/// P0-40 — the polling loop tracks per-(task_id, kind) latches in a
/// `BTreeMap<(String, SignalKind), LatchState>`. Latches clear after
/// `STALL_LATCH_QUIESCENT_SECS` of contiguous healthy samples (CPU
/// recovered above threshold, memory dropped below) so a task that
/// transiently recovers can re-fire on a second genuine stall. A
/// `(task_id, kind)` rate limit guarantees at least
/// `STALL_RATE_LIMIT_SECS` between two identical emissions even when
/// the latch legitimately reset, preventing a chatty configuration
/// from spamming the channel.
fn cloudwatch_stall_loop(
    region: String,
    live_instance: Arc<Mutex<Option<ProvisionedInstance>>>,
    task_id_cell: Arc<Mutex<Option<String>>>,
    thresholds: StallThresholds,
    tx: std::sync::mpsc::Sender<StallSignal>,
    shutdown: Arc<Mutex<bool>>,
) {
    if !thresholds.enabled {
        return;
    }
    let interval = Duration::from_secs(thresholds.sample_interval_secs.max(1));
    let cpu_window_size =
        (thresholds.cpu_window_mins * 60 / thresholds.sample_interval_secs.max(1)) as usize;
    let mem_window_size =
        (thresholds.mem_window_mins * 60 / thresholds.sample_interval_secs.max(1)) as usize;
    // Number of samples required to satisfy the quiescent window. At
    // a default 30s sample cadence and 300s quiescent window this is
    // 10 samples; clamped to at least 1 so a tiny test interval still
    // requires a confirming sample before clearing the latch.
    let quiescent_sample_count =
        (STALL_LATCH_QUIESCENT_SECS_DEFAULT / thresholds.sample_interval_secs.max(1)).max(1);
    let rate_limit = Duration::from_secs(STALL_RATE_LIMIT_SECS_DEFAULT);
    let mut cpu_samples: VecDeque<f32> = VecDeque::with_capacity(cpu_window_size.max(1));
    let mut mem_samples: VecDeque<f32> = VecDeque::with_capacity(mem_window_size.max(1));
    let mut latches: BTreeMap<(String, SignalKind), LatchState> = BTreeMap::new();

    loop {
        if *shutdown.lock().unwrap() {
            return;
        }
        std::thread::sleep(interval);
        if *shutdown.lock().unwrap() {
            return;
        }

        // P0-39 — read the live mirror every iteration. `None` means
        // the executor released the host; exit so we don't poll
        // CloudWatch against a stale id. `start_stall_monitor` spawns
        // a fresh thread on the next provision.
        let iid_owned: Option<String> = {
            let guard = live_instance.lock().unwrap_or_else(|p| p.into_inner());
            guard.as_ref().map(|i| i.instance_id.clone())
        };
        let Some(iid) = iid_owned else {
            return;
        };

        // Sample CPU.
        let mut cpu_sample: Option<f32> = None;
        if let Some(cpu_pct) = poll_cloudwatch_average(&region, "CPUUtilization", &iid) {
            push_windowed(&mut cpu_samples, cpu_pct, cpu_window_size);
            cpu_sample = Some(cpu_pct);
        }
        // Sample memory. CloudWatch agent may be absent — `None`
        // means "skip this sample" rather than "emit zero".
        let mut mem_sample: Option<f32> = None;
        if let Some(mem_pct) = poll_cloudwatch_average(&region, "MemoryUtilization", &iid) {
            push_windowed(&mut mem_samples, mem_pct, mem_window_size);
            mem_sample = Some(mem_pct);
        }

        let task_id = task_id_cell
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .unwrap_or_else(|| iid.clone());

        // P0-40 — evaluate CPU latch + rate limit.
        let cpu_key = (task_id.clone(), SignalKind::Cpu);
        let cpu_latched = latches
            .get(&cpu_key)
            .and_then(|s| s.last_fired_at)
            .is_some();
        if !cpu_latched && cpu_samples.len() >= cpu_window_size.max(1) {
            let samples: Vec<f32> = cpu_samples.iter().copied().collect();
            if let Some(sig) = super::super::stall_monitor::evaluate_cpu_window(
                &task_id,
                &samples,
                thresholds.cpu_window_mins,
                thresholds.cpu_min_pct,
            ) {
                if rate_limit_allows(&latches, &cpu_key, rate_limit) {
                    let _ = tx.send(sig);
                    latches.insert(
                        cpu_key.clone(),
                        LatchState {
                            last_fired_at: Some(Instant::now()),
                            healthy_streak: 0,
                        },
                    );
                }
            }
        }
        // Latch reset bookkeeping: any sample at or above the CPU
        // threshold counts as a healthy sample. After
        // `quiescent_sample_count` contiguous healthy samples, clear
        // the latched marker so a fresh stall on the same task can
        // re-fire (subject to the rate limit).
        if let Some(pct) = cpu_sample {
            if let Some(state) = latches.get_mut(&cpu_key) {
                if state.last_fired_at.is_some() {
                    if pct >= thresholds.cpu_min_pct {
                        state.healthy_streak = state.healthy_streak.saturating_add(1);
                        if state.healthy_streak >= quiescent_sample_count {
                            state.last_fired_at = None;
                            state.healthy_streak = 0;
                        }
                    } else {
                        state.healthy_streak = 0;
                    }
                }
            }
        }

        // P0-40 — evaluate memory latch + rate limit.
        let mem_key = (task_id.clone(), SignalKind::Memory);
        let mem_latched = latches
            .get(&mem_key)
            .and_then(|s| s.last_fired_at)
            .is_some();
        if !mem_latched && mem_samples.len() >= mem_window_size.max(1) {
            let samples: Vec<f32> = mem_samples.iter().copied().collect();
            if let Some(sig) = super::super::stall_monitor::evaluate_memory_window(
                &task_id,
                &samples,
                thresholds.mem_window_mins,
                thresholds.mem_max_pct,
            ) {
                if rate_limit_allows(&latches, &mem_key, rate_limit) {
                    let _ = tx.send(sig);
                    latches.insert(
                        mem_key.clone(),
                        LatchState {
                            last_fired_at: Some(Instant::now()),
                            healthy_streak: 0,
                        },
                    );
                }
            }
        }
        // Latch reset bookkeeping for memory: a sample at or below
        // the memory threshold counts as healthy.
        if let Some(pct) = mem_sample {
            if let Some(state) = latches.get_mut(&mem_key) {
                if state.last_fired_at.is_some() {
                    if pct <= thresholds.mem_max_pct {
                        state.healthy_streak = state.healthy_streak.saturating_add(1);
                        if state.healthy_streak >= quiescent_sample_count {
                            state.last_fired_at = None;
                            state.healthy_streak = 0;
                        }
                    } else {
                        state.healthy_streak = 0;
                    }
                }
            }
        }

        // Garbage-collect latches that are fully reset AND haven't
        // fired this iteration so the map stays bounded as tasks
        // come and go.
        latches.retain(|_, s| s.last_fired_at.is_some());
    }
}

/// Returns true when enough wall time has elapsed since the last
/// emission for the given key. Missing latch entries are treated as
/// "never fired"; the rate limit is then trivially satisfied.
fn rate_limit_allows(
    latches: &BTreeMap<(String, SignalKind), LatchState>,
    key: &(String, SignalKind),
    rate_limit: Duration,
) -> bool {
    match latches.get(key).and_then(|s| s.last_fired_at) {
        Some(when) => when.elapsed() >= rate_limit,
        None => true,
    }
}

/// Shell-out wrapper used by the stall poller (not a method on
/// AwsExecutor so the thread doesn't need to borrow `&self`). Returns
/// the latest Average datapoint over the last 5 minutes, or `None`
/// when no datapoints are available.
fn poll_cloudwatch_average(region: &str, metric_name: &str, instance_id: &str) -> Option<f32> {
    let end = chrono::Utc::now();
    let start = end - chrono::Duration::minutes(5);
    let start_s = start.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let end_s = end.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let dim = format!("Name=InstanceId,Value={}", instance_id);
    let stdout = match cmd(
        "aws",
        &[
            "cloudwatch",
            "get-metric-statistics",
            "--namespace",
            "AWS/EC2",
            "--metric-name",
            metric_name,
            "--dimensions",
            &dim,
            "--statistics",
            "Average",
            "--period",
            "60",
            "--start-time",
            &start_s,
            "--end-time",
            &end_s,
            "--output",
            "json",
        ],
    )
    .env("AWS_REGION", region)
    .stderr_to_stdout()
    .read()
    {
        Ok(out) => out,
        Err(_) => return None,
    };
    let averages = parse_cloudwatch_averages(&stdout);
    // Take the most recent datapoint as the current sample.
    averages.last().copied()
}

fn push_windowed(buf: &mut VecDeque<f32>, value: f32, max: usize) {
    if max == 0 {
        return;
    }
    buf.push_back(value);
    while buf.len() > max {
        buf.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cloudwatch_max_returns_highest_maximum_datapoint() {
        let canned = r#"{
            "Datapoints":[
                {"Timestamp":"2026-04-17T12:00:00Z","Maximum":30.5,"Unit":"Percent"},
                {"Timestamp":"2026-04-17T12:01:00Z","Maximum":75.0,"Unit":"Percent"},
                {"Timestamp":"2026-04-17T12:02:00Z","Maximum":50.0,"Unit":"Percent"}
            ]
        }"#;
        assert_eq!(parse_cloudwatch_max(canned), 75);
    }

    #[test]
    fn parse_cloudwatch_max_returns_zero_on_empty_or_malformed() {
        assert_eq!(parse_cloudwatch_max(r#"{"Datapoints":[]}"#), 0);
        assert_eq!(parse_cloudwatch_max("not-json"), 0);
        assert_eq!(parse_cloudwatch_max(r#"{"OtherKey":42}"#), 0);
    }

    #[test]
    fn parse_cloudwatch_averages_sorts_by_timestamp() {
        // Datapoints intentionally out of order.
        let canned = r#"{
            "Datapoints":[
                {"Timestamp":"2026-04-17T12:02:00Z","Average":3.0},
                {"Timestamp":"2026-04-17T12:00:00Z","Average":1.0},
                {"Timestamp":"2026-04-17T12:01:00Z","Average":2.0}
            ]
        }"#;
        let samples = parse_cloudwatch_averages(canned);
        assert_eq!(samples, vec![1.0, 2.0, 3.0]);
    }
}
