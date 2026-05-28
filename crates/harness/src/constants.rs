//! Centralized operational defaults for the harness. Every value here has an
//! env-var override; this module documents the *baked-in defaults* an
//! operator gets out of the box.
//!
//! When adding a constant:
//! 1. Document the env-var override.
//! 2. Document why this specific default.
//! 3. If it's a clamp, name MIN/MAX siblings.

use std::time::Duration;

// ---- Heartbeat & stall detection ----

/// Default `ECAA_TASK_HEARTBEAT_STALL_SECS`. Above this gap between heartbeat
/// touches, a task is assumed dead and gets reaped. 15 min covers the worst
/// observed batch-correction stall on a healthy r6i.4xlarge.
pub const HEARTBEAT_STALL_THRESHOLD_SECS_DEFAULT: u64 = 900;

/// Default `ECAA_HEARTBEAT_LIVENESS_SECS`. Window within which an in-flight
/// agent process must touch its heartbeat to be considered live. Paired
/// with the agent-side 30s touch cadence (see scripts/agent-claude.sh).
pub const HEARTBEAT_LIVENESS_WINDOW_SECS_DEFAULT: u64 = 60;
/// Upper clamp on `ECAA_HEARTBEAT_LIVENESS_SECS`. Values above this are
/// rejected so a misconfigured operator can't disable the liveness window.
pub const HEARTBEAT_LIVENESS_WINDOW_SECS_MAX: u64 = 600;

// ---- Harness main loop ----

/// Default `ECAA_HARNESS_SETTLE_SECS`. Sleep between iterations when no
/// task is ready (idle-compute-waiting). Trade-off: shorter = more
/// responsive when a task becomes ready, longer = less CPU burn.
pub const HARNESS_SETTLE_INTERVAL_SECS_DEFAULT: u64 = 60;
/// Minimum value accepted for `ECAA_HARNESS_SETTLE_SECS`. Prevents hot-spin.
pub const HARNESS_SETTLE_INTERVAL_SECS_MIN: u64 = 5;
/// Maximum value accepted for `ECAA_HARNESS_SETTLE_SECS`. Above this the
/// operator is warned and the value is clamped.
pub const HARNESS_SETTLE_INTERVAL_SECS_MAX: u64 = 1800;

/// Default `--task-timeout` CLI arg (seconds). 5 minutes covers all
/// non-execution stages (build, validate, discover_*). Execution stages
/// override per-profile via compute-profiles/profiles.yaml.
pub const TASK_TIMEOUT_SECS_DEFAULT: u64 = 300;

// ---- ProgressClient ----

/// Bounded mpsc capacity for the progress-event sender thread.
/// Overflow silently drops events; WAL recovery sees ~256
/// `task_blocked` events in pathological replays, hence the conservative cap.
pub const PROGRESS_SENDER_QUEUE_CAPACITY: usize = 256;

/// TCP connect timeout for progress-event POST requests. Short so a
/// dead server doesn't stall the harness main loop for long.
pub const PROGRESS_HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Total request timeout for progress-event POST requests. Covers the
/// connect + write + read round-trip; larger than `PROGRESS_HTTP_CONNECT_TIMEOUT`
/// to allow for a slow but alive server.
pub const PROGRESS_HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Exponential backoff schedule for POST retries on transient failures.
pub const PROGRESS_SENDER_RETRY_BACKOFFS_MS: &[u64] = &[100, 500, 2000];

/// Write the rolling health sidecar every Nth post.
pub const PROGRESS_HEALTH_SIDECAR_WRITE_CADENCE: u64 = 10;

// ---- Stall monitor (defaults; envs ECAA_STALL_*) ----

/// Minimum average CPU utilisation (%) below which a task is flagged as
/// CPU-starved. Overridden by `ECAA_STALL_CPU_MIN_PCT`.
pub const STALL_CPU_MIN_PCT_DEFAULT: f32 = 5.0;
/// Rolling window (minutes) for the CPU-starvation check.
/// Overridden by `ECAA_STALL_CPU_WINDOW_MINS`.
pub const STALL_CPU_WINDOW_MINS_DEFAULT: u64 = 30;
/// Maximum average memory utilisation (%) above which a task is flagged
/// as under memory pressure. Overridden by `ECAA_STALL_MEM_MAX_PCT`.
pub const STALL_MEM_MAX_PCT_DEFAULT: f32 = 90.0;
/// Rolling window (minutes) for the memory-pressure check.
/// Overridden by `ECAA_STALL_MEM_WINDOW_MINS`.
pub const STALL_MEM_WINDOW_MINS_DEFAULT: u64 = 5;
/// Minutes of consecutive GPU idle before the GPU-idle stall fires.
/// Overridden by `ECAA_STALL_GPU_IDLE_MINS`.
pub const STALL_GPU_IDLE_MINS_DEFAULT: u64 = 15;
/// Multiplier on `expected_wall_seconds`; when actual runtime exceeds
/// `expected × multiplier` the runtime-over-expected stall fires.
/// Overridden by `ECAA_STALL_RUNTIME_OVER_EXPECTED_MULT`.
pub const STALL_RUNTIME_OVER_EXPECTED_MULT_DEFAULT: f32 = 2.0;
/// How often the stall monitor samples system metrics (seconds).
/// Overridden by `ECAA_STALL_SAMPLE_INTERVAL_SECS`.
pub const STALL_SAMPLE_INTERVAL_SECS_DEFAULT: u64 = 30;

