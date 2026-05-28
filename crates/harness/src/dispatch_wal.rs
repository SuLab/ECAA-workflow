//! Crash-safe dispatch write-ahead log.
//!
//! Appends one line per task dispatch to
//! `<package>/runtime/dispatches.jsonl` before the agent subprocess
//! spawns. On harness startup, [`recover_orphaned_dispatches`] reads
//! every prior-run entry whose task is still in `Running` state and
//! re-blocks it with a `[orphaned_by_crash]` marker the server's
//! blocker mapper promotes to `BlockerKind::OrphanedByCrash`.
//!
//! Design notes:
//! - Append-only JSON Lines; each line is a self-contained
//!   `DispatchRecord`. Crashes mid-write leave a partial line — the
//!   reader drops unparseable lines without aborting (W1.2 surfaces
//!   the drop count via the `_observability` silent-skip counter so
//!   the harness-health sidecar shows it).
//! - Per-process `harness_run_id` is a short random hex (`uuid_short`
//!   lives in core, but the harness already does ad-hoc random in
//!   other places; we use a 16-char hex here to keep the dep graph
//!   small).
//! - Sync file I/O; no tokio. `OpenOptions::append(true)` + fsync per
//!   line keeps the WAL durable even if the process is SIGKILLed
//!   seconds later.
//!
//! ## Recovery semantics — the `timeout_at`-future skip window (W7.1)
//!
//! Naively, any WAL entry whose `harness_run_id` differs from the
//! current process's id is an "orphan" to be recovered. That false-
//! positives in a well-defined scenario:
//!
//! 1. Harness A dispatches task T at `t0` with `timeout_at = t0 + 5min`.
//! 2. A exits (clean or crash) before T completes.
//! 3. The SME hits `/unblock` or `/start-execution` at `t0 + 30s`.
//! 4. New harness B starts at `t0 + 31s`.
//! 5. Without a guard, B would treat A's record as orphan → re-block
//!    T immediately, often interrupting a still-running agent.
//!
//! [`prior_run_dispatches`] adds the deadline guard: a record is
//! candidate for orphan flagging only when *both* `harness_run_id`
//! differs *and* `timeout_at < now`. The 5-minute timeout means real
//! crash recovery still kicks in once the deadline passes — the
//! safety net is preserved, just narrowed to actually-stale dispatches.
//!
//! The tradeoff: a task with a very long `timeout_at` that hits a real
//! agent crash but a fresh `.heartbeat` (from a pre-crash touch) won't
//! be recovered until the timeout expires. This is by design and
//! catalogued as **SE-8** in `docs/harness-sharp-edges.md`. The
//! followup W5.4 (heartbeat PID check via `kill(pid, 0)` + WAL schema
//! v4 with `agent_pid`) closes the remaining false-negative window.

use anyhow::Result;
use ecaa_workflow_core::clock::Clock;
use ecaa_workflow_core::dag::{BlockedRecord, TaskState, DAG};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Probe that verifies whether a task believed to be `Running` shows
/// fresh evidence of liveness — used by the orphan-by-crash recovery
/// to suppress false positives.
///
/// Why this exists: long-running detached compute (Seurat CCA, etc.)
/// is owned by an agent-side bash wrapper that touches the task's
/// `.heartbeat` file every 30s, INDEPENDENT of any harness process.
/// When the harness exits at `--max-iterations` and a new harness
/// starts, the prior-run WAL entries for those tasks are still present
/// — but the actual compute is alive and progressing. Without a
/// liveness check the new harness flags every such task
/// `[orphaned_by_crash]`, blocking the session and forcing a manual
/// `/unblock` for every restart cycle.
///
/// Probes are trait objects so production code can use the on-disk
/// heartbeat ([`HeartbeatLivenessProbe`]) while tests can inject a
/// `MockProbe { live_ids: HashSet<String> }`. [`AlwaysDeadProbe`]
/// recovers the legacy "no liveness check" behavior — used in tests
/// that exercise the post-recovery state, and via the
/// `SWFC_HEARTBEAT_LIVENESS_SECS=0` operator opt-out.
pub trait LivenessProbe {
    /// Returns `true` when `task_id`'s agent process is believed to be alive.
    fn is_live(&self, task_id: &str) -> bool;
}

/// Default probe: read `<package>/runtime/outputs/<task_id>/.heartbeat`
/// mtime, return true when it's within `freshness_secs` of now. False
/// when the file is missing, unreadable, or older than the threshold —
/// any of which indicates the orphan recovery SHOULD proceed.
pub struct HeartbeatLivenessProbe {
    /// Root directory of the package (contains `runtime/outputs/`).
    pub package_root: PathBuf,
    /// Age threshold in seconds: heartbeat files older than this are treated as dead.
    pub freshness_secs: u64,
}

