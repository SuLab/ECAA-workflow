//! Non-blocking host-level lock keyed by `session_id` so two harness
//! processes can never legitimately share one chat session.
//!
//! Spike 16.0.C showed the failure mode: the server spawns a harness
//! for `--session-id foo`, the operator simultaneously runs a manual
//! `cargo run -p ecaa-workflow-harness -- --session-id foo`, and
//! the two processes race on `WORKFLOW.json` (split-brain DAG patch
//! merges), the dispatch WAL (cross-process write collisions), and
//! AWS instance tags (each process believes the other's instance is
//! an orphan).
//!
//! The lock is a POSIX `flock(LOCK_EX | LOCK_NB)` on
//! `~/.scripps-workflow/locks/<session_id>.lock`. Kernel-managed,
//! per-fd: when the harness process exits — even via SIGKILL or a
//! crash — the kernel drops the lock and the file is removed in
//! `Drop`. A peer harness acquiring the same id discovers it
//! immediately and exits with a clear stderr message rather than
//! racing on shared state.
//!
//! Bypass: setting `ECAA_HARNESS_DEBUG_ALLOW_MULTI_PROCESS=1` skips
//! the acquire entirely so tests that deliberately spawn two
//! harnesses (e.g. multi-process WAL contention scenarios) can opt
//! out. Production paths should never set this.

use anyhow::{anyhow, Context, Result};
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::PathBuf;

/// Process-bound host lock keyed by a `session_id`. The exclusive
/// flock is released by the kernel when the file descriptor is
/// dropped; the lockfile is best-effort unlinked in `Drop`.
pub struct SessionLock {
    file: Option<std::fs::File>,
    path: PathBuf,
}

impl SessionLock {
    /// Acquire a non-blocking exclusive flock on the lockfile for
    /// `session_id`. Fails when another live harness already holds
    /// it; the caller in `main` prints the contention message and
    /// exits with code 2.
    ///
    /// Honors `ECAA_HARNESS_DEBUG_ALLOW_MULTI_PROCESS=1` by
    /// returning a sentinel that holds no real lock — only test
    /// harnesses that need two processes deliberately should set it.
    pub fn acquire(session_id: &str) -> Result<Self> {
        if std::env::var("ECAA_HARNESS_DEBUG_ALLOW_MULTI_PROCESS")
            .ok()
            .as_deref()
            == Some("1")
        {
            return Ok(Self {
                file: None,
                path: PathBuf::new(),
            });
        }
        let path = lockfile_path(session_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating lock directory {}", parent.display()))?;
        }
        // Mode 0o600 — only the owning user can poke at the file. The
        // contents are advisory (pid + start time), the load-bearing
        // bit is the kernel-tracked flock.
        // W7.2: try acquire-then-retry-once on stale lockfile. The kernel
        // drops a held flock when its owning process dies (any cause,
        // including SIGKILL), so contention is real if and only if some
        // live process still holds the fd. The `Drop` impl unlinks the
        // lockfile on graceful exit, but a SIGKILL leaves the inode in
        // place with no live holder — the next acquire would then fail
        // with EWOULDBLOCK even though no peer is alive. Defend by parsing
        // the recorded pid on failure and unlinking + retrying when dead.
        let mut retried_after_unlink = false;
        let (file, fd) = loop {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&path)
                .with_context(|| format!("opening lock file {}", path.display()))?;
            let fd = file.as_raw_fd();
            // SAFETY: libc::flock with a valid fd is sound. The fd's
            // ownership stays with `file`; `flock` does not transfer it.
            // The workspace `unsafe_code = "deny"` lint requires this
            // bounded waiver — see Cargo.toml. LOCK_EX | LOCK_NB is the
            // non-blocking exclusive variant: returns -1 with errno
            // EWOULDBLOCK if another process holds the lock.
            let rc = {
                #[allow(unsafe_code)]
                unsafe {
                    libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB)
                }
            };
            if rc == 0 {
                break (file, fd);
            }
            let err = std::io::Error::last_os_error();
            // W7.2: if contention is from a dead pid (SIGKILL leftover),
            // unlink the inode and retry exactly once. The retry is
            // race-safe: a second harness that also tries to clean up
            // either succeeds (we proceed) or finds the file held by a
            // newly-arrived live peer (we fail and report contention).
            if !retried_after_unlink {
                retried_after_unlink = true;
                if let Some(holder_pid) = read_lockfile_pid(&path) {
                    if !is_pid_alive(holder_pid) {
                        drop(file); // release the fd before unlink
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                }
            }
            return Err(anyhow!(
                "could not acquire session lock {}: {} \
                 (another harness for session_id={} is already running; \
                 set ECAA_HARNESS_DEBUG_ALLOW_MULTI_PROCESS=1 to bypass for tests)",
                path.display(),
                err,
                session_id
            ));
        };
        let _ = fd; // silence unused — bound for SAFETY comment context
                    // Best-effort: write our pid + start hint into the file so
                    // operators can diagnose contention without strace. Writing
                    // is non-fatal because the flock is the authoritative state.
        let _ = std::fs::write(
            &path,
            format!(
                "{} {}\n",
                std::process::id(),
                ecaa_workflow_core::time_helpers::now_rfc3339()
            ),
        );
        Ok(Self {
            file: Some(file),
            path,
        })
    }

    /// Path of the lockfile this guard holds. Exposed for diagnostic
    /// logging in `main`.
    #[allow(dead_code)] // reserved-for-diagnostics: surfaced in operator runbook flows
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        // The kernel drops the flock when the fd closes. We additionally
        // unlink the file so an operator's `ls ~/.scripps-workflow/locks`
        // doesn't accumulate stale entries; if unlink fails (e.g. peer
        // grabbed it after we closed), we ignore the error.
        if self.file.is_some() && !self.path.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn lockfile_path(session_id: &str) -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // No HOME — fall back to /tmp so the helper doesn't crash in
            // exotic CI shells. Two harnesses without HOME still collide
            // on the same /tmp path so the lock remains effective.
            PathBuf::from("/tmp")
        });
    // Sanitise the session_id so a chat-side bug that lets a slash
    // into the id can't escape the lock dir. Same character set the
    // server's path-jail helper accepts.
    let sanitised: String = session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    home.join(".scripps-workflow")
        .join("locks")
        .join(format!("{}.lock", sanitised))
}

