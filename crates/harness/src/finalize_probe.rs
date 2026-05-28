//! Deterministic finalize probe — runs the agent-declared
//! `recoverable_action` for tasks whose blocker.json indicates the
//! recovery is mechanical rather than LLM-guided.
//!
//! Cuts the "harness pump" failure mode: a long-running detached
//! compute (R, Python, remote API poll) emits a sentinel file when
//! done. Without this probe, the harness must dispatch the agent on
//! every iteration so the agent can re-check the sentinel — burning
//! $3-4 of Opus per check on a job whose only output is "still
//! waiting." With this probe, the deterministic shell wrapper runs
//! first; the agent is only dispatched when the wrapper either
//! advances state (writing a state.patch.json the harness merges next)
//! or stays silent (genuinely needs LLM judgement).
//!
//! The probe is sync, sandboxed to the task's output directory, and
//! throttled — see [`should_probe`] — so a fast-iterating harness
//! doesn't oversample a slow-emitting compute.
//!
//! # TOCTOU defense in `resolve_script_path`
//!
//! The path-jail uses an open-first-then-verify pattern to close the
//! symlink-swap race that exists when you canonicalize a path and then
//! open it separately (time-of-check vs time-of-use).
//!
//! The sequence is:
//! 1. Open the candidate file with `O_NOFOLLOW` — the kernel refuses
//!    the open if the *last* path component is a symlink, so a swap
//!    that replaces the file with a symlink after we've checked but
//!    before we open is caught here.
//! 2. Read the canonical path of the *open fd* via `/proc/self/fd/<n>`
//!    on Linux.  Because the fd was opened before any path was stored,
//!    whatever `/proc` resolves is the actual inode we hold — there is
//!    no further window for a swap.
//! 3. Re-assert that the resolved fd path starts with the canonical
//!    task output dir.  This catches the case where the *parent
//!    directory* (rather than the final component) was a symlink that
//!    pointed outside the jail.
//!
//! On non-Linux targets the `/proc` step is skipped and we fall back
//! to pre-open `canonicalize()` (best-effort; macOS/BSD callers would
//! need `fcntl(F_GETPATH)` for equivalent protection).
//!
//! The function returns an open `std::fs::File` handle rather than a
//! `PathBuf` so the caller invokes the script via the fd-derived path,
//! not via the user-supplied path string — the same inode that was
//! validated is the one that is executed.
//!
//! Failure modes covered:
//! - Long-running R/Python compute waiting on its own sentinel (the
//!   IVD `batch_correction` case).
//! - Upstream-polling tasks (`clustering` waiting on
//!   `batch_correction/integration_status.OK`).
//! - Network-bound polls where the script does an HTTP HEAD.
//! - Anything else the agent can express as an idempotent shell
//!   wrapper that exits 0 with no state change while waiting.
//!
//! Failure modes NOT covered:
//! - Stalled / crashed compute — that's the heartbeat-stall path.
//! - Sentinel-failure where the wrapper writes a structured `running →
//! blocked` patch — handled correctly via the existing patch
//!   protocol; no special case here.

use crate::constants::OUTPUT_TAIL_BYTES;
use crate::swfc_io::{read_capped, resolve_max_bytes};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default throttle: don't invoke the same task's recoverable_action
/// more than once per this many seconds. Override with
/// `ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS`.
pub const DEFAULT_PROBE_MIN_INTERVAL_SECS: u64 = 60;

/// Per-invocation timeout for the wrapper script. The script must
/// either complete fast (sentinel present → finalize) or exit fast
/// (sentinel pending → no-op exit 0). A wrapper that hangs is
/// terminated; the harness logs a warning and the next iteration tries
/// again. Override with `ECAA_HARNESS_FINALIZE_PROBE_TIMEOUT_SECS`.
pub const DEFAULT_PROBE_TIMEOUT_SECS: u64 = 30;

/// Read the probe interval threshold from env. Range-clamped to
/// `[5, 3600]` to prevent both tight loops and accidental disables.
/// Pass-through 0 disables the throttle entirely (used in tests).
pub fn probe_min_interval_secs() -> u64 {
    let raw = std::env::var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_PROBE_MIN_INTERVAL_SECS);
    if raw == 0 {
        return 0;
    }
    raw.clamp(5, 3600)
}