/// P0-40 — quiescent window the stall monitor requires before it
/// resets a fired latch. Once a signal fires, the monitor refuses to
/// re-fire that `(task_id, kind)` until either a) it has observed
/// `STALL_LATCH_QUIESCENT_SECS_DEFAULT` of continuous healthy samples,
/// or b) the running task id flips. Without this, the latch wedges
/// after a transient recovery and a second genuine stall on the same
/// task goes unreported. 300 s mirrors the post-recovery heartbeat
/// reset window used elsewhere in the harness.
pub const STALL_LATCH_QUIESCENT_SECS_DEFAULT: u64 = 300;

/// P0-40 — minimum gap between two identical
/// `(task_id, signal-kind)` emissions even when the latch has reset
/// (i.e. the signal genuinely re-fired). Prevents a high-rate-sample
/// configuration from spamming the channel when a task is durably
/// stalled. Defaults to twice the latch quiescent window.
pub const STALL_RATE_LIMIT_SECS_DEFAULT: u64 = 600;

// ---- Wall-clock watchdog (env vars ECAA_WATCHDOG_*) ----

/// Default `ECAA_WATCHDOG_PERIOD_SECS`. The watchdog polls every 30 s —
/// fast enough to catch runaway tasks within one period of the multiplied
/// budget, cheap enough not to burn CPU.
pub const WATCHDOG_PERIOD_SECS_DEFAULT: u64 = 30;
/// Minimum value accepted for `ECAA_WATCHDOG_PERIOD_SECS`.
pub const WATCHDOG_PERIOD_SECS_MIN: u64 = 10;
/// Maximum value accepted for `ECAA_WATCHDOG_PERIOD_SECS`.
pub const WATCHDOG_PERIOD_SECS_MAX: u64 = 600;

/// Default `ECAA_WATCHDOG_MULTIPLIER`. 6× means a task expected to run for
/// 30 min is given 3 h before the watchdog fires. Chosen so the watchdog
/// catches CPU-bound infinite loops without tripping on legitimately slow
/// stages that finish eventually.
pub const WATCHDOG_MULTIPLIER_DEFAULT: f64 = 6.0;
/// Minimum value accepted for `ECAA_WATCHDOG_MULTIPLIER`.
pub const WATCHDOG_MULTIPLIER_MIN: f64 = 1.5;
/// Maximum value accepted for `ECAA_WATCHDOG_MULTIPLIER`.
pub const WATCHDOG_MULTIPLIER_MAX: f64 = 100.0;

/// Fallback `expected_wall_seconds` when the task's spec carries no budget.
/// 1800 s (30 min) × default multiplier (6.0) = 3 h default hard limit.
pub const WATCHDOG_FALLBACK_EXPECTED_WALL_SECS: u64 = 1800;

// ---- Orphan reaping & SSM ----

/// How long the orphan-verification poll loop waits for a definitive
/// alive/dead verdict before giving up and flagging as dead.
pub const ORPHAN_VERIFICATION_POLL_TIMEOUT_SECS_DEFAULT: u64 = 300;
/// Polling interval (seconds) between orphan-liveness checks.
pub const ORPHAN_VERIFICATION_POLL_INTERVAL_SECS: u64 = 10;
/// Timeout (seconds) for SSM command execution when no per-stage value
/// is configured. 1 h covers the longest bioinformatics stages observed.
pub const FALLBACK_SSM_TIMEOUT_SECS: u64 = 3600;
/// Seconds the AWS executor waits for SSM agent readiness after
/// the EC2 instance enters `running` state.
pub const AWS_SSM_PROVISION_WAIT_SECS: u64 = 120;

// ---- SLURM defaults ----

/// How often (seconds) the SLURM executor polls `sacct` / `squeue` to
/// check job status.
pub const SLURM_POLL_INTERVAL_SECS_DEFAULT: u64 = 20;
/// Maximum time (seconds) to wait for a SLURM job to leave the queue
/// before the executor flags it as stuck. Default is 6 hours.
pub const SLURM_MAX_QUEUE_WAIT_SECS_DEFAULT: u64 = 21600; // 6 h
/// Default `--time` value passed to `sbatch` when no per-stage profile
/// overrides the wall limit.
pub const SLURM_DEFAULT_TIME_LIMIT: &str = "04:00:00";

// ---- CloudWatch ----

/// CloudWatch look-back window (minutes) when fetching instance metrics
/// for stall-detection and pilot sizing.
pub const CLOUDWATCH_METRICS_LOOKBACK_MINS: i64 = 15;
/// CloudWatch aggregation period (seconds) for per-metric data points.
pub const CLOUDWATCH_METRICS_PERIOD_SECS: u64 = 60;

// ---- Output tail caps ----

/// Tail-bytes cap for captured stdout/stderr in error envelopes. 4 KB is
/// large enough to contain typical traceback + last few error lines while
/// staying within the SSM response size envelope.
pub const OUTPUT_TAIL_BYTES: usize = 4096;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn settle_interval_bounds_are_consistent() {
        assert!(HARNESS_SETTLE_INTERVAL_SECS_MIN < HARNESS_SETTLE_INTERVAL_SECS_DEFAULT);
        assert!(HARNESS_SETTLE_INTERVAL_SECS_DEFAULT < HARNESS_SETTLE_INTERVAL_SECS_MAX);
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn liveness_window_bounds_are_consistent() {
        assert!(HEARTBEAT_LIVENESS_WINDOW_SECS_DEFAULT < HEARTBEAT_LIVENESS_WINDOW_SECS_MAX);
    }
}