impl LivenessProbe for HeartbeatLivenessProbe {
    fn is_live(&self, task_id: &str) -> bool {
        let path = self
            .package_root
            .join("runtime/outputs")
            .join(task_id)
            .join(".heartbeat");
        let Ok(meta) = std::fs::metadata(&path) else {
            return false;
        };
        let Ok(modified) = meta.modified() else {
            return false;
        };
        let Ok(elapsed) = modified.elapsed() else {
            return false;
        };
        if elapsed.as_secs() > self.freshness_secs {
            return false;
        }
        // W5.4: fresh heartbeat is necessary but not sufficient. The
        // `agent-pgrep-deadlock` incident showed that a zombie polling
        // loop can keep touching .heartbeat after the real agent dies.
        // When `.agent-pid` is also present, require the recorded PID
        // to still be alive — kill(pid, 0) is the standard liveness
        // probe (no signal delivered). When the sidecar is absent
        // (remote executors, older harness builds), fall back to the
        // heartbeat-only check so we don't regress existing setups.
        let pid_path = self
            .package_root
            .join("runtime/outputs")
            .join(task_id)
            .join(".agent-pid");
        match std::fs::read_to_string(&pid_path) {
            Ok(contents) => {
                let Some(token) = contents.split_whitespace().next() else {
                    return true; // sidecar present but malformed — degrade to mtime-only
                };
                let Ok(pid) = token.parse::<i32>() else {
                    return true;
                };
                if pid <= 0 {
                    return true;
                }
                // SAFETY: libc::kill(pid, 0) is the standard POSIX
                // liveness probe — no signal delivered, just kernel
                // permission/existence check. The workspace
                // `unsafe_code = "deny"` lint requires this bounded
                // waiver; same pattern as multiprocess_lock::is_pid_alive.
                let rc = {
                    #[allow(unsafe_code)]
                    unsafe {
                        libc::kill(pid, 0)
                    }
                };
                if rc == 0 {
                    return true;
                }
                // SAFETY: errno read immediately after the syscall is
                // the documented POSIX pattern.
                let errno = {
                    #[allow(unsafe_code)]
                    unsafe {
                        *libc::__errno_location()
                    }
                };
                // EPERM means "process exists but we can't signal it"
                // — still alive. ESRCH means dead.
                errno == libc::EPERM
            }
            Err(_) => true, // No sidecar — degrade to mtime-only.
        }
    }
}

/// Probe that always reports dead. Selected by the legacy operator
/// opt-out (`SWFC_HEARTBEAT_LIVENESS_SECS=0`) and used in unit tests
/// that want the pre-liveness orphan-flagging semantics.
pub struct AlwaysDeadProbe;

impl LivenessProbe for AlwaysDeadProbe {
    fn is_live(&self, _task_id: &str) -> bool {
        false
    }
}

/// Schema version stamped on every newly-written `DispatchRecord`.
/// Readers downgrade gracefully when they see an
/// unfamiliar version (skip the record + log a warning rather than
/// panic). Bump when the on-disk shape changes.
///
/// v3 P7 — promoted to a SemVer constructor so the canonical version
/// lives alongside the other IR types in `core::migration`.
pub fn dispatch_wal_schema_version() -> semver::Version {
    ecaa_workflow_core::migration::current_dispatch_wal_version()
}

/// One dispatch log entry. Serialized as a single JSON Lines row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchRecord {
    /// On-disk schema version. `#[serde(default = "...")]`
    /// returns the canonical SemVer for records persisted before this
    /// field existed, so pre-S11.15 WAL entries continue to load
    /// without migration. Future readers that see an unrecognized
    /// version must skip-and-warn rather than panic.
    ///
    /// v3 P7 — promoted from `u32` to `semver::Version`. The
    /// `schema_version_serde` adapter accepts both legacy `u64` JSON
    /// values and canonical SemVer strings on read; writes the
    /// canonical SemVer string.
    #[serde(
        default = "default_dispatch_wal_schema_version",
        with = "ecaa_workflow_core::migration::schema_version_serde"
    )]
    pub schema_version: semver::Version,
    /// Task id being dispatched.
    pub task_id: String,
    /// Monotonically-increasing dispatch epoch within a harness run.
    pub epoch: u64,
    /// The harness process's unique id, stamped on every dispatch.
    /// Startup recovery compares this to the *current* run id to
    /// detect orphans.
    pub harness_run_id: String,
    /// RFC 3339 dispatch timestamp.
    pub started_at: String,
    /// RFC 3339 timestamp at which the dispatch is considered stale
    /// (typically `started_at + task_timeout_secs`).
    pub timeout_at: String,
}

fn default_dispatch_wal_schema_version() -> semver::Version {
    dispatch_wal_schema_version()
}

/// Return value from a successful startup recovery sweep — used to
/// log a summary line + feed metrics.
///
/// `skipped_live_*` counts tasks the recovery would have flagged as
/// orphan but skipped because the [`LivenessProbe`] reported them
/// alive. Surfaced in the operator log so harness restarts that
/// would otherwise have churned a /unblock dance are observable.
#[derive(Debug, Clone, Default)]
pub struct RecoveryReport {
    /// Number of tasks that were flagged orphaned and blocked.
    pub orphaned_count: usize,
    /// Task ids that were flagged as orphaned.
    pub orphaned_task_ids: Vec<String>,
    /// Number of tasks skipped by the liveness probe (heartbeat still fresh).
    pub skipped_live_count: usize,
    /// Task ids that were skipped because the heartbeat was fresh.
    pub skipped_live_task_ids: Vec<String>,
}