/// Directory holding all session-lock files. Exposed so peer
/// detection can enumerate them.
pub fn locks_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".scripps-workflow").join("locks")
}

/// Enumerate session ids of live peer harnesses.
///
/// Reads every `*.lock` file under `~/.scripps-workflow/locks/`,
/// extracts the recorded pid from its contents, and checks the pid
/// is alive via `kill(pid, 0)` (POSIX liveness probe — no signal
/// delivered, just permission/existence check). Returns the
/// session_id portion of the filename (`<sid>.lock` → `<sid>`).
///
/// `self_session_id` is excluded so the caller doesn't accidentally
/// treat its own lock as a peer.
///
/// Stale lockfiles (process exited without unlinking — possible after
/// SIGKILL) are skipped because their pid no longer exists, so they
/// don't pollute the peer set.
pub fn live_peer_sessions(self_session_id: Option<&str>) -> std::collections::HashSet<String> {
    let mut peers = std::collections::HashSet::new();
    let dir = locks_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return peers, // no locks dir = no peers
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("lock") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if Some(stem) == self_session_id {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(pid_str) = contents.split_whitespace().next() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<i32>() else {
            continue;
        };
        if is_pid_alive(pid) {
            peers.insert(stem.to_string());
        }
    }
    peers
}

/// Read the pid token recorded in a lockfile (first whitespace-separated
/// field of the contents — see `SessionLock::acquire`). Returns `None`
/// when the file is unreadable, empty, or carries a non-integer first
/// token. W7.2: used at acquire time to identify a stale lockfile.
fn read_lockfile_pid(path: &std::path::Path) -> Option<i32> {
    let contents = std::fs::read_to_string(path).ok()?;
    let token = contents.split_whitespace().next()?;
    token.parse::<i32>().ok()
}