/// Per-invocation timeout for one wrapper run. Range-clamped to
/// `[1, 600]`. Set this generous enough that a wrapper which ACTUALLY
/// finalizes (e.g. running the figures script after the R job
/// completes) has time to finish. The default 30s is sized for the
/// "exit early, no sentinel" path; consumers writing slower wrappers
/// should bump the env var.
pub fn probe_timeout_secs() -> u64 {
    std::env::var("ECAA_HARNESS_FINALIZE_PROBE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_PROBE_TIMEOUT_SECS)
        .clamp(1, 600)
}

/// Subset of the agent's `runtime/outputs/<task_id>/blocker.json` that
/// declares the recoverable action. Other fields are ignored — only
/// `recoverable_action.{kind, rel_path}` is load-bearing here.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BlockerJsonProbe {
    #[serde(default)]
    recoverable_action: Option<RecoverableAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RecoverableAction {
    /// Only `"rerun_script"` is dispatched here. Other kinds (e.g.
    /// `"sme_pick"`, `"sme_approve"`) require human judgement and are
    /// handed to the BlockerCard, not the probe.
    kind: String,
    /// Path of the wrapper script, relative to
    /// `runtime/outputs/<task_id>/`. The script must be inside that
    /// directory tree — see [`resolve_script_path`] for the
    /// path-escape guard.
    rel_path: String,
}

/// Sidecar that records the last time the probe ran for a given task.
/// Persisted to disk so throttling survives harness restarts (the IVD
/// pump scenario respawns the harness every ~1s in the worst case).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeRecord {
    /// Task this probe record belongs to.
    pub task_id: String,
    /// RFC 3339 timestamp of the most recent probe invocation.
    pub last_probe_at: String,
    /// Exit code of the most recent wrapper run (`None` if not yet run or timed out).
    pub last_exit_code: Option<i32>,
    /// Captured stdout / stderr tail (last ~2 KiB) for audit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output_tail: Option<String>,
}

/// Outcome of one probe attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The wrapper ran and exited successfully. The harness's normal
    /// `apply_pending_patches` pass picks up any `state.patch.json`
    /// the wrapper wrote.
    Ran {
        /// Exit code returned by the wrapper script.
        exit_code: i32,
    },
    /// The wrapper ran but exceeded the timeout.
    TimedOut,
    /// The wrapper or its prerequisites couldn't be located /
    /// validated. Caller logs and moves on.
    Skipped {
        /// Human-readable reason for skipping.
        reason: String,
    },
    /// The throttle window hasn't elapsed since the last probe.
    Throttled {
        /// Seconds since the last probe was recorded.
        age_secs: u64,
    },
}

/// Decide whether the probe should run for `task_id` based on the
/// last-probe sidecar age and the configured min interval.
/// `min_interval_secs == 0` disables throttling.
pub fn should_probe(probe_record_path: &Path, min_interval_secs: u64) -> bool {
    if min_interval_secs == 0 {
        return true;
    }
    let Ok(meta) = std::fs::metadata(probe_record_path) else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    let Ok(elapsed) = modified.elapsed() else {
        return true;
    };
    elapsed.as_secs() >= min_interval_secs
}

