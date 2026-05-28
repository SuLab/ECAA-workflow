//! Multi-process server lock keyed by the session-store directory.
//!
//! Two server processes pointed at the same `SWFC_CHAT_SESSIONS_DIR`
//! would race on `atomic_write_rename` (the same `<id>.json.tmp` →
//! `<id>.json` swap) and on the per-session HashMaps the prune-hook
//! GC operates on. Symptoms of a collision are subtle: lost updates
//! (whoever's `rename` lost), torn writes (one process reads
//! mid-flight), and inconsistent in-memory state (each process
//! observes the on-disk file the other left).
//!
//! Deployment-topology constraint enforced by this lock: a given
//! session-store directory may have at most one live server process
//! attached. The on-disk lockfile is a host-level flock at
//! `~/.scripps-workflow/locks/server-store-<hash>.lock` where `<hash>`
//! is a content hash of the canonicalised session-store path; the
//! kernel drops the flock when the process exits (even via SIGKILL).
//!
//! This is intentionally a process-wide lock, not a per-session lock,
//! because per-session locks at the route-handler layer would (1)
//! double the syscall count on every mutating request and (2) miss
//! the prune-loop / batcher write paths that don't run inside a
//! per-route guard. A single boot-time acquire on the store dir is
//! both cheaper and more comprehensive.
//!
//! Bypass: `SWFC_SERVER_DEBUG_ALLOW_MULTI_PROCESS=1` skips the
//! acquire entirely so multi-server integration tests can opt out.
//! Production never sets this.
//!
//! Mirrors the harness-side
//! [`ecaa_workflow_harness::multiprocess_lock::SessionLock`]: same
//! POSIX `flock(LOCK_EX | LOCK_NB)` semantics, same `.scripps-workflow/
//! locks/` directory, same bypass-env-var pattern (so an operator
//! debugging a contention issue can audit both layers with one
//! `ls` of the locks dir).

use anyhow::{anyhow, Context, Result};
use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

/// Process-bound host lock keyed by the session-store directory's
/// canonicalised path. The flock is released by the kernel when the
/// file descriptor drops; the lockfile is best-effort unlinked in
/// `Drop`.
pub struct ServerSessionStoreLock {
    file: Option<std::fs::File>,
    path: PathBuf,
}

impl ServerSessionStoreLock {
    /// Acquire a non-blocking exclusive flock on the lockfile for
    /// `session_store_dir`. Fails when another live server already
    /// holds it; the caller (`lib::run`) prints the contention
    /// message and exits.
    ///
    /// Honors `SWFC_SERVER_DEBUG_ALLOW_MULTI_PROCESS=1` by returning
    /// a sentinel that holds no real lock — only test harnesses that
    /// need two server processes deliberately should set it.
    pub fn acquire(session_store_dir: &Path) -> Result<Self> {
        if std::env::var("SWFC_SERVER_DEBUG_ALLOW_MULTI_PROCESS")
            .ok()
            .as_deref()
            == Some("1")
        {
            return Ok(Self {
                file: None,
                path: PathBuf::new(),
            });
        }
        let path = lockfile_path(session_store_dir);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating lock directory {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .with_context(|| format!("opening lock file {}", path.display()))?;
        let fd = file.as_raw_fd();
        // SAFETY: libc::flock with a valid fd is sound. Ownership of
        // the fd stays with `file`; `flock` does not transfer it.
        // LOCK_EX | LOCK_NB returns -1 with errno EWOULDBLOCK when a
        // peer process holds the lock.
        let rc = {
            #[allow(unsafe_code)]
            unsafe {
                libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB)
            }
        };
        if rc != 0 {
            let err = std::io::Error::last_os_error();
            return Err(anyhow!(
                "could not acquire server session-store lock {}: {} \
                 (another ecaa-workflow-server pointed at {} is already running; \
                 set SWFC_SERVER_DEBUG_ALLOW_MULTI_PROCESS=1 to bypass for tests)",
                path.display(),
                err,
                session_store_dir.display()
            ));
        }
        // Best-effort: write pid + start hint into the file so
        // operators can diagnose contention without strace.
        let _ = std::fs::write(
            &path,
            format!(
                "{} {}\n",
                std::process::id(),
                chrono::Utc::now().to_rfc3339()
            ),
        );
        Ok(Self {
            file: Some(file),
            path,
        })
    }

    /// Path of the lockfile this guard holds. Exposed for diagnostic
    /// logging at boot.
    #[allow(dead_code)] // reserved-for-diagnostics
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for ServerSessionStoreLock {
    fn drop(&mut self) {
        // The kernel drops the flock when the fd closes; we
        // additionally unlink the file so `ls
        // ~/.scripps-workflow/locks` doesn't accumulate stale entries.
        if self.file.is_some() && !self.path.as_os_str().is_empty() {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Derive the lockfile path from a session-store directory. The
/// canonicalised path is hashed via `sha2::Sha256` so two distinct
/// session-store dirs always produce distinct lockfile names even
/// when their basename collides (e.g. `/var/.../sessions` vs
/// `/home/.../sessions`). When canonicalisation fails (dir doesn't
/// exist yet at boot — rare, the store opens before this), we fall
/// back to the un-canonicalised path.
fn lockfile_path(session_store_dir: &Path) -> PathBuf {
    use sha2::Digest;
    let canon = std::fs::canonicalize(session_store_dir)
        .unwrap_or_else(|_| session_store_dir.to_path_buf());
    let mut hasher = sha2::Sha256::new();
    hasher.update(canon.as_os_str().as_encoded_bytes());
    let digest = hasher.finalize();
    let short = hex::encode(&digest[..8]);
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".scripps-workflow")
        .join("locks")
        .join(format!("server-store-{short}.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn acquire_succeeds_for_fresh_dir() {
        let tmp = tempdir().unwrap();
        let lock = ServerSessionStoreLock::acquire(tmp.path()).unwrap();
        assert!(lock.path().exists(), "lockfile must be created on acquire");
    }

    #[test]
    fn second_acquire_against_same_dir_fails() {
        let tmp = tempdir().unwrap();
        let _lock1 = ServerSessionStoreLock::acquire(tmp.path()).unwrap();
        let result = ServerSessionStoreLock::acquire(tmp.path());
        assert!(
            result.is_err(),
            "second acquire against same store dir must fail while first guard is alive"
        );
    }

    #[test]
    fn lockfile_dropped_on_guard_drop() {
        let tmp = tempdir().unwrap();
        let lock = ServerSessionStoreLock::acquire(tmp.path()).unwrap();
        let path = lock.path().to_path_buf();
        drop(lock);
        // Drop unlinks the lockfile.
        assert!(
            !path.exists(),
            "lockfile must be removed when guard drops; remains at {}",
            path.display()
        );
    }

    #[test]
    fn bypass_env_returns_sentinel() {
        // SAFETY: process-wide env mutation is sound for single-threaded test scope.
        std::env::set_var("SWFC_SERVER_DEBUG_ALLOW_MULTI_PROCESS", "1");
        let tmp = tempdir().unwrap();
        // Two acquires must both succeed under the bypass.
        let _l1 = ServerSessionStoreLock::acquire(tmp.path()).unwrap();
        let _l2 = ServerSessionStoreLock::acquire(tmp.path()).unwrap();
        std::env::remove_var("SWFC_SERVER_DEBUG_ALLOW_MULTI_PROCESS");
    }
}