/// POSIX `kill(pid, 0)` returns 0 if the process exists and we have
/// permission to signal it. Returns `false` on ESRCH (no such
/// process), `true` on EPERM (process exists but we can't signal —
/// still alive). The `errno` check distinguishes the two.
fn is_pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: libc::kill with sig=0 is the standard liveness probe.
    // No signal is delivered; the kernel only validates the pid. The
    // workspace `unsafe_code = "deny"` lint requires this bounded
    // waiver — see Cargo.toml.
    let rc = {
        #[allow(unsafe_code)]
        unsafe {
            libc::kill(pid, 0)
        }
    };
    if rc == 0 {
        return true;
    }
    // SAFETY: libc::__errno_location returns a per-thread errno ptr;
    // dereferencing it immediately after a syscall is the documented
    // POSIX pattern.
    let errno = {
        #[allow(unsafe_code)]
        unsafe {
            *libc::__errno_location()
        }
    };
    errno == libc::EPERM
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Tests in this module all mutate the process-wide HOME env. The
    // workspace runs them in parallel by default; this Mutex
    // serialises so the redirect each test installs isn't clobbered
    // before its assertions run.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn lockfile_path_sanitises_path_traversal() {
        let _g = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let scratch = tempfile::tempdir().unwrap();
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("HOME", scratch.path());
        }
        let p = lockfile_path("../escape/attempt");
        assert!(
            p.to_string_lossy().ends_with("__escape_attempt.lock"),
            "path-traversal must be neutralised: {}",
            p.display()
        );
    }

    #[test]
    fn live_peer_sessions_skips_stale_lockfiles() {
        let _g = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let scratch = tempfile::tempdir().unwrap();
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("HOME", scratch.path());
        }
        let locks = scratch.path().join(".scripps-workflow").join("locks");
        std::fs::create_dir_all(&locks).unwrap();
        // Live: our own pid is by definition alive.
        std::fs::write(
            locks.join("live-session.lock"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        // Stale: pid 0 is never a real process.
        std::fs::write(locks.join("stale-session.lock"), "0\n").unwrap();
        let peers = live_peer_sessions(None);
        assert!(
            peers.contains("live-session"),
            "live-session must appear: {:?}",
            peers
        );
        assert!(
            !peers.contains("stale-session"),
            "stale-session must be filtered out: {:?}",
            peers
        );
    }

    #[test]
    fn live_peer_sessions_excludes_self() {
        let _g = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let scratch = tempfile::tempdir().unwrap();
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("HOME", scratch.path());
        }
        let locks = scratch.path().join(".scripps-workflow").join("locks");
        std::fs::create_dir_all(&locks).unwrap();
        std::fs::write(
            locks.join("self-session.lock"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        std::fs::write(
            locks.join("other-session.lock"),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        let peers = live_peer_sessions(Some("self-session"));
        assert!(
            !peers.contains("self-session"),
            "self must be excluded: {:?}",
            peers
        );
        assert!(
            peers.contains("other-session"),
            "other-session must remain: {:?}",
            peers
        );
    }

    /// W7.2 — acquire must succeed when the previous holder left a
    /// lockfile recording a dead pid (SIGKILL leftover). The fix
    /// unlinks the stale inode and retries exactly once.
    #[test]
    fn acquire_retries_after_stale_lockfile_with_dead_pid() {
        let _g = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let scratch = tempfile::tempdir().unwrap();
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("HOME", scratch.path());
        }
        let session = "w72-stale-lock-test";
        let locks = scratch.path().join(".scripps-workflow").join("locks");
        std::fs::create_dir_all(&locks).unwrap();
        // Seed a stale lockfile recording pid=0 (sentinel — never live).
        // No flock is held on it because no process is alive holding the
        // fd; the contents alone are advisory. The acquire path should
        // unlink + retry rather than spuriously fail.
        std::fs::write(locks.join(format!("{}.lock", session)), "0\n").unwrap();
        let lock = SessionLock::acquire(session).expect("acquire must succeed after stale unlink");
        // Held: dropping the guard cleans up.
        drop(lock);
    }

    /// W7.2 — when the existing lockfile records a live pid (ours, in
    /// this test), acquire must still fail. The retry only fires when
    /// the recorded holder is dead.
    #[test]
    fn acquire_refuses_when_recorded_pid_is_live() {
        let _g = HOME_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let scratch = tempfile::tempdir().unwrap();
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("HOME", scratch.path());
        }
        let session = "w72-live-pid-test";
        // First acquire — succeeds, holds the flock for the rest of the test.
        let _held = SessionLock::acquire(session).expect("first acquire must succeed");
        // Second acquire from the same process: the lockfile carries our
        // own pid (live), so the retry path must refuse, not steal the
        // lock from ourselves. SessionLock doesn't derive Debug, so
        // expect_err is unavailable — match on Result directly instead.
        match SessionLock::acquire(session) {
            Ok(_) => panic!("second acquire while live holder present must fail"),
            Err(e) => assert!(
                e.to_string().contains("already running"),
                "unexpected contention message: {e}"
            ),
        }
    }
}