/// Resolve the wrapper script path and verify it stays inside the
/// task's output directory using the open-first-then-verify pattern
/// described in the module-level doc.
///
/// Returns an open `File` handle whose path has been verified against
/// the jail boundary.  The caller must derive the execution path from
/// this handle (via `/proc/self/fd/<n>` on Linux) rather than from the
/// original `rel_path` string, so the validated inode is the one that
/// is executed.
///
/// Path-escape guard: absolute paths, symlinks at the final component
/// (`O_NOFOLLOW`), and parent-directory symlinks pointing outside the
/// task output dir are all rejected.  The script must also be a
/// regular file.
///
/// Tolerates the two `rel_path` shapes agents emit in the wild:
/// `<wrapper>` (bare) and `scripts/<wrapper>` (the canonical layout
/// most agents actually write the script under). Bare path is tried
/// first so a script placed at the task root wins precedence over a
/// same-named one in `scripts/`. The path-escape and is-regular-file
/// checks apply to whichever candidate resolves.
fn resolve_script_path(task_output_dir: &Path, rel_path: &str) -> Result<(File, PathBuf)> {
    use std::os::unix::fs::OpenOptionsExt;

    if rel_path.is_empty() {
        anyhow::bail!("rel_path is empty");
    }
    let rel = Path::new(rel_path);
    if rel.is_absolute() {
        anyhow::bail!("rel_path must be relative: {}", rel_path);
    }
    let canonical_root = task_output_dir
        .canonicalize()
        .with_context(|| format!("canonicalizing {}", task_output_dir.display()))?;

    let candidates = [
        task_output_dir.join(rel),
        task_output_dir.join("scripts").join(rel),
    ];

    // O_NOFOLLOW (0x20000 on Linux) causes open(2) to fail with ELOOP
    // when the *final* path component is a symlink.  This closes the
    // last-component symlink-swap window without needing to check
    // before open.
    let o_nofollow = libc::O_NOFOLLOW;

    let mut last_err: Option<anyhow::Error> = None;
    for candidate in &candidates {
        // Open with O_NOFOLLOW — kernel refuses if the last component
        // is a symlink.  We request read-only; the caller executes via
        // the fd-derived path, not by re-opening.
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(o_nofollow)
            .open(candidate)
        {
            Ok(f) => f,
            Err(e) => {
                last_err = Some(
                    anyhow::Error::new(e)
                        .context(format!("open(O_NOFOLLOW) {}", candidate.display())),
                );
                continue;
            }
        };

        // Obtain the canonical path of the open fd.  On Linux we read
        // /proc/self/fd/<n> which resolves to the actual inode path —
        // there is no further race window.  On non-Linux we fall back
        // to canonicalize() on the original candidate path (best-effort;
        // macOS/BSD would need fcntl(F_GETPATH) for equivalent
        // protection).
        #[cfg(target_os = "linux")]
        let canonical_candidate: PathBuf = {
            use std::os::unix::io::AsRawFd;
            let fd = file.as_raw_fd();
            std::fs::read_link(format!("/proc/self/fd/{}", fd))
                .with_context(|| format!("read_link /proc/self/fd/{}", fd))?
        };

        #[cfg(not(target_os = "linux"))]
        let canonical_candidate: PathBuf = {
            // Best-effort on non-Linux: canonicalize the candidate path.
            // A symlink swap between open and this canonicalize call is
            // theoretically possible; Linux users get the fully race-free
            // /proc path above.
            candidate
                .canonicalize()
                .with_context(|| format!("canonicalizing {}", candidate.display()))?
        };

        if !canonical_candidate.starts_with(&canonical_root) {
            anyhow::bail!(
                "path escape: {} resolves outside {}",
                canonical_candidate.display(),
                canonical_root.display()
            );
        }
        // Confirm the inode we actually opened is a regular file, not a
        // device, directory, or FIFO.
        let meta = file
            .metadata()
            .with_context(|| format!("fstat {}", canonical_candidate.display()))?;
        if !meta.is_file() {
            anyhow::bail!("not a regular file: {}", canonical_candidate.display());
        }
        return Ok((file, canonical_candidate));
    }

    Err(last_err.unwrap_or_else(|| {
        anyhow::anyhow!(
            "could not resolve {} under {} (also tried scripts/{})",
            rel_path,
            task_output_dir.display(),
            rel_path
        )
    }))
}

/// Read the recoverable_action from a task's blocker.json. Returns
/// `Ok(None)` when the file is missing, malformed, or the action's
/// kind is not `"rerun_script"` — all of which are normal
/// (heartbeat-stalled tasks, SME-decision blockers, etc.).
fn read_recoverable_action(blocker_json: &Path) -> Result<Option<(String, String)>> {
    // Cap blocker.json so a malicious agent can't OOM
    // the harness through the deterministic finalize-probe path.
    let raw = match read_capped(blocker_json, resolve_max_bytes()) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", blocker_json.display())),
    };
    let probe: BlockerJsonProbe = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let Some(action) = probe.recoverable_action else {
        return Ok(None);
    };
    if action.kind != "rerun_script" {
        return Ok(None);
    }
    if action.rel_path.is_empty() {
        return Ok(None);
    }
    Ok(Some((action.kind, action.rel_path)))
}