/// Path of the dispatch log under the package root.
pub fn wal_path(package_root: &Path) -> std::path::PathBuf {
    package_root.join("runtime/dispatches.jsonl")
}

/// Collect the set of EC2/remote instance_ids that WORKFLOW.json records
/// as currently Running (with a `remote` field set). Used by the AWS orphan
/// reaper's WAL cross-check: only instances that appear here were launched
/// by this harness and are eligible for reap; any candidate with matching
/// tags but absent from this set is a likely tag-spoof and is skipped.
///
/// Reads `<package_root>/WORKFLOW.json`. Returns an empty set when the file
/// is missing, unreadable, or contains no Running+remote tasks — the reaper
/// degrades to tag-only filtering rather than refusing to operate.
pub fn instance_ids_from_workflow_json(package_root: &Path) -> std::collections::BTreeSet<String> {
    let path = package_root.join("WORKFLOW.json");
    let Ok(raw) = crate::swfc_io::read_capped(&path, crate::swfc_io::resolve_max_bytes()) else {
        return std::collections::BTreeSet::new();
    };
    let Ok(dag) = serde_json::from_str::<DAG>(&raw) else {
        return std::collections::BTreeSet::new();
    };
    dag.tasks
        .values()
        .filter_map(|task| {
            if let TaskState::Running { remote, .. } = &task.state {
                remote.as_ref().map(|r| r.instance_id.clone())
            } else {
                None
            }
        })
        .filter(|id| !id.is_empty())
        .collect()
}

/// Generate a short random run id (16 hex chars). Uses nanosecond
/// timestamp XOR'd with the process pid so the value is unique per
/// harness invocation without pulling in a uuid dep.
pub fn generate_harness_run_id() -> String {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0) as u64;
    let pid = std::process::id() as u64;
    format!("{:016x}", now_ns ^ pid.wrapping_mul(0x9E37_79B9_7F4A_7C15))
}

/// Append a single dispatch record. Ensures the parent directory
/// exists. Errors are returned so the caller can decide whether to
/// abort (the main harness flow logs + continues — a WAL write
/// failure shouldn't stop a dispatch that would otherwise proceed,
/// because the task state in WORKFLOW.json is still authoritative).
pub fn append_dispatch(package_root: &Path, record: &DispatchRecord) -> Result<()> {
    let path = wal_path(package_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    // Compose the JSON + trailing newline into ONE buffer and issue a
    // single `write_all` call. POSIX guarantees atomicity of a single
    // O_APPEND write below PIPE_BUF (4 KiB on Linux); two separate
    // writes can interleave under concurrent appenders and surface as
    // torn lines in the WAL.
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    f.write_all(line.as_bytes())?;
    f.sync_data()?; // durable before agent spawn
    Ok(())
}

/// Truncate the WAL to zero length. Called on every clean harness
/// exit (max-iterations reached, all tasks complete, /execution/stop)
/// so the next harness start sees an empty WAL and skips the
/// orphan-recovery scan altogether — no false positives from
/// dispatches that completed normally.
///
/// Best-effort: missing file is treated as already-truncated. Errors
/// from filesystem operations are returned for the caller to log but
/// must not block exit (a failed truncation degrades to "next start
/// runs orphan recovery on stale entries", same as today).
pub fn truncate_wal(package_root: &Path) -> Result<()> {
    let path = wal_path(package_root);
    if !path.exists() {
        return Ok(());
    }
    std::fs::write(&path, b"")?;
    Ok(())
}

/// Read the WAL, returning each parseable record in order. Unparseable
/// lines are skipped (partial writes from a crash) but surfaced via
/// the `_observability` silent-skip counter so operators see the
/// count in `harness-health.json` even when individual lines aren't
/// worth a full log entry.
pub fn read_dispatches(package_root: &Path) -> Vec<DispatchRecord> {
    let path = wal_path(package_root);
    let Ok(f) = std::fs::File::open(&path) else {
        return Vec::new();
    };
    let reader = BufReader::new(f);
    let mut out = Vec::new();
    for (idx, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                // W1.2: torn read (rare — usually a truncation race).
                crate::_observability::note_silent_skip(
                    crate::_observability::SkipCategory::WalTornLine,
                    &format!("WAL line {} read failed: {}", idx, e),
                    None,
                );
                continue;
            }
        };
        match serde_json::from_str::<DispatchRecord>(&line) {
            Ok(rec) => out.push(rec),
            Err(e) => {
                // W1.2: line present but parse fails — typical when a
                // crash wrote a half-line before fsync. Counting these
                // lets us notice if the crash rate climbs.
                crate::_observability::note_silent_skip(
                    crate::_observability::SkipCategory::WalTornLine,
                    &format!("WAL line {} parse failed: {}", idx, e),
                    None,
                );
            }
        }
    }
    out
}

