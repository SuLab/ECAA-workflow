//! Stall detection for long-running tasks.
//!
//! The stall monitor runs in a dedicated `std::thread` alongside the
//! agent subprocess (local) or the SSM command (AWS). It samples
//! resource utilisation at `StallThresholds::sample_interval_secs` and
//! emits a `StallSignal` on the mpsc channel whenever a threshold is
//! breached. The harness main loop reads the channel between iterations
//! and translates signals into server-side events (`kind: "task_stalled"`)
//! that transition the session to `Blocked { Stalled }`.
//!
//! Sync-friendly by construction — no tokio leaks into the harness
//! crate.
//!
//! Per-executor impls:
//!
//! * `LocalExecutor` — polls `/proc/<pid>/stat` for CPU +
//!   `/proc/<pid>/status` for memory on Linux; no-ops on non-Linux.
//! * `AwsExecutor` — shell-outs to
//!   `aws cloudwatch get-metric-statistics` for `CPUUtilization` and
//!   `MemoryUtilization`.

use crate::constants::{
    STALL_CPU_MIN_PCT_DEFAULT, STALL_CPU_WINDOW_MINS_DEFAULT, STALL_GPU_IDLE_MINS_DEFAULT,
    STALL_MEM_MAX_PCT_DEFAULT, STALL_MEM_WINDOW_MINS_DEFAULT,
    STALL_RUNTIME_OVER_EXPECTED_MULT_DEFAULT, STALL_SAMPLE_INTERVAL_SECS_DEFAULT,
};
use ecaa_workflow_core::blocker::{StallAction, StallSignalWire};
use std::env;
use std::io::Write as _;

/// Threshold configuration for the stall monitor. Read from env vars
/// with hard-coded defaults matching plan §3.2.
#[derive(Debug, Clone, PartialEq)]
pub struct StallThresholds {
    /// When `false`, the stall monitor does nothing; all other fields are ignored.
    pub enabled: bool,
    /// CPU below this percentage counts as starvation.
    pub cpu_min_pct: f32,
    /// Sustained duration before a starvation event fires.
    pub cpu_window_mins: u64,
    /// Memory above this percentage counts as pressure.
    pub mem_max_pct: f32,
    /// Rolling window (minutes) for the memory-pressure check.
    pub mem_window_mins: u64,
    /// Minutes of consecutive GPU idle before the GPU-idle stall fires.
    pub gpu_idle_when_training_mins: u64,
    /// Multiplier on expected task runtime before a runtime-overrun
    /// signal fires.
    pub runtime_over_expected_mult: f32,
    /// Sampling cadence. 30s is plenty for CPU + memory; the CloudWatch
    /// backend is capped at 60s native granularity.
    pub sample_interval_secs: u64,
}

impl Default for StallThresholds {
    fn default() -> Self {
        Self {
            enabled: false,
            cpu_min_pct: STALL_CPU_MIN_PCT_DEFAULT,
            cpu_window_mins: STALL_CPU_WINDOW_MINS_DEFAULT,
            mem_max_pct: STALL_MEM_MAX_PCT_DEFAULT,
            mem_window_mins: STALL_MEM_WINDOW_MINS_DEFAULT,
            gpu_idle_when_training_mins: STALL_GPU_IDLE_MINS_DEFAULT,
            runtime_over_expected_mult: STALL_RUNTIME_OVER_EXPECTED_MULT_DEFAULT,
            sample_interval_secs: STALL_SAMPLE_INTERVAL_SECS_DEFAULT,
        }
    }
}