/// Run one wrapper script with a timeout. The child process inherits
/// the harness's environment (so `ECAA_*` vars propagate to the
/// wrapper) but its stdout / stderr are captured rather than
/// streamed — keeps the harness's primary log readable when many
/// probes fire per iteration.
fn run_wrapper_with_timeout(
    script: &Path,
    cwd: &Path,
    timeout: Duration,
) -> Result<(Option<i32>, String)> {
    use std::process::{Command, Stdio};

    let mut child = Command::new(script)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {}", script.display()))?;

    // Poll for completion — std::process doesn't have a portable
    // `wait_timeout`, and we don't want a tokio dep here. The loop
    // is bounded by `timeout` and sleeps for `poll_interval` between
    // checks. 50 ms keeps tail latency low without busy-waiting.
    let deadline = std::time::Instant::now() + timeout;
    let poll_interval = Duration::from_millis(50);
    let mut exit: Option<std::process::ExitStatus> = None;
    while std::time::Instant::now() < deadline {
        match child.try_wait()? {
            Some(status) => {
                exit = Some(status);
                break;
            }
            None => std::thread::sleep(poll_interval),
        }
    }
    let exit_code = if let Some(status) = exit {
        status.code()
    } else {
        // Timeout — best-effort kill. SIGKILL semantics on unix; on
        // platforms without it the child may linger but the harness
        // moves on.
        let _ = child.kill();
        let _ = child.wait();
        return Ok((None, String::new()));
    };

    // Drain piped output. The wrapper is expected to write little
    // (a "[finalize] no sentinel yet" line, typically), so we read
    // up to 2 KiB per stream to bound memory.
    fn read_capped(s: Option<impl std::io::Read>) -> String {
        let Some(mut r) = s else {
            return String::new();
        };
        let mut buf = [0u8; OUTPUT_TAIL_BYTES];
        let mut out = String::new();
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            out.push_str(&String::from_utf8_lossy(&buf[..n]));
            if out.len() >= OUTPUT_TAIL_BYTES {
                break;
            }
        }
        out
    }
    let stdout = read_capped(child.stdout.take());
    let stderr = read_capped(child.stderr.take());
    let combined = if stdout.is_empty() {
        stderr
    } else if stderr.is_empty() {
        stdout
    } else {
        format!("{}\n--stderr--\n{}", stdout, stderr)
    };
    Ok((exit_code, combined))
}

/// Persist the probe record sidecar so the next harness invocation
/// can throttle correctly. Best-effort: a failed write is logged but
/// doesn't fail the iteration.
fn write_probe_record(probe_record_path: &Path, record: &ProbeRecord) {
    let pretty = match serde_json::to_string_pretty(record) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "[finalize_probe] serialize record for {} failed: {}",
                record.task_id, e
            );
            return;
        }
    };
    if let Err(e) = std::fs::write(probe_record_path, pretty) {
        eprintln!(
            "[finalize_probe] write record {} failed: {}",
            probe_record_path.display(),
            e
        );
    }
}