/// Find the last dispatch for each task id — the one the harness
/// would need to reconcile against WORKFLOW.json at startup. Returns
/// only records from runs other than `current_run_id` whose
/// `timeout_at` has already passed (i.e., genuinely orphaned, not
/// just "in-flight on a different live harness run we don't know
/// about yet").
///
/// The `timeout_at` guard prevents the orphan-recovery loop that
/// fires when:
/// - harness A dispatches task T at t0 with timeout_at = t0 + 5min
/// - harness A exits (clean or crash) before T completes
/// - the SME hits /unblock or /start-execution at t0 + 30s
/// - new harness B starts at t0 + 31s
/// - without the guard, B sees A's dispatch as "prior run" → orphan
/// - WITH the guard, B sees timeout_at is still in the future → skip
///
/// The 5-minute timeout means a real crash recovery still kicks in
/// once the prior run's dispatch deadline passes — the safety net is
/// preserved, just narrowed to actually-stale dispatches.
pub fn prior_run_dispatches(
    records: &[DispatchRecord],
    current_run_id: &str,
    clock: &dyn Clock,
) -> std::collections::BTreeMap<String, DispatchRecord> {
    let now = clock.now();
    let mut latest: std::collections::BTreeMap<String, DispatchRecord> =
        std::collections::BTreeMap::new();
    for r in records {
        if r.harness_run_id == current_run_id {
            continue;
        }
        // Skip dispatches whose deadline hasn't passed. The dispatch
        // could still be in-flight on a parallel harness run we're
        // not aware of (e.g. in development, or after an /unblock
        // races with auto_relaunch). Only flag as orphan once the
        // deadline is genuinely stale.
        if let Ok(timeout_at) = chrono::DateTime::parse_from_rfc3339(&r.timeout_at) {
            if now < timeout_at.with_timezone(&chrono::Utc) {
                continue;
            }
        }
        latest.insert(r.task_id.clone(), r.clone());
    }
    latest
}

/// Walk every task still in `Running` state whose last WAL entry is
/// from a *different* harness run; flip to
/// `Blocked { [orphaned_by_crash prior_run=X at=Y] }` so the server
/// mapper produces a typed `BlockerKind::OrphanedByCrash` the UI can
/// dispatch a rerun for. Returns the ids of re-blocked tasks so the
/// caller can log + emit progress events.
///
/// The `liveness_probe` short-circuits the re-block when the task's
/// detached compute is provably still running (heartbeat fresh). Pass
/// [`AlwaysDeadProbe`] to skip the liveness check entirely (legacy
/// behavior — every Running task with a prior-run WAL entry whose
/// deadline passed gets flagged).
pub fn recover_orphaned_dispatches(
    dag: &mut DAG,
    records: &[DispatchRecord],
    current_run_id: &str,
    liveness_probe: &dyn LivenessProbe,
    clock: &dyn Clock,
) -> RecoveryReport {
    recover_orphaned_dispatches_with_denylist(
        dag,
        records,
        current_run_id,
        liveness_probe,
        &std::collections::HashSet::new(),
        clock,
    )
}