impl StallThresholds {
    /// Read thresholds from `SWFC_STALL_*` env vars. Unset vars fall
    /// back to the compiled-in defaults. `enabled` auto-enables on
    /// `SWFC_EXECUTOR_MODE=aws` when not explicitly set.
    pub fn from_env() -> Self {
        let mut t = Self::default();
        let aws_mode = env::var("SWFC_EXECUTOR_MODE").as_deref() == Ok("aws");
        t.enabled = match env::var("SWFC_STALL_ENABLED") {
            Ok(v) => parse_bool(&v).unwrap_or(aws_mode),
            Err(_) => aws_mode,
        };
        if let Ok(v) = env::var("SWFC_STALL_CPU_MIN_PCT") {
            if let Ok(n) = v.trim().parse() {
                t.cpu_min_pct = n;
            }
        }
        if let Ok(v) = env::var("SWFC_STALL_CPU_WINDOW_MINS") {
            if let Ok(n) = v.trim().parse() {
                // 0 is a legitimate setting: after
                // `sample_stall_loop` clamps the window size to a
                // minimum of 1 sample, `cpu_window_mins=0` means "fire
                // on the first sub-threshold observation." Useful for
                // integration tests and for aggressive one-shot checks.
                t.cpu_window_mins = n;
            }
        }
        if let Ok(v) = env::var("SWFC_STALL_MEM_MAX_PCT") {
            if let Ok(n) = v.trim().parse() {
                t.mem_max_pct = n;
            }
        }
        if let Ok(v) = env::var("SWFC_STALL_MEM_WINDOW_MINS") {
            if let Ok(n) = v.trim().parse() {
                // Same rationale as CPU window.
                t.mem_window_mins = n;
            }
        }
        if let Ok(v) = env::var("SWFC_STALL_GPU_IDLE_MINS") {
            if let Ok(n) = v.trim().parse() {
                if n > 0 {
                    t.gpu_idle_when_training_mins = n;
                }
            }
        }
        if let Ok(v) = env::var("SWFC_STALL_RUNTIME_MULT") {
            if let Ok(n) = v.trim().parse() {
                if n > 0.0 {
                    t.runtime_over_expected_mult = n;
                }
            }
        }
        if let Ok(v) = env::var("SWFC_STALL_SAMPLE_INTERVAL_SECS") {
            if let Ok(n) = v.trim().parse() {
                if n > 0 {
                    t.sample_interval_secs = n;
                }
            }
        }
        t
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Signal emitted by the monitor when a threshold breach is observed.
/// Carries the task id and the raw measurements so downstream
/// consumers (`HarnessEvent`, `BlockerKind::Stalled`) can render in
/// plain language.
#[derive(Debug, Clone, PartialEq)]
pub enum StallSignal {
    /// Average CPU utilisation stayed below the threshold for the full window.
    CpuStarvation {
        /// Task that triggered the signal.
        task_id: String,
        /// Observed average CPU utilisation (percent).
        avg_cpu_pct: f32,
        /// Window over which the average was computed (minutes).
        window_mins: u64,
    },
    /// Average memory utilisation exceeded the threshold for the full window.
    MemoryPressure {
        /// Task that triggered the signal.
        task_id: String,
        /// Observed average memory utilisation (percent).
        pct: f32,
        /// Window over which the average was computed (minutes).
        window_mins: u64,
    },
    /// GPU was idle for the full window while the task was running.
    GpuIdleDuringTraining {
        /// Task that triggered the signal.
        task_id: String,
        /// Duration of observed GPU idleness (minutes).
        window_mins: u64,
    },
    /// Actual runtime exceeded expected × multiplier.
    RuntimeOverExpected {
        /// Task that triggered the signal.
        task_id: String,
        /// Observed wall time (seconds).
        actual_secs: u64,
        /// Expected wall time used as the baseline (seconds).
        expected_secs: u64,
    },
}

impl StallSignal {
    /// Convert to the wire-compatible variant for cross-crate transfer.
    pub fn to_wire(&self) -> StallSignalWire {
        match self {
            StallSignal::CpuStarvation {
                avg_cpu_pct,
                window_mins,
                ..
            } => StallSignalWire::CpuStarvation {
                avg_cpu_pct: *avg_cpu_pct,
                window_mins: *window_mins,
            },
            StallSignal::MemoryPressure {
                pct, window_mins, ..
            } => StallSignalWire::MemoryPressure {
                pct: *pct,
                window_mins: *window_mins,
            },
            StallSignal::GpuIdleDuringTraining { window_mins, .. } => {
                StallSignalWire::GpuIdleDuringTraining {
                    window_mins: *window_mins,
                }
            }
            StallSignal::RuntimeOverExpected {
                actual_secs,
                expected_secs,
                ..
            } => StallSignalWire::RuntimeOverExpected {
                actual_secs: *actual_secs,
                expected_secs: *expected_secs,
            },
        }
    }

    /// Return the task id associated with this signal regardless of variant.
    pub fn task_id(&self) -> &str {
        match self {
            StallSignal::CpuStarvation { task_id, .. }
            | StallSignal::MemoryPressure { task_id, .. }
            | StallSignal::GpuIdleDuringTraining { task_id, .. }
            | StallSignal::RuntimeOverExpected { task_id, .. } => task_id,
        }
    }

    /// Per-signal default recovery recommendation. `BlockerCard`
    /// highlights this as the default button; all three choices are
    /// always offered.
    pub fn suggested_action(&self) -> StallAction {
        match self {
            // Low CPU suggests the instance is oversized for the load; retry
            // with the same shape and see if it completes. The SME can
            // decide to abort after the second stall.
            StallSignal::CpuStarvation { .. } => StallAction::Retry,
            // Memory pressure means the instance is undersized; resize
            // before retrying or the next iteration will stall too.
            StallSignal::MemoryPressure { .. } => StallAction::Resize,
            // GPU idle during training is usually a misconfigured job,
            // not a sizing issue; a retry often fixes the mount / env.
            StallSignal::GpuIdleDuringTraining { .. } => StallAction::Retry,
            // Runtime overrun: abort by default, the SME can choose to
            // resize up if they believe the task needs a bigger shape.
            StallSignal::RuntimeOverExpected { .. } => StallAction::Abort,
        }
    }
}

/// Classification helper: given a window of CPU samples, return a
/// `CpuStarvation` signal when the average is below threshold for the
/// full window. Pure — no clocks, no threads — so the rule is unit
/// testable without spawning.
pub fn evaluate_cpu_window(
    task_id: &str,
    samples_pct: &[f32],
    window_mins: u64,
    threshold_pct: f32,
) -> Option<StallSignal> {
    if samples_pct.is_empty() {
        return None;
    }
    let avg = samples_pct.iter().sum::<f32>() / samples_pct.len() as f32;
    if avg < threshold_pct {
        Some(StallSignal::CpuStarvation {
            task_id: task_id.to_string(),
            avg_cpu_pct: avg,
            window_mins,
        })
    } else {
        None
    }
}

/// Classification helper for memory pressure: every sample in the
/// window must be above the threshold.
pub fn evaluate_memory_window(
    task_id: &str,
    samples_pct: &[f32],
    window_mins: u64,
    threshold_pct: f32,
) -> Option<StallSignal> {
    if samples_pct.is_empty() {
        return None;
    }
    if samples_pct.iter().all(|&p| p > threshold_pct) {
        let avg = samples_pct.iter().sum::<f32>() / samples_pct.len() as f32;
        Some(StallSignal::MemoryPressure {
            task_id: task_id.to_string(),
            pct: avg,
            window_mins,
        })
    } else {
        None
    }
}

/// Map a memory-pressure stall signal to the next larger instance
/// type. Returns `None` for CPU starvation, idle GPU, or runtime
/// overrun (resize isn't the right recovery for those), and for
/// instance types already at the top of their family (where a resize
/// within the family is impossible). Small table is hardcoded here
/// rather than derived from the profiles YAML because the bump
/// path is fixed by AWS instance-type shape, not by per-workload
/// sizing.
pub fn suggest_resize(signal: &StallSignal, current_instance: &str) -> Option<String> {
    match signal {
        StallSignal::MemoryPressure { .. } => next_larger_instance(current_instance),
        _ => None,
    }
}

fn next_larger_instance(current: &str) -> Option<String> {
    // Memory-optimized family (r6i.*): one step up within family.
    match current {
        "r6i.xlarge" => Some("r6i.2xlarge".to_string()),
        "r6i.2xlarge" => Some("r6i.4xlarge".to_string()),
        "r6i.4xlarge" => Some("r6i.8xlarge".to_string()),
        "r6i.8xlarge" => None,
        // Compute-optimized family (c6i.*): pivot to memory-optimized
        // on a pressure event because memory was the limiting factor.
        "c6i.xlarge" => Some("r6i.xlarge".to_string()),
        "c6i.2xlarge" => Some("r6i.2xlarge".to_string()),
        "c6i.4xlarge" => Some("r6i.4xlarge".to_string()),
        "c6i.8xlarge" => Some("r6i.8xlarge".to_string()),
        // Burstable family: pivot to memory-optimized at baseline.
        "t3.medium" => Some("r6i.xlarge".to_string()),
        "t3.large" => Some("r6i.xlarge".to_string()),
        // GPU families: resize within family by one step where known.
        "g4dn.xlarge" => Some("g6.xlarge".to_string()),
        "g6.xlarge" => Some("p4d.24xlarge".to_string()),
        "p4d.24xlarge" => None,
        _ => None,
    }
}

/// Classification helper for runtime overrun. `expected_secs == 0`
/// means no budget was configured and the signal is suppressed.
pub fn evaluate_runtime(
    task_id: &str,
    actual_secs: u64,
    expected_secs: u64,
    mult: f32,
) -> Option<StallSignal> {
    if expected_secs == 0 {
        return None;
    }
    let budget = (expected_secs as f32 * mult) as u64;
    if actual_secs > budget {
        Some(StallSignal::RuntimeOverExpected {
            task_id: task_id.to_string(),
            actual_secs,
            expected_secs,
        })
    } else {
        None
    }
}

/// Append a crash-survival record for a stall signal to
/// `<package_root>/runtime/stall-signals.jsonl`.
///
/// This sidecar is a defense-in-depth forensic record written BEFORE
/// the harness POSTs the signal to the server. If the harness crashes
/// between detection and a successful POST, the signal survives here
/// for post-crash forensics and future replay tooling.
///
/// The wire-event (`HarnessProgressEvent` with `stall_signal`) remains
/// the authoritative channel for live operations. The sidecar is
/// intentionally redundant — it trades a small I/O cost at signal time
/// for zero-loss durability on the crash path.
///
/// Best-effort: if the append fails (disk full, permission error) we
/// `tracing::warn!` and continue. The stall channel send is the
/// load-bearing signal; the sidecar never blocks it.
pub fn append_stall_signal_record(package_root: &std::path::Path, signal: &StallSignal) {
    let runtime = package_root.join("runtime");
    if let Err(e) = std::fs::create_dir_all(&runtime) {
        tracing::warn!(
            target: "harness-stall",
            error = %e,
            "stall-signals.jsonl: mkdir runtime/ failed"
        );
        return;
    }
    let path = runtime.join("stall-signals.jsonl");

    let (kind, measurements) = match signal {
        StallSignal::CpuStarvation {
            avg_cpu_pct,
            window_mins,
            ..
        } => (
            "cpu_starvation",
            serde_json::json!({ "avg_cpu_pct": avg_cpu_pct, "window_mins": window_mins }),
        ),
        StallSignal::MemoryPressure {
            pct, window_mins, ..
        } => (
            "memory_pressure",
            serde_json::json!({ "pct": pct, "window_mins": window_mins }),
        ),
        StallSignal::GpuIdleDuringTraining { window_mins, .. } => (
            "gpu_idle",
            serde_json::json!({ "window_mins": window_mins }),
        ),
        StallSignal::RuntimeOverExpected {
            actual_secs,
            expected_secs,
            ..
        } => (
            "runtime_overrun",
            serde_json::json!({ "actual_secs": actual_secs, "expected_secs": expected_secs }),
        ),
    };

    let suggested_action = match signal.suggested_action() {
        StallAction::Retry => "retry",
        StallAction::Resize => "resize",
        StallAction::Abort => "abort",
    };

    let record = serde_json::json!({
        "ts": ecaa_workflow_core::time_helpers::now_rfc3339(),
        "task_id": signal.task_id(),
        "kind": kind,
        "measurements": measurements,
        "suggested_action": suggested_action,
    });
    let mut line = serde_json::to_string(&record).unwrap_or_default();
    line.push('\n');

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                tracing::warn!(
                    target: "harness-stall",
                    error = %e,
                    "stall-signals.jsonl write failed"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                target: "harness-stall",
                error = %e,
                "stall-signals.jsonl open failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup; bounded waiver scoped to this
    // `mod tests` block.
    #![allow(unsafe_code)]
    use super::*;

    #[test]
    fn default_thresholds_match_plan_spec() {
        let t = StallThresholds::default();
        assert_eq!(t.cpu_min_pct, 5.0);
        assert_eq!(t.cpu_window_mins, 30);
        assert_eq!(t.mem_max_pct, 90.0);
        assert_eq!(t.mem_window_mins, 5);
        assert_eq!(t.gpu_idle_when_training_mins, 15);
        assert_eq!(t.runtime_over_expected_mult, 2.0);
    }

    #[test]
    fn cpu_window_fires_when_average_below_threshold() {
        let samples = vec![1.0, 2.0, 3.0, 4.0]; // avg 2.5 < 5.0
        let s = evaluate_cpu_window("t1", &samples, 30, 5.0);
        assert!(matches!(s, Some(StallSignal::CpuStarvation { .. })));
    }

    #[test]
    fn cpu_window_silent_when_average_above_threshold() {
        let samples = vec![10.0, 20.0, 5.5];
        assert!(evaluate_cpu_window("t1", &samples, 30, 5.0).is_none());
    }

    #[test]
    fn cpu_window_empty_is_silent() {
        assert!(evaluate_cpu_window("t1", &[], 30, 5.0).is_none());
    }

    #[test]
    fn memory_window_requires_all_samples_above_threshold() {
        // One sample dips below threshold → no signal.
        let samples = vec![92.0, 93.0, 85.0, 94.0];
        assert!(evaluate_memory_window("t1", &samples, 5, 90.0).is_none());

        // All samples above threshold → signal.
        let samples = vec![92.0, 93.0, 91.0, 94.0];
        let s = evaluate_memory_window("t1", &samples, 5, 90.0);
        assert!(matches!(s, Some(StallSignal::MemoryPressure { .. })));
    }

    #[test]
    fn runtime_fires_when_over_budget() {
        // expected 600s, actual 1500s, mult 2.0 → budget 1200s → fires.
        let s = evaluate_runtime("t1", 1500, 600, 2.0);
        assert!(matches!(s, Some(StallSignal::RuntimeOverExpected { .. })));
    }

    #[test]
    fn runtime_silent_when_under_budget() {
        let s = evaluate_runtime("t1", 900, 600, 2.0);
        assert!(s.is_none());
    }

    #[test]
    fn runtime_silent_without_budget() {
        let s = evaluate_runtime("t1", 1_000_000, 0, 2.0);
        assert!(s.is_none());
    }

    #[test]
    fn to_wire_preserves_fields() {
        let sig = StallSignal::MemoryPressure {
            task_id: "t1".into(),
            pct: 93.0,
            window_mins: 5,
        };
        let wire = sig.to_wire();
        match wire {
            StallSignalWire::MemoryPressure { pct, window_mins } => {
                assert!((pct - 93.0).abs() < 1e-4);
                assert_eq!(window_mins, 5);
            }
            other => panic!("unexpected wire variant: {:?}", other),
        }
    }

    #[test]
    fn suggested_actions_are_sane() {
        assert_eq!(
            StallSignal::CpuStarvation {
                task_id: "t".into(),
                avg_cpu_pct: 2.0,
                window_mins: 30,
            }
            .suggested_action(),
            StallAction::Retry
        );
        assert_eq!(
            StallSignal::MemoryPressure {
                task_id: "t".into(),
                pct: 92.0,
                window_mins: 5,
            }
            .suggested_action(),
            StallAction::Resize
        );
        assert_eq!(
            StallSignal::RuntimeOverExpected {
                task_id: "t".into(),
                actual_secs: 2000,
                expected_secs: 500,
            }
            .suggested_action(),
            StallAction::Abort
        );
    }

    #[test]
    fn suggest_resize_bumps_r6i_family() {
        let sig = StallSignal::MemoryPressure {
            task_id: "t".into(),
            pct: 95.0,
            window_mins: 5,
        };
        assert_eq!(
            suggest_resize(&sig, "r6i.xlarge"),
            Some("r6i.2xlarge".to_string())
        );
        assert_eq!(
            suggest_resize(&sig, "r6i.4xlarge"),
            Some("r6i.8xlarge".to_string())
        );
        // Top of family: no further resize possible.
        assert_eq!(suggest_resize(&sig, "r6i.8xlarge"), None);
    }

    #[test]
    fn suggest_resize_pivots_from_compute_to_memory() {
        let sig = StallSignal::MemoryPressure {
            task_id: "t".into(),
            pct: 92.0,
            window_mins: 5,
        };
        assert_eq!(
            suggest_resize(&sig, "c6i.2xlarge"),
            Some("r6i.2xlarge".to_string())
        );
        assert_eq!(
            suggest_resize(&sig, "t3.medium"),
            Some("r6i.xlarge".to_string())
        );
    }

    #[test]
    fn suggest_resize_returns_none_for_non_memory_signals() {
        let cpu_sig = StallSignal::CpuStarvation {
            task_id: "t".into(),
            avg_cpu_pct: 1.0,
            window_mins: 30,
        };
        assert!(suggest_resize(&cpu_sig, "r6i.xlarge").is_none());

        let runtime_sig = StallSignal::RuntimeOverExpected {
            task_id: "t".into(),
            actual_secs: 9000,
            expected_secs: 3600,
        };
        assert!(suggest_resize(&runtime_sig, "r6i.xlarge").is_none());
    }

    #[test]
    fn suggest_resize_none_for_unknown_instance() {
        let sig = StallSignal::MemoryPressure {
            task_id: "t".into(),
            pct: 95.0,
            window_mins: 5,
        };
        assert!(suggest_resize(&sig, "fake.unknown").is_none());
    }

    #[test]
    fn env_parsing_overrides_defaults() {
        unsafe {
            env::set_var("SWFC_STALL_ENABLED", "1");
            env::set_var("SWFC_STALL_CPU_MIN_PCT", "10");
            env::set_var("SWFC_STALL_CPU_WINDOW_MINS", "60");
            env::set_var("SWFC_STALL_MEM_MAX_PCT", "85");
        }
        let t = StallThresholds::from_env();
        assert!(t.enabled);
        assert_eq!(t.cpu_min_pct, 10.0);
        assert_eq!(t.cpu_window_mins, 60);
        assert_eq!(t.mem_max_pct, 85.0);
        unsafe {
            env::remove_var("SWFC_STALL_ENABLED");
            env::remove_var("SWFC_STALL_CPU_MIN_PCT");
            env::remove_var("SWFC_STALL_CPU_WINDOW_MINS");
            env::remove_var("SWFC_STALL_MEM_MAX_PCT");
        }
    }

    #[test]
    fn stall_signal_sidecar_append_creates_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pkg_root = tmp.path();

        let signal = StallSignal::MemoryPressure {
            task_id: "align_reads_1".into(),
            pct: 94.5,
            window_mins: 5,
        };
        append_stall_signal_record(pkg_root, &signal);

        let sidecar = pkg_root.join("runtime").join("stall-signals.jsonl");
        assert!(sidecar.exists(), "stall-signals.jsonl was not created");

        let content = std::fs::read_to_string(&sidecar).expect("read sidecar");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 1, "expected exactly one JSON line");

        let rec: serde_json::Value = serde_json::from_str(lines[0]).expect("line is valid JSON");
        assert_eq!(rec["kind"], "memory_pressure", "kind field mismatch");
        assert_eq!(rec["task_id"], "align_reads_1", "task_id field mismatch");
        assert_eq!(
            rec["suggested_action"], "resize",
            "suggested_action mismatch"
        );
        assert!(rec["ts"].is_string(), "ts must be a string");
        assert!(
            rec["measurements"]["pct"].is_number(),
            "measurements.pct missing"
        );
    }
}