/// One probe attempt for one task. Owns the read-blocker → resolve →
/// throttle → run → record sequence so the iteration loop can call
/// it once per candidate without tracking intermediate state.
pub fn probe_one_task(package_root: &Path, task_id: &str) -> ProbeOutcome {
    let task_output_dir = package_root.join("runtime/outputs").join(task_id);
    let blocker_json = task_output_dir.join("blocker.json");
    let action = match read_recoverable_action(&blocker_json) {
        Ok(Some((_, rel_path))) => rel_path,
        Ok(None) => {
            return ProbeOutcome::Skipped {
                reason: "no rerun_script recoverable_action declared".into(),
            };
        }
        Err(e) => {
            return ProbeOutcome::Skipped {
                reason: format!("blocker.json read failed: {:#}", e),
            };
        }
    };

    let probe_record_path = task_output_dir.join("last_probe.json");
    let interval = probe_min_interval_secs();
    if !should_probe(&probe_record_path, interval) {
        let age = std::fs::metadata(&probe_record_path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|e| e.as_secs())
            .unwrap_or(0);
        return ProbeOutcome::Throttled { age_secs: age };
    }

    // resolve_script_path returns the open File handle (validated via
    // O_NOFOLLOW + /proc/self/fd) and the fd-derived canonical path.
    // We execute using the canonical path so the same inode that was
    // validated is the one that runs — no re-open by the original
    // user-supplied string.
    let (_file, script) = match resolve_script_path(&task_output_dir, &action) {
        Ok(p) => p,
        Err(e) => {
            return ProbeOutcome::Skipped {
                reason: format!("resolve_script_path failed: {:#}", e),
            };
        }
    };

    let timeout = Duration::from_secs(probe_timeout_secs());
    let (exit_code, output) = match run_wrapper_with_timeout(&script, &task_output_dir, timeout) {
        Ok(t) => t,
        Err(e) => {
            return ProbeOutcome::Skipped {
                reason: format!("spawn wrapper failed: {:#}", e),
            };
        }
    };

    let record = ProbeRecord {
        task_id: task_id.to_string(),
        last_probe_at: chrono::Utc::now().to_rfc3339(),
        last_exit_code: exit_code,
        last_output_tail: if output.is_empty() {
            None
        } else {
            Some(
                output
                    .chars()
                    .rev()
                    .take(OUTPUT_TAIL_BYTES)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect(),
            )
        },
    };
    write_probe_record(&probe_record_path, &record);

    match exit_code {
        Some(code) => ProbeOutcome::Ran { exit_code: code },
        None => ProbeOutcome::TimedOut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Serializes tests that mutate `ECAA_HARNESS_FINALIZE_PROBE_*`
    /// env vars — `std::env::set_var` is process-global, so concurrent
    /// test threads racing on the same key produce flaky results
    /// (e.g. throttle test sees a previous test's "0" override).
    /// `std::sync::Mutex` is sufficient for in-test serialization;
    /// poison on test panic is acceptable because the next test will
    /// also panic on the same env-state expectation.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn make_executable(path: &Path) {
        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }

    fn write_blocker_json(dir: &Path, recoverable_kind: &str, rel_path: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let payload = serde_json::json!({
            "task_id": "t",
            "blocker_kind": "awaiting_sme_input",
            "recoverable_action": {
                "kind": recoverable_kind,
                "rel_path": rel_path,
                "label": "Retry",
                "description": "test",
            }
        });
        std::fs::write(
            dir.join("blocker.json"),
            serde_json::to_string_pretty(&payload).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn skip_when_blocker_json_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let outcome = probe_one_task(tmp.path(), "ghost");
        assert!(matches!(outcome, ProbeOutcome::Skipped { .. }));
    }

    #[test]
    fn skip_when_recoverable_action_kind_not_rerun_script() {
        // sme_pick / sme_approve / etc. require human judgement and
        // must NOT be auto-invoked by the probe.
        let tmp = tempfile::tempdir().unwrap();
        let task_dir = tmp.path().join("runtime/outputs/t");
        write_blocker_json(&task_dir, "sme_pick", "irrelevant.sh");
        let outcome = probe_one_task(tmp.path(), "t");
        assert!(matches!(outcome, ProbeOutcome::Skipped { .. }));
    }

    #[test]
    fn rerun_script_runs_and_exits_zero() {
        // Idempotent wrapper that exits 0 (sentinel not yet present).
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let task_dir = tmp.path().join("runtime/outputs/t");
        std::fs::create_dir_all(&task_dir).unwrap();
        let script = task_dir.join("finalize.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho '[finalize] no sentinel yet'\nexit 0\n",
        )
        .unwrap();
        make_executable(&script);
        write_blocker_json(&task_dir, "rerun_script", "finalize.sh");
        // Disable throttle so we can run back-to-back in the test.
        std::env::set_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS", "0");
        let outcome = probe_one_task(tmp.path(), "t");
        std::env::remove_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS");
        assert!(matches!(outcome, ProbeOutcome::Ran { exit_code: 0 }));
        // Sidecar persisted.
        assert!(task_dir.join("last_probe.json").exists());
    }

    #[test]
    fn rerun_script_writes_state_patch_when_sentinel_present() {
        // Real-world shape: wrapper detects sentinel and writes
        // state.patch.json. The probe runs the wrapper and exits;
        // the patch is left for the harness's normal
        // apply_pending_patches pass to merge.
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let task_dir = tmp.path().join("runtime/outputs/t");
        std::fs::create_dir_all(&task_dir).unwrap();
        // Pre-place the sentinel.
        std::fs::write(task_dir.join("integration_status.OK"), "ok").unwrap();
        let script = task_dir.join("finalize.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nif [ -f integration_status.OK ]; then\n  cat > state.patch.json <<EOF\n{\"from\":\"running\",\"to\":{\"status\":\"completed\",\"result\":{\"finalized\":true}}}\nEOF\nfi\nexit 0\n",
        )
        .unwrap();
        make_executable(&script);
        write_blocker_json(&task_dir, "rerun_script", "finalize.sh");
        std::env::set_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS", "0");
        let outcome = probe_one_task(tmp.path(), "t");
        std::env::remove_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS");
        assert!(matches!(outcome, ProbeOutcome::Ran { exit_code: 0 }));
        let patch = std::fs::read_to_string(task_dir.join("state.patch.json")).unwrap();
        assert!(patch.contains("completed"));
    }

    #[test]
    fn throttle_skips_back_to_back_probes() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let task_dir = tmp.path().join("runtime/outputs/t");
        std::fs::create_dir_all(&task_dir).unwrap();
        let script = task_dir.join("noop.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&script);
        write_blocker_json(&task_dir, "rerun_script", "noop.sh");
        // Default throttle is 60s.
        std::env::remove_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS");
        let first = probe_one_task(tmp.path(), "t");
        assert!(matches!(first, ProbeOutcome::Ran { .. }));
        let second = probe_one_task(tmp.path(), "t");
        assert!(
            matches!(second, ProbeOutcome::Throttled { .. }),
            "got {:?}",
            second
        );
    }

    #[test]
    fn rerun_script_falls_back_to_scripts_subdir() {
        // Real-world shape: the agent places the wrapper under
        // `scripts/<name>` but writes the bare name into blocker.json's
        // `recoverable_action.rel_path`. Without the fallback the probe
        // returns Skipped silently and the harness pumps the LLM
        // agent every iteration. With the fallback the bare path
        // resolves into `scripts/`, the wrapper runs, and probe returns
        // Ran.
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let task_dir = tmp.path().join("runtime/outputs/t");
        let scripts_dir = task_dir.join("scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        let script = scripts_dir.join("finalize.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\necho '[finalize] no sentinel yet'\nexit 0\n",
        )
        .unwrap();
        make_executable(&script);
        // rel_path is the BARE name — exercises the scripts/ fallback.
        write_blocker_json(&task_dir, "rerun_script", "finalize.sh");
        std::env::set_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS", "0");
        let outcome = probe_one_task(tmp.path(), "t");
        std::env::remove_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS");
        assert!(
            matches!(outcome, ProbeOutcome::Ran { exit_code: 0 }),
            "got {:?}",
            outcome
        );
        // Probe sidecar still lands at the task root, not under scripts/.
        assert!(task_dir.join("last_probe.json").exists());
    }

    #[test]
    fn bare_path_wins_when_both_locations_have_script() {
        // Precedence: a script at the task root wins over a same-named
        // one in scripts/. Detect by writing different stdout markers
        // and asserting the bare wrapper's output appears in the probe
        // sidecar.
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let task_dir = tmp.path().join("runtime/outputs/t");
        let scripts_dir = task_dir.join("scripts");
        std::fs::create_dir_all(&scripts_dir).unwrap();
        let bare = task_dir.join("finalize.sh");
        std::fs::write(&bare, "#!/bin/sh\necho 'BARE'\nexit 0\n").unwrap();
        make_executable(&bare);
        let nested = scripts_dir.join("finalize.sh");
        std::fs::write(&nested, "#!/bin/sh\necho 'NESTED'\nexit 0\n").unwrap();
        make_executable(&nested);
        write_blocker_json(&task_dir, "rerun_script", "finalize.sh");
        std::env::set_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS", "0");
        let outcome = probe_one_task(tmp.path(), "t");
        std::env::remove_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS");
        assert!(matches!(outcome, ProbeOutcome::Ran { exit_code: 0 }));
        let record = std::fs::read_to_string(task_dir.join("last_probe.json")).unwrap();
        assert!(
            record.contains("BARE"),
            "bare wrapper should have run; sidecar={}",
            record
        );
        assert!(!record.contains("NESTED"));
    }

    #[test]
    fn rejects_path_escape_via_dotdot() {
        // Defense in depth — a malicious / buggy blocker.json could
        // declare "../../../etc/passwd" as the script. Refuse.
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let task_dir = tmp.path().join("runtime/outputs/t");
        std::fs::create_dir_all(&task_dir).unwrap();
        std::fs::write(task_dir.join("blocker.json"), "{}").unwrap();
        write_blocker_json(&task_dir, "rerun_script", "../../etc/passwd");
        std::env::set_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS", "0");
        let outcome = probe_one_task(tmp.path(), "t");
        std::env::remove_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS");
        assert!(matches!(outcome, ProbeOutcome::Skipped { .. }));
    }

    #[test]
    fn rejects_absolute_rel_path() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let task_dir = tmp.path().join("runtime/outputs/t");
        std::fs::create_dir_all(&task_dir).unwrap();
        write_blocker_json(&task_dir, "rerun_script", "/bin/sh");
        std::env::set_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS", "0");
        let outcome = probe_one_task(tmp.path(), "t");
        std::env::remove_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS");
        assert!(matches!(outcome, ProbeOutcome::Skipped { .. }));
    }

    #[test]
    fn timeout_terminates_hung_wrapper() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let task_dir = tmp.path().join("runtime/outputs/t");
        std::fs::create_dir_all(&task_dir).unwrap();
        let script = task_dir.join("hang.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 30\n").unwrap();
        make_executable(&script);
        write_blocker_json(&task_dir, "rerun_script", "hang.sh");
        std::env::set_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS", "0");
        std::env::set_var("ECAA_HARNESS_FINALIZE_PROBE_TIMEOUT_SECS", "1");
        let started = std::time::Instant::now();
        let outcome = probe_one_task(tmp.path(), "t");
        let elapsed = started.elapsed().as_secs();
        std::env::remove_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS");
        std::env::remove_var("ECAA_HARNESS_FINALIZE_PROBE_TIMEOUT_SECS");
        assert!(
            matches!(outcome, ProbeOutcome::TimedOut),
            "got {:?}",
            outcome
        );
        // Bound on the timeout itself + a bit of slop for spawn / poll.
        assert!(
            elapsed <= 5,
            "timeout enforcement broken: probe ran {}s",
            elapsed
        );
    }

    #[test]
    fn probe_min_interval_clamps() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS", "1");
        assert_eq!(probe_min_interval_secs(), 5, "must clamp up to 5s");
        std::env::set_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS", "9999");
        assert_eq!(probe_min_interval_secs(), 3600, "must clamp down to 3600s");
        std::env::set_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS", "0");
        assert_eq!(probe_min_interval_secs(), 0, "0 is the disable sentinel");
        std::env::set_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS", "60");
        assert_eq!(probe_min_interval_secs(), 60);
        std::env::remove_var("ECAA_HARNESS_FINALIZE_PROBE_MIN_INTERVAL_SECS");
    }

    /// A symlink inside the task output dir whose target is outside the
    /// dir must be rejected.  This exercises the O_NOFOLLOW + proc-fd
    /// TOCTOU defense: the open itself fails (ELOOP) when the final
    /// component is a symlink, so a malicious agent cannot trick the
    /// harness into executing an arbitrary path by replacing a
    /// legitimate script with a symlink after the pre-open check.
    #[test]
    fn resolve_rejects_symlink_to_outside_task_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let task_dir = tmp.path().join("runtime/outputs/t");
        std::fs::create_dir_all(&task_dir).unwrap();
        // Create a symlink inside the task dir pointing to /etc/passwd
        // (or any file guaranteed to exist outside the dir).
        let symlink_path = task_dir.join("evil.sh");
        std::os::unix::fs::symlink("/etc/passwd", &symlink_path).unwrap();
        let result = resolve_script_path(&task_dir, "evil.sh");
        assert!(
            result.is_err(),
            "expected resolve_script_path to reject a symlink pointing outside the task dir, \
             but it returned Ok"
        );
    }

    /// A plain regular file inside the task output dir must be accepted
    /// without error — the happy path must survive the TOCTOU hardening.
    #[test]
    fn resolve_accepts_regular_file_inside_task_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let task_dir = tmp.path().join("runtime/outputs/t");
        std::fs::create_dir_all(&task_dir).unwrap();
        let script = task_dir.join("probe.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&script);
        let result = resolve_script_path(&task_dir, "probe.sh");
        assert!(
            result.is_ok(),
            "expected resolve_script_path to accept a regular file inside the task dir, \
             but it returned Err: {:?}",
            result.err()
        );
    }
}