/// P1-226 — like `recover_orphaned_dispatches` but consults
/// `instance_denylist` first. When a task's
/// `Running { remote: Some { instance_id, .. } }` matches an id in
/// the denylist, the recovery skips the liveness probe and re-blocks
/// the task outright: the AWS orphan reap already terminated the
/// host, so any stale heartbeat file is necessarily a no-op artefact
/// from before the kill.
///
/// Reordering the harness startup so the AWS sweep fires BEFORE the
/// WAL recovery (and feeds the denylist) closes a window where:
///
///   1. Harness A crashed mid-task.
///   2. A's agent process is dead but the heartbeat file mtime is
///      fresh (the agent flushed it just before the crash).
///   3. The reaper terminates A's EC2 instance.
///   4. The (legacy ordering) WAL recovery sees the fresh heartbeat
///      and treats the task as live — but the host is already gone.
///   5. The task wedges in Running forever until a manual unblock.
///
/// With the denylist, step (4) instead recognises the instance was
/// just killed and immediately re-blocks the task so the SME sees a
/// BlockerCard.
pub fn recover_orphaned_dispatches_with_denylist(
    dag: &mut DAG,
    records: &[DispatchRecord],
    current_run_id: &str,
    liveness_probe: &dyn LivenessProbe,
    instance_denylist: &std::collections::HashSet<String>,
    clock: &dyn Clock,
) -> RecoveryReport {
    let latest_prior = prior_run_dispatches(records, current_run_id, clock);
    let mut report = RecoveryReport::default();
    for (tid, task) in dag.tasks.iter_mut() {
        let TaskState::Running { remote, .. } = &task.state else {
            continue;
        };
        let task_instance_id = remote.as_ref().map(|r| r.instance_id.clone());
        let Some(last) = latest_prior.get(tid.as_str()) else {
            continue;
        };
        // P1-226 — denylist short-circuit. The instance is already
        // terminated; the heartbeat file (fresh or stale) cannot
        // possibly reflect a live agent.
        let killed_by_reaper = task_instance_id
            .as_ref()
            .is_some_and(|iid| instance_denylist.contains(iid));
        if !killed_by_reaper {
            // Liveness short-circuit. A fresh heartbeat means the task's
            // detached subprocess (BPCells/Seurat wrapper, agent
            // heartbeat fork, etc.) is alive and progressing
            // independent of any harness lifecycle — the orphan-by-crash
            // assumption (subprocess died with the harness) does not
            // hold. Skip without re-blocking.
            if liveness_probe.is_live(tid.as_str()) {
                report.skipped_live_count += 1;
                report.skipped_live_task_ids.push(tid.to_string());
                continue;
            }
        }
        let reason = if killed_by_reaper {
            format!(
                "[orphaned_by_crash] task={} prior_run={} at={} — instance {} terminated by reaper. Rerun to resume.",
                tid,
                last.harness_run_id,
                last.started_at,
                task_instance_id.clone().unwrap_or_default(),
            )
        } else {
            format!(
                "[orphaned_by_crash] task={} prior_run={} at={} — recovered from prior harness run. Rerun to resume.",
                tid, last.harness_run_id, last.started_at,
            )
        };
        task.state = TaskState::Blocked {
            record: BlockedRecord {
                reason,
                attempts: vec![],
            },
        };
        report.orphaned_count += 1;
        report.orphaned_task_ids.push(tid.to_string());
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::clock::{FrozenClock, WallClock};
    use ecaa_workflow_core::dag::{Assignee, ResourceClass, Task, TaskKind};
    use std::collections::BTreeMap;

    fn running_task() -> Task {
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Running {
                started_at: "2026-04-23T14:00:00Z".into(),
                remote: None,
            },
            depends_on: vec![],
            assignee: Assignee::Agent,
            description: "".into(),
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

    #[test]
    fn append_and_read_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = DispatchRecord {
            schema_version: dispatch_wal_schema_version(),
            task_id: "t1".into(),
            epoch: 1,
            harness_run_id: "abc".into(),
            started_at: "2026-04-23T14:00:00Z".into(),
            timeout_at: "2026-04-23T14:30:00Z".into(),
        };
        append_dispatch(tmp.path(), &rec).unwrap();
        let back = read_dispatches(tmp.path());
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].task_id, "t1");
        assert_eq!(back[0].epoch, 1);
    }

    #[test]
    fn partial_line_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let rec = DispatchRecord {
            schema_version: dispatch_wal_schema_version(),
            task_id: "t1".into(),
            epoch: 1,
            harness_run_id: "abc".into(),
            started_at: "2026-04-23T14:00:00Z".into(),
            timeout_at: "2026-04-23T14:30:00Z".into(),
        };
        append_dispatch(tmp.path(), &rec).unwrap();
        // Append a truncated line that looks like a crash-during-write.
        let path = wal_path(tmp.path());
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"task_id\":\"t2\",\"ep\n").unwrap();
        drop(f);
        let back = read_dispatches(tmp.path());
        assert_eq!(back.len(), 1, "partial line must not crash the reader");
        assert_eq!(back[0].task_id, "t1");
    }

    #[test]
    fn recovery_skips_prior_run_tasks_within_timeout_window() {
        // The fix for the post-restart orphan-recovery loop: when a
        // prior-run dispatch's timeout_at is still in the future,
        // treat it as "potentially in-flight" rather than orphaned.
        // Stops the false-fire when /unblock + auto_relaunch races
        // with a freshly-spawned new harness.
        let mut dag = DAG {
            version: "v1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks: {
                let mut m = BTreeMap::new();
                m.insert("stuck".into(), running_task());
                m
            },
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };
        let future = chrono::Utc::now() + chrono::Duration::hours(1);
        let records = vec![DispatchRecord {
            schema_version: dispatch_wal_schema_version(),
            task_id: "stuck".into(),
            epoch: 1,
            harness_run_id: "prior-run-id".into(),
            started_at: chrono::Utc::now().to_rfc3339(),
            // Deadline still 1h away — must NOT be flagged orphan.
            timeout_at: future.to_rfc3339(),
        }];
        let report = recover_orphaned_dispatches(
            &mut dag,
            &records,
            "current-run-id",
            &AlwaysDeadProbe,
            &WallClock,
        );
        assert_eq!(
            report.orphaned_count, 0,
            "dispatches whose timeout hasn't passed yet must not be orphaned"
        );
        assert!(matches!(
            dag.tasks.get("stuck").unwrap().state,
            TaskState::Running { .. }
        ));
    }

    #[test]
    fn recovery_reblocks_prior_run_tasks() {
        let mut dag = DAG {
            version: "v1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks: {
                let mut m = BTreeMap::new();
                m.insert("stuck".into(), running_task());
                m
            },
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };
        let records = vec![DispatchRecord {
            schema_version: dispatch_wal_schema_version(),
            task_id: "stuck".into(),
            epoch: 1,
            harness_run_id: "prior-run-id".into(),
            started_at: "2026-04-23T14:00:00Z".into(),
            timeout_at: "2026-04-23T14:30:00Z".into(),
        }];
        let report = recover_orphaned_dispatches(
            &mut dag,
            &records,
            "current-run-id",
            &AlwaysDeadProbe,
            &WallClock,
        );
        assert_eq!(report.orphaned_count, 1);
        assert_eq!(report.orphaned_task_ids, vec!["stuck".to_string()]);
        let state = &dag.tasks.get("stuck").unwrap().state;
        match state {
            TaskState::Blocked { record } => {
                assert!(record.reason.contains("[orphaned_by_crash]"));
                assert!(record.reason.contains("prior_run=prior-run-id"));
            }
            other => panic!("expected Blocked, got {:?}", other),
        }
    }

    /// Test probe: live for any task in `live_ids`, dead otherwise.
    /// Lets us exercise the new liveness short-circuit deterministically
    /// without writing actual `.heartbeat` files.
    struct MockProbe {
        live_ids: std::collections::HashSet<String>,
    }

    impl LivenessProbe for MockProbe {
        fn is_live(&self, task_id: &str) -> bool {
            self.live_ids.contains(task_id)
        }
    }

    #[test]
    fn recovery_skips_running_with_fresh_heartbeat() {
        // The load-bearing fix: a task in Running with a prior-run
        // dispatch whose deadline has passed but whose heartbeat is
        // fresh must NOT be flagged orphan. This is the case for
        // long-running detached compute (Seurat CCA, etc.) that
        // outlives a harness's --max-iterations exit.
        let mut dag = DAG {
            version: "v1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks: {
                let mut m = BTreeMap::new();
                m.insert("alive".into(), running_task());
                m
            },
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };
        let records = vec![DispatchRecord {
            schema_version: dispatch_wal_schema_version(),
            task_id: "alive".into(),
            epoch: 1,
            harness_run_id: "prior-run-id".into(),
            // Deadline already passed (would be flagged without the
            // liveness probe).
            started_at: "2026-04-23T14:00:00Z".into(),
            timeout_at: "2026-04-23T14:30:00Z".into(),
        }];
        let mut live_ids = std::collections::HashSet::new();
        live_ids.insert("alive".into());
        let probe = MockProbe { live_ids };
        let report =
            recover_orphaned_dispatches(&mut dag, &records, "current-run-id", &probe, &WallClock);
        assert_eq!(report.orphaned_count, 0, "live task must not be flagged");
        assert_eq!(report.skipped_live_count, 1, "skip must be reported");
        assert_eq!(report.skipped_live_task_ids, vec!["alive".to_string()]);
        assert!(matches!(
            dag.tasks.get("alive").unwrap().state,
            TaskState::Running { .. }
        ));
    }

    #[test]
    fn recovery_flags_running_with_stale_heartbeat() {
        // False-negative coverage: when the liveness probe reports
        // dead (heartbeat stale or missing), the orphan-recovery
        // path runs unchanged — preserves crash-recovery for genuine
        // crashes where the agent + its heartbeat fork both died.
        let mut dag = DAG {
            version: "v1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks: {
                let mut m = BTreeMap::new();
                m.insert("dead".into(), running_task());
                m
            },
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };
        let records = vec![DispatchRecord {
            schema_version: dispatch_wal_schema_version(),
            task_id: "dead".into(),
            epoch: 1,
            harness_run_id: "prior-run-id".into(),
            started_at: "2026-04-23T14:00:00Z".into(),
            timeout_at: "2026-04-23T14:30:00Z".into(),
        }];
        let probe = MockProbe {
            live_ids: std::collections::HashSet::new(),
        };
        let report =
            recover_orphaned_dispatches(&mut dag, &records, "current-run-id", &probe, &WallClock);
        assert_eq!(report.orphaned_count, 1);
        assert_eq!(report.skipped_live_count, 0);
        match &dag.tasks.get("dead").unwrap().state {
            TaskState::Blocked { record } => {
                assert!(record.reason.contains("[orphaned_by_crash]"));
            }
            other => panic!("expected Blocked, got {:?}", other),
        }
    }

    #[test]
    fn heartbeat_liveness_probe_reads_real_filesystem() {
        // End-to-end probe test: write a fresh `.heartbeat` and verify
        // the production probe reports live; write a stale one and
        // verify it reports dead.
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();

        // Fresh heartbeat (just touched).
        let fresh_dir = pkg.join("runtime/outputs/fresh");
        std::fs::create_dir_all(&fresh_dir).unwrap();
        std::fs::write(fresh_dir.join(".heartbeat"), "now").unwrap();

        // Stale heartbeat (mtime back-dated 10 minutes ago).
        let stale_dir = pkg.join("runtime/outputs/stale");
        std::fs::create_dir_all(&stale_dir).unwrap();
        let stale_path = stale_dir.join(".heartbeat");
        std::fs::write(&stale_path, "old").unwrap();
        let stale_mtime = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
        let stale_file = std::fs::OpenOptions::new()
            .write(true)
            .open(&stale_path)
            .unwrap();
        let times = std::fs::FileTimes::new()
            .set_modified(stale_mtime)
            .set_accessed(stale_mtime);
        stale_file.set_times(times).unwrap();
        drop(stale_file);

        // Missing heartbeat (no file at all).
        let missing = "missing";

        let probe = HeartbeatLivenessProbe {
            package_root: pkg.to_path_buf(),
            freshness_secs: 60,
        };
        assert!(probe.is_live("fresh"), "fresh heartbeat → live");
        assert!(!probe.is_live("stale"), "stale heartbeat → dead");
        assert!(!probe.is_live(missing), "missing heartbeat → dead");
    }

    #[test]
    fn truncate_wal_empties_existing_log() {
        // Clean-exit truncation: a populated WAL becomes zero-length,
        // so the next harness start sees an empty WAL and short-circuits
        // the orphan-recovery scan.
        let tmp = tempfile::tempdir().unwrap();
        let rec = DispatchRecord {
            schema_version: dispatch_wal_schema_version(),
            task_id: "t1".into(),
            epoch: 1,
            harness_run_id: "abc".into(),
            started_at: "2026-04-23T14:00:00Z".into(),
            timeout_at: "2026-04-23T14:30:00Z".into(),
        };
        append_dispatch(tmp.path(), &rec).unwrap();
        assert!(!read_dispatches(tmp.path()).is_empty());
        truncate_wal(tmp.path()).unwrap();
        assert!(
            read_dispatches(tmp.path()).is_empty(),
            "WAL must be empty after truncate"
        );
        // File still exists (empty) so subsequent appends are idempotent.
        assert!(
            wal_path(tmp.path()).exists(),
            "WAL file must still exist after truncate (zero-length, not deleted)"
        );
    }

    #[test]
    fn truncate_wal_missing_file_is_ok() {
        // No WAL file ⇒ no-op success, so a fresh package that never
        // dispatched anything doesn't error on clean exit.
        let tmp = tempfile::tempdir().unwrap();
        truncate_wal(tmp.path()).unwrap();
    }

    #[test]
    fn always_dead_probe_preserves_legacy_behavior() {
        // The SWFC_HEARTBEAT_LIVENESS_SECS=0 operator opt-out selects
        // AlwaysDeadProbe. Verify it never reports live regardless
        // of input.
        let probe = AlwaysDeadProbe;
        assert!(!probe.is_live("anything"));
        assert!(!probe.is_live(""));
    }

    #[test]
    fn recovery_ignores_current_run_tasks() {
        let mut dag = DAG {
            version: "v1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks: {
                let mut m = BTreeMap::new();
                m.insert("active".into(), running_task());
                m
            },
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };
        let records = vec![DispatchRecord {
            schema_version: dispatch_wal_schema_version(),
            task_id: "active".into(),
            epoch: 1,
            harness_run_id: "current-run-id".into(),
            started_at: "2026-04-23T14:00:00Z".into(),
            timeout_at: "2026-04-23T14:30:00Z".into(),
        }];
        let report = recover_orphaned_dispatches(
            &mut dag,
            &records,
            "current-run-id",
            &AlwaysDeadProbe,
            &WallClock,
        );
        assert_eq!(report.orphaned_count, 0);
        assert!(matches!(
            dag.tasks.get("active").unwrap().state,
            TaskState::Running { .. }
        ));
    }

    /// P1-226 — when the AWS reaper just terminated the task's
    /// instance, the recovery must re-block the task EVEN IF the
    /// liveness probe still claims it's alive (stale heartbeat
    /// from before the kill).
    #[test]
    fn denylist_short_circuits_liveness_probe() {
        use ecaa_workflow_core::dag::RemoteExecution;

        struct AlwaysLive;
        impl LivenessProbe for AlwaysLive {
            fn is_live(&self, _task_id: &str) -> bool {
                true
            }
        }

        let mut task = running_task();
        task.state = TaskState::Running {
            started_at: chrono::Utc::now().to_rfc3339(),
            remote: Some(RemoteExecution {
                backend: "aws".into(),
                instance_id: "i-killed".into(),
                instance_type: "t3.medium".into(),
                command_id: None,
                output_uri: None,
            }),
        };
        let mut dag = DAG {
            version: "v1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks: {
                let mut m = BTreeMap::new();
                m.insert("ghost".into(), task);
                m
            },
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };
        let past = chrono::Utc::now() - chrono::Duration::hours(2);
        let records = vec![DispatchRecord {
            schema_version: dispatch_wal_schema_version(),
            task_id: "ghost".into(),
            epoch: 1,
            harness_run_id: "prior-run".into(),
            started_at: past.to_rfc3339(),
            timeout_at: (past + chrono::Duration::minutes(30)).to_rfc3339(),
        }];

        // Without denylist: AlwaysLive probe says alive → not orphaned.
        let mut dag_no_deny = dag.clone();
        let report = recover_orphaned_dispatches(
            &mut dag_no_deny,
            &records,
            "current-run-id",
            &AlwaysLive,
            &WallClock,
        );
        assert_eq!(report.orphaned_count, 0);
        assert_eq!(report.skipped_live_count, 1);

        // With denylist containing the killed instance: re-block
        // outright, ignoring the bogus liveness signal.
        let mut deny = std::collections::HashSet::new();
        deny.insert("i-killed".to_string());
        let report = recover_orphaned_dispatches_with_denylist(
            &mut dag,
            &records,
            "current-run-id",
            &AlwaysLive,
            &deny,
            &WallClock,
        );
        assert_eq!(report.orphaned_count, 1);
        assert_eq!(report.skipped_live_count, 0);
        let reason = match &dag.tasks.get("ghost").unwrap().state {
            TaskState::Blocked { record } => record.reason.clone(),
            _ => panic!(
                "expected Blocked, got {:?}",
                dag.tasks.get("ghost").unwrap().state
            ),
        };
        assert!(
            reason.contains("terminated by reaper"),
            "denylist re-block must mention the reaper: {reason}"
        );
        assert!(
            reason.contains("i-killed"),
            "denylist re-block must mention the dead instance id: {reason}"
        );
    }

    /// P1-226 — when the task has no `remote` (local executor), the
    /// denylist cannot match and the legacy liveness behaviour is
    /// preserved.
    #[test]
    fn denylist_does_not_match_tasks_without_remote() {
        struct AlwaysLive;
        impl LivenessProbe for AlwaysLive {
            fn is_live(&self, _task_id: &str) -> bool {
                true
            }
        }

        let task = running_task(); // remote=None
        let mut dag = DAG {
            version: "v1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks: {
                let mut m = BTreeMap::new();
                m.insert("local".into(), task);
                m
            },
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };
        let past = chrono::Utc::now() - chrono::Duration::hours(2);
        let records = vec![DispatchRecord {
            schema_version: dispatch_wal_schema_version(),
            task_id: "local".into(),
            epoch: 1,
            harness_run_id: "prior-run".into(),
            started_at: past.to_rfc3339(),
            timeout_at: (past + chrono::Duration::minutes(30)).to_rfc3339(),
        }];

        let mut deny = std::collections::HashSet::new();
        deny.insert("i-irrelevant".to_string());
        let report = recover_orphaned_dispatches_with_denylist(
            &mut dag,
            &records,
            "current-run-id",
            &AlwaysLive,
            &deny,
            &WallClock,
        );
        // Without a remote, denylist cannot trigger; AlwaysLive
        // wins → skip-live, not orphaned.
        assert_eq!(report.orphaned_count, 0);
        assert_eq!(report.skipped_live_count, 1);
    }

    /// Clock injection — `prior_run_dispatches` uses the injected clock's
    /// "now" for the timeout comparison, not `chrono::Utc::now()`. A
    /// `FrozenClock` pinned to a time BEFORE the record's `timeout_at`
    /// means the dispatch is still "in-flight" and must NOT be returned.
    /// A `FrozenClock` pinned AFTER the deadline must return it.
    #[test]
    fn dispatch_wal_records_timestamps_via_clock() {
        let timeout_str = "2026-06-01T12:00:00Z";
        let timeout_at: chrono::DateTime<chrono::Utc> = timeout_str.parse().unwrap();

        let records = vec![DispatchRecord {
            schema_version: dispatch_wal_schema_version(),
            task_id: "t1".into(),
            epoch: 1,
            harness_run_id: "prior-run".into(),
            started_at: "2026-06-01T11:00:00Z".into(),
            timeout_at: timeout_str.into(),
        }];

        // Clock says it's 30 minutes before deadline → in-flight, NOT returned.
        let before = FrozenClock {
            at: timeout_at - chrono::Duration::minutes(30),
        };
        let result = prior_run_dispatches(&records, "new-run", &before);
        assert!(
            result.is_empty(),
            "dispatch with future deadline must NOT be returned by a clock before the deadline"
        );

        // Clock says it's 1 second after deadline → genuinely stale, IS returned.
        let after = FrozenClock {
            at: timeout_at + chrono::Duration::seconds(1),
        };
        let result = prior_run_dispatches(&records, "new-run", &after);
        assert_eq!(
            result.len(),
            1,
            "dispatch with past deadline must be returned by a clock after the deadline"
        );
        assert_eq!(result["t1"].task_id, "t1");
    }

    /// `recover_orphaned_dispatches` with a `FrozenClock` produces
    /// deterministic orphan detection: a task is flagged only when
    /// the injected clock says the dispatch deadline has passed.
    #[test]
    fn dispatch_wal_orphan_recovery_uses_injected_clock() {
        let timeout_str = "2026-06-01T12:00:00Z";
        let timeout_at: chrono::DateTime<chrono::Utc> = timeout_str.parse().unwrap();

        let mut dag_before = DAG {
            version: "v1".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "wf".into(),
            current_task: None,
            tasks: {
                let mut m = BTreeMap::new();
                m.insert("task_a".into(), running_task());
                m
            },
            reverse_deps: BTreeMap::new(),
            run_id: None,
        };
        let mut dag_after = dag_before.clone();

        let records = vec![DispatchRecord {
            schema_version: dispatch_wal_schema_version(),
            task_id: "task_a".into(),
            epoch: 1,
            harness_run_id: "prior-run".into(),
            started_at: "2026-06-01T11:00:00Z".into(),
            timeout_at: timeout_str.into(),
        }];

        // Clock before deadline → no orphan detected.
        let before = FrozenClock {
            at: timeout_at - chrono::Duration::seconds(1),
        };
        let report = recover_orphaned_dispatches(
            &mut dag_before,
            &records,
            "current-run",
            &AlwaysDeadProbe,
            &before,
        );
        assert_eq!(
            report.orphaned_count, 0,
            "before-deadline clock must not flag orphan"
        );

        // Clock after deadline → orphan detected.
        let after = FrozenClock {
            at: timeout_at + chrono::Duration::seconds(1),
        };
        let report = recover_orphaned_dispatches(
            &mut dag_after,
            &records,
            "current-run",
            &AlwaysDeadProbe,
            &after,
        );
        assert_eq!(
            report.orphaned_count, 1,
            "after-deadline clock must flag orphan"
        );
        assert_eq!(report.orphaned_task_ids, vec!["task_a".to_string()]);
    }
}
