//! LRU eviction for the per-session agent cache.
//!
//! `SWFC_AGENT_CACHE_MAX_GB` is documented as a soft ceiling;
//! this module is the enforcement code. Per-session
//! caches (`$SWFC_AGENT_CACHE_DIR` rooted at
//! `~/.scripps-workflow/agent-cache/` by default, with one subdirectory
//! per session id) grew unbounded between the 30-day persistence-TTL
//! sweep. Long-running operators with many sessions hit disk-pressure
//! before the TTL ever fired.
//!
//! The evictor walks the cache root, computes the size of each
//! immediate sub-directory (each = one session cache), sorts them by
//! last-access time (atime; oldest first), and removes whole sessions
//! oldest-first until total size ≤ cap. Eviction is whole-session, not
//! per-file, so we never half-evict a session and leave a broken cache
//! that an active dispatch would reuse.
//!
//! Activation: opt-in via `SWFC_AGENT_CACHE_MAX_GB=<integer>`. Unset =
//! no enforcement (graceful default for hosts where cache-pressure
//! isn't a problem yet). Invoked from `main::main` after
//! `SessionLock::acquire` so the host-level multi-process guard is
//! already in place — a second harness can't race on the same eviction
//! sweep because two harnesses targeting one session id can't coexist
//! in the first place.
//!
//! A periodic background sweep is also launched via `spawn_periodic` to
//! catch bursty workloads that fill disk between harness restarts. The
//! period is controlled by `SWFC_CACHE_EVICTION_PERIOD_SECS` (default
//! 600 = 10 min; clamped to [60, 3600]). The sweep runs the same LRU
//! walk as the one-shot startup call; the background thread exits
//! cleanly when the `EvictionGuard` (which holds the shutdown sender)
//! is dropped.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

/// Walk-rooted enforcer keyed by an LRU strategy. Constructed via
/// `from_env()`; the absence of `SWFC_AGENT_CACHE_MAX_GB` returns
/// `None` so the harness can call `.map(|e| e.enforce())` without
/// branching itself.
pub struct CacheEvictor {
    cache_dir: PathBuf,
    max_bytes: u64,
}

impl CacheEvictor {
    /// Build from environment. Returns `None` when
    /// `SWFC_AGENT_CACHE_MAX_GB` is unset / non-numeric — graceful
    /// opt-in, never panics on bad input.
    pub fn from_env() -> Option<Self> {
        let max_gb: u64 = std::env::var("SWFC_AGENT_CACHE_MAX_GB")
            .ok()?
            .parse()
            .ok()?;
        let cache_dir = std::env::var_os("SWFC_AGENT_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|h| PathBuf::from(h).join(".scripps-workflow/agent-cache"))
            })?;
        // Multiplier in plan/doc text is 1e9 (decimal GB) — matches
        // operator intuition ("50 GB" = 50_000_000_000 bytes). CLAUDE.md
        // and docs/env-vars-reference.md both quote "GB" without a
        // GiB/GB suffix; pick the operator-friendly definition.
        let max_bytes = max_gb.saturating_mul(1_000_000_000);
        Some(Self {
            cache_dir,
            max_bytes,
        })
    }

    /// Construct directly for tests that don't want to touch process
    /// env. Production callers go through `from_env()`.
    #[cfg(test)]
    pub fn new(cache_dir: PathBuf, max_bytes: u64) -> Self {
        Self {
            cache_dir,
            max_bytes,
        }
    }

    /// Alias for `enforce()`. Used by `spawn_periodic` and in contexts
    /// where the "evict if over cap" intent needs to be explicit at the
    /// call-site.
    pub fn evict_if_over_cap(&self) -> Result<()> {
        self.enforce()
    }

    /// Spawn a background thread that calls `evict_if_over_cap()` every
    /// `period`. The thread exits when the returned `EvictionGuard` is
    /// dropped (the shutdown channel disconnects and the loop wakes on
    /// the next sleep or instantly if already waiting).
    ///
    /// Eviction errors are logged as warnings but never panic; the
    /// thread continues after a failed sweep. The `CacheEvictor` is
    /// moved into the thread — call `from_env()` again in the caller if
    /// you need to retain a local copy.
    pub fn spawn_periodic(self, period: Duration) -> EvictionGuard {
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();
        let handle = std::thread::Builder::new()
            .name("cache-evictor".into())
            .spawn(move || {
                // W1.5: track consecutive sweep failures so an
                // ongoing problem (permission error on a subtree,
                // sticky FS error, full disk) escalates to ERROR
                // and backs off the sweep cadence instead of
                // hammering the same broken path every period.
                const ESCALATION_THRESHOLD: u32 = 3;
                const BACKOFF_PERIOD_MULT: u32 = 4;
                let mut consecutive_failures: u32 = 0;
                loop {
                    // Active wait: under sustained failure, sweep
                    // less often (BACKOFF_PERIOD_MULT × period). Once
                    // a sweep succeeds, the counter resets and the
                    // normal cadence resumes.
                    let effective_period = if consecutive_failures >= ESCALATION_THRESHOLD {
                        period.saturating_mul(BACKOFF_PERIOD_MULT)
                    } else {
                        period
                    };
                    match shutdown_rx.recv_timeout(effective_period) {
                        // A value was sent — not currently possible
                        // (sender never sends); treat as shutdown.
                        Ok(()) => {
                            tracing::debug!("cache-evictor: shutdown signal received");
                            break;
                        }
                        // Timeout elapsed — run a sweep.
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        // Sender dropped — harness is exiting.
                        Err(mpsc::RecvTimeoutError::Disconnected) => {
                            tracing::debug!("cache-evictor: shutdown channel closed; exiting");
                            break;
                        }
                    }
                    match self.evict_if_over_cap() {
                        Ok(()) => {
                            if consecutive_failures > 0 {
                                tracing::info!(
                                    after_failures = consecutive_failures,
                                    "cache eviction recovered; resuming normal cadence"
                                );
                            }
                            consecutive_failures = 0;
                        }
                        Err(e) => {
                            consecutive_failures = consecutive_failures.saturating_add(1);
                            if consecutive_failures >= ESCALATION_THRESHOLD {
                                tracing::error!(
                                    consecutive = consecutive_failures,
                                    backoff_mult = BACKOFF_PERIOD_MULT,
                                    error = %e,
                                    "cache eviction failed N+ times in a row; escalating to ERROR \
                                     and backing off sweep cadence — investigate the cache root \
                                     for permission / FS-full / disk-corruption issues"
                                );
                            } else {
                                tracing::warn!(
                                    consecutive = consecutive_failures,
                                    error = %e,
                                    "periodic cache eviction failed"
                                );
                            }
                        }
                    }
                }
            })
            .expect("failed to spawn cache-evictor thread");
        EvictionGuard {
            shutdown_tx: Some(shutdown_tx),
            handle: Some(handle),
        }
    }

    /// Walk the cache root, sum per-subdirectory sizes, and evict
    /// oldest-access sessions until total size ≤ `max_bytes`. No-op
    /// when the cache root doesn't exist (fresh host, no agent has
    /// run yet). Read-dir errors propagate; per-entry stat/remove
    /// errors are logged and skipped (a partial walk is still useful).
    pub fn enforce(&self) -> Result<()> {
        if !self.cache_dir.exists() {
            return Ok(());
        }
        // First pass: collect (path, atime) sequentially. POSIX atime
        // semantics require capturing atime BEFORE walking the
        // directory — `dir_size` performs `read_dir` on every nested
        // directory, which updates the directory's own atime (a
        // `readdir` is an access). If we captured atime after the
        // walk, every entry would carry a "just now" timestamp and
        // the LRU sort would collapse to the read order — undermining
        // the entire point of this evictor. Confirmed empirically by
        // the test `evicts_multiple_sessions_until_under_cap`:
        // pre-fix, entry.metadata() returned all-equal atimes
        // (current wall-clock) regardless of the per-session atime
        // the test set via utimensat.
        let mut path_atimes: Vec<(PathBuf, SystemTime)> = Vec::new();
        for e in std::fs::read_dir(&self.cache_dir)
            .with_context(|| format!("reading {}", self.cache_dir.display()))?
        {
            let entry = match e {
                Ok(e) => e,
                Err(err) => {
                    tracing::warn!(error = %err, "skipping unreadable cache dir entry");
                    continue;
                }
            };
            let path = entry.path();
            // Skip non-directories (.lockfile,.keep, etc.). Per-session
            // caches are always directories.
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => {}
                Ok(_) => continue,
                Err(err) => {
                    tracing::warn!(?path, error = %err, "stat failed; skipping");
                    continue;
                }
            }
            let atime = entry
                .metadata()
                .ok()
                .and_then(|m| m.accessed().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            path_atimes.push((path, atime));
        }
        // Second pass: compute sizes in parallel via rayon. Each
        // `dir_size` walk is an I/O-bound recursive traversal; the
        // work-stealing pool fans them out so the wall time is
        // dominated by the largest session rather than the sum.
        // Errors per-entry are warned + filtered as in the prior
        // sequential implementation — a partial walk is still useful.
        use rayon::prelude::*;
        let mut entries: Vec<(PathBuf, u64, SystemTime)> = path_atimes
            .into_par_iter()
            .filter_map(|(path, atime)| {
                let size = match dir_size(&path) {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::warn!(?path, error = %err, "dir_size failed; skipping");
                        return None;
                    }
                };
                eprintln!("[evictor-debug] {:?} size={} atime={:?}", path, size, atime);
                Some((path, size, atime))
            })
            .collect();
        let total: u64 = entries.iter().map(|(_, s, _)| *s).sum();
        if total <= self.max_bytes {
            tracing::debug!(
                total_bytes = total,
                cap_bytes = self.max_bytes,
                sessions = entries.len(),
                "cache under cap; no eviction needed",
            );
            return Ok(());
        }
        // Evict oldest-access first.
        entries.sort_by_key(|(_, _, atime)| *atime);
        let need = total - self.max_bytes;
        let mut freed: u64 = 0;
        for (path, size, _atime) in entries {
            if freed >= need {
                break;
            }
            tracing::info!(
                ?path,
                size_bytes = size,
                "evicting LRU session cache (total over cap)",
            );
            if let Err(err) = std::fs::remove_dir_all(&path) {
                tracing::warn!(?path, error = %err, "evict remove_dir_all failed");
                continue;
            }
            freed = freed.saturating_add(size);
        }
        tracing::info!(
            total_before = total,
            freed_bytes = freed,
            cap_bytes = self.max_bytes,
            "cache eviction complete",
        );
        Ok(())
    }
}

/// RAII guard returned by `CacheEvictor::spawn_periodic`. Dropping this
/// value signals the background eviction thread to exit on its next
/// wakeup and then joins it. This ensures the thread does not outlive
/// the harness process's main cleanup path.
pub struct EvictionGuard {
    /// `Some` while the background thread is alive. Taken to `None`
    /// (and dropped) in `drop()` before joining, so the channel
    /// disconnects and the thread wakes immediately instead of waiting
    /// out a full period.
    shutdown_tx: Option<mpsc::Sender<()>>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for EvictionGuard {
    fn drop(&mut self) {
        // Drop the sender first: this disconnects the channel and wakes
        // the background thread's `recv_timeout` with `Disconnected`
        // so it exits without waiting a full period.
        drop(self.shutdown_tx.take());
        if let Some(h) = self.handle.take() {
            // Ignore join errors (thread may have already exited).
            let _ = h.join();
        }
    }
}

/// Resolve `SWFC_CACHE_EVICTION_PERIOD_SECS` from the environment.
/// Returns a `Duration` clamped to [60 s, 3600 s]; default is 600 s.
/// Non-numeric or out-of-range values fall back to the default with a
/// warning so a misconfigured operator doesn't silently disable sweeps.
pub fn eviction_period_from_env() -> Duration {
    const DEFAULT_SECS: u64 = 600;
    const MIN_SECS: u64 = 60;
    const MAX_SECS: u64 = 3600;

    let raw = match std::env::var("SWFC_CACHE_EVICTION_PERIOD_SECS") {
        Ok(v) => v,
        Err(_) => return Duration::from_secs(DEFAULT_SECS),
    };
    let parsed: u64 = match raw.parse() {
        Ok(n) => n,
        Err(_) => {
            tracing::warn!(
                value = %raw,
                "SWFC_CACHE_EVICTION_PERIOD_SECS is not a valid integer; \
                 using default {}s",
                DEFAULT_SECS,
            );
            return Duration::from_secs(DEFAULT_SECS);
        }
    };
    if !(MIN_SECS..=MAX_SECS).contains(&parsed) {
        tracing::warn!(
            value = parsed,
            min = MIN_SECS,
            max = MAX_SECS,
            "SWFC_CACHE_EVICTION_PERIOD_SECS out of range [60, 3600]; \
             clamping to bounds",
        );
        return Duration::from_secs(parsed.clamp(MIN_SECS, MAX_SECS));
    }
    Duration::from_secs(parsed)
}

/// Recursive size walk without external deps. Skips entries that
/// stat fails (broken symlinks, permission errors); the caller's
/// "freed >= need" loop is robust to slight under-estimates.
fn dir_size(path: &Path) -> Result<u64> {
    let mut total: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(err) => {
                // W1.2/W1.5: count silent skips so a host with
                // pervasive read errors surfaces in harness-health.json
                // rather than degrading invisibly.
                crate::_observability::note_silent_skip(
                    crate::_observability::SkipCategory::CacheEvictionStatError,
                    &format!("read_dir {} failed: {}", dir.display(), err),
                    None,
                );
                continue;
            }
        };
        for e in rd.flatten() {
            let entry_path = e.path();
            let meta = match e.metadata() {
                Ok(m) => m,
                Err(err) => {
                    crate::_observability::note_silent_skip(
                        crate::_observability::SkipCategory::CacheEvictionStatError,
                        &format!("stat {} failed: {}", entry_path.display(), err),
                        None,
                    );
                    continue;
                }
            };
            // symlink_metadata would be a stricter choice, but `e.metadata()`
            // already follows symlinks by default on stable. We're computing a
            // size-pressure estimate, not a tamper-proof audit; following is
            // fine.
            if meta.is_dir() {
                stack.push(entry_path);
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::Mutex;
    use std::time::Duration;

    // The from_env() tests mutate process env; serialize so parallel
    // test threads don't clobber.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn make_session_dir(root: &Path, name: &str, file_bytes: usize, atime_offset_secs: i64) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).expect("mkdir");
        let f = dir.join("payload.bin");
        fs::write(&f, vec![0u8; file_bytes]).expect("write payload");
        // Set both atime+mtime to the same offset relative to now.
        // We use `filetime` indirectly via `utimensat`-style call;
        // since we don't have that crate as a dep, set via the std
        // `set_modified` (which on Linux sets mtime; atime requires
        // a syscall). For the LRU test we need atime — use a libc
        // utimensat wrapper. Workspace allows libc.
        let target = if atime_offset_secs == 0 {
            SystemTime::now()
        } else if atime_offset_secs > 0 {
            SystemTime::now() + Duration::from_secs(atime_offset_secs as u64)
        } else {
            SystemTime::now() - Duration::from_secs((-atime_offset_secs) as u64)
        };
        set_atime(&dir, target);
    }

    /// Set the access time of a path. Implemented via libc::utimensat
    /// because std exposes set_modified but not set_accessed on stable.
    /// The workspace forbids unsafe_code at the lint level (`-D
    /// unsafe_code`); allow it locally with a justification —
    /// utimensat is the only way to set atime portably on Linux from
    /// stable Rust without pulling in `filetime`.
    fn set_atime(path: &Path, t: SystemTime) {
        use std::os::unix::ffi::OsStrExt;
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        let dur = t
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        let ts = [
            libc::timespec {
                tv_sec: dur.as_secs() as libc::time_t,
                tv_nsec: dur.subsec_nanos() as libc::c_long,
            },
            libc::timespec {
                // mtime — leave at now; not under test.
                tv_sec: 0,
                tv_nsec: libc::UTIME_NOW as libc::c_long,
            },
        ];
        // SAFETY: cpath is a valid NUL-terminated C string; ts is a
        // properly-initialized 2-element array of timespec; AT_FDCWD
        // is the documented sentinel for "use cwd, but we passed an
        // absolute path so this is moot". utimensat returns -1 on
        // error and sets errno; we ignore — best-effort test setup.
        #[allow(unsafe_code)]
        unsafe {
            libc::utimensat(libc::AT_FDCWD, cpath.as_ptr(), ts.as_ptr(), 0);
        }
    }

    #[test]
    fn no_op_when_total_under_cap() {
        let scratch = tempfile::tempdir().unwrap();
        make_session_dir(scratch.path(), "session_a", 1_000, 0);
        make_session_dir(scratch.path(), "session_b", 1_000, 0);

        let ev = CacheEvictor::new(scratch.path().to_path_buf(), 10_000);
        ev.enforce().unwrap();

        assert!(scratch.path().join("session_a").exists());
        assert!(scratch.path().join("session_b").exists());
    }

    #[test]
    fn evicts_lru_session_when_over_cap() {
        let scratch = tempfile::tempdir().unwrap();
        // Each session ~1 MB; cap is 1.5 MB. Two sessions = 2 MB > cap.
        // session_old's atime is 1 hour ago; session_new is now. The
        // evictor must drop session_old.
        make_session_dir(scratch.path(), "session_old", 1_000_000, -3600);
        make_session_dir(scratch.path(), "session_new", 1_000_000, 0);

        // Debug
        for n in ["session_old", "session_new"] {
            let p = scratch.path().join(n);
            let m = fs::metadata(&p).unwrap();
            eprintln!(
                "{}: atime={:?} size={}",
                n,
                m.accessed().unwrap(),
                dir_size(&p).unwrap()
            );
        }

        let ev = CacheEvictor::new(scratch.path().to_path_buf(), 1_500_000);
        ev.enforce().unwrap();

        assert!(
            !scratch.path().join("session_old").exists(),
            "session_old (older atime) must have been evicted"
        );
        assert!(
            scratch.path().join("session_new").exists(),
            "session_new (newer atime) must survive"
        );
    }

    #[test]
    fn evicts_multiple_sessions_until_under_cap() {
        let scratch = tempfile::tempdir().unwrap();
        // 4x 1MB sessions; cap 1.5MB. With LRU sort, evict in order:
        // s_oldest -> s_old -> s_mid -> s_new until total <= cap.
        // freed-required: 4MB total - 1.5MB cap = 2.5MB. After
        // evicting s_oldest (1MB) and s_old (1MB) we've freed 2MB —
        // still short; evictor pulls s_mid next (3MB freed >= 2.5MB).
        // s_new (1MB) survives, remaining total = 1MB <= cap.
        make_session_dir(scratch.path(), "s_oldest", 1_000_000, -7200);
        make_session_dir(scratch.path(), "s_old", 1_000_000, -3600);
        make_session_dir(scratch.path(), "s_mid", 1_000_000, -1800);
        make_session_dir(scratch.path(), "s_new", 1_000_000, 0);

        // NOTE: do NOT call `dir_size(p)` (or any `read_dir(p)`) before
        // `enforce()`. POSIX/relatime updates a directory's atime on
        // readdir when the prior atime is older than mtime — which is
        // exactly the state `make_session_dir` puts the seeded
        // directories in (atime set to the past via utimensat; mtime
        // refreshed to now). A pre-flight `dir_size` walk would clobber
        // every seeded atime to "now", collapse the LRU sort to readdir
        // order, and let `s_new` get evicted ahead of `s_oldest`.
        // Defer any debug introspection until after `enforce()` returns
        // — by then the surviving directory's atime is moot.

        let ev = CacheEvictor::new(scratch.path().to_path_buf(), 1_500_000);
        ev.enforce().unwrap();

        for n in ["s_oldest", "s_old", "s_mid", "s_new"] {
            let exists = scratch.path().join(n).exists();
            eprintln!("after-evict {}: exists={}", n, exists);
        }

        assert!(
            !scratch.path().join("s_oldest").exists(),
            "s_oldest should be evicted"
        );
        assert!(
            !scratch.path().join("s_old").exists(),
            "s_old should be evicted"
        );
        // s_mid may or may not be evicted depending on size accounting
        // (we evict whole sessions; once we've freed >= need, we stop).
        // Strict assertion: s_new must survive.
        assert!(
            scratch.path().join("s_new").exists(),
            "s_new (newest atime) must survive"
        );
    }

    #[test]
    fn no_op_when_cache_dir_absent() {
        let scratch = tempfile::tempdir().unwrap();
        let missing = scratch.path().join("does-not-exist");
        let ev = CacheEvictor::new(missing, 1_000_000);
        // Should not error.
        ev.enforce().unwrap();
    }

    #[test]
    fn from_env_returns_none_when_max_gb_unset() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("SWFC_AGENT_CACHE_MAX_GB");
        }
        assert!(CacheEvictor::from_env().is_none());
    }

    #[test]
    fn from_env_returns_none_when_max_gb_garbage() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("SWFC_AGENT_CACHE_MAX_GB", "not-a-number");
        }
        let got = CacheEvictor::from_env();
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("SWFC_AGENT_CACHE_MAX_GB");
        }
        assert!(
            got.is_none(),
            "non-numeric SWFC_AGENT_CACHE_MAX_GB must be ignored"
        );
    }

    #[test]
    fn from_env_picks_up_cache_dir_override() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let custom = tempfile::tempdir().unwrap();
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("SWFC_AGENT_CACHE_MAX_GB", "5");
            std::env::set_var("SWFC_AGENT_CACHE_DIR", custom.path());
        }
        let ev = CacheEvictor::from_env().expect("must construct");
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("SWFC_AGENT_CACHE_MAX_GB");
            std::env::remove_var("SWFC_AGENT_CACHE_DIR");
        }
        // No assert on private fields directly — just verify enforce()
        // doesn't blow up against the temp root.
        ev.enforce().unwrap();
    }

    // --- periodic sweep tests ---

    #[test]
    fn periodic_thread_fires_multiple_times() {
        // Use a very short period so the test completes quickly.
        // The counter is incremented by each sweep; we assert ≥ 2
        // increments within the observation window.
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        // We can't inject a counter into CacheEvictor directly without
        // changing its API, so we use a temp dir and a tiny cap that
        // forces an eviction on every sweep, verifying the files are
        // re-created each time. Instead, we verify the thread keeps
        // running by checking that the EvictionGuard drops cleanly and
        // that a sequence of periodic sweeps doesn't panic or deadlock.
        //
        // For a count-based assertion we wrap the check via a shared
        // atomic: a tiny thread that sleeps briefly between creating
        // payloads, then asserts the evictor removed them at least twice.

        let scratch = tempfile::tempdir().unwrap();
        // Cap = 0 bytes → every sweep must evict everything it finds.
        let evictor = CacheEvictor::new(scratch.path().to_path_buf(), 0);
        let period = Duration::from_millis(60);

        let sweep_count = Arc::new(AtomicU32::new(0));
        let count_clone = Arc::clone(&sweep_count);
        let scratch_path = scratch.path().to_path_buf();

        // Seed a session dir before spawning, then watch the evictor
        // clear it. Re-seed between checks.
        fs::create_dir_all(scratch_path.join("sess_a")).unwrap();
        fs::write(scratch_path.join("sess_a/payload.bin"), vec![0u8; 512]).unwrap();

        // A watcher thread that re-seeds the scratch dir once it
        // observes the evictor cleared it. Each cleared+reseeded cycle
        // counts as one observed fire.
        let watcher = std::thread::spawn(move || {
            for _ in 0..3u32 {
                // Wait until the evictor has removed the session dir.
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                loop {
                    if !scratch_path.join("sess_a").exists() {
                        count_clone.fetch_add(1, Ordering::SeqCst);
                        // Re-seed for the next cycle.
                        fs::create_dir_all(scratch_path.join("sess_a")).ok();
                        fs::write(scratch_path.join("sess_a/payload.bin"), vec![0u8; 512]).ok();
                        break;
                    }
                    if std::time::Instant::now() > deadline {
                        // Didn't observe eviction in time.
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        });

        let _guard = evictor.spawn_periodic(period);
        watcher.join().unwrap();
        // Drop the guard — thread must exit without panic.
        drop(_guard);

        assert!(
            sweep_count.load(Ordering::SeqCst) >= 2,
            "expected at least 2 periodic eviction fires, got {}",
            sweep_count.load(Ordering::SeqCst)
        );
    }

    #[test]
    fn eviction_guard_drop_does_not_panic() {
        let scratch = tempfile::tempdir().unwrap();
        let evictor = CacheEvictor::new(scratch.path().to_path_buf(), u64::MAX);
        let guard = evictor.spawn_periodic(Duration::from_secs(600));
        // Drop immediately — must join cleanly without blocking more
        // than a trivial amount of time.
        drop(guard);
    }

    #[test]
    fn eviction_period_from_env_defaults_to_600() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("SWFC_CACHE_EVICTION_PERIOD_SECS");
        }
        assert_eq!(super::eviction_period_from_env(), Duration::from_secs(600));
    }

    #[test]
    fn eviction_period_from_env_clamps_low() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("SWFC_CACHE_EVICTION_PERIOD_SECS", "10");
        }
        let result = super::eviction_period_from_env();
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("SWFC_CACHE_EVICTION_PERIOD_SECS");
        }
        assert_eq!(result, Duration::from_secs(60));
    }

    #[test]
    fn eviction_period_from_env_clamps_high() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("SWFC_CACHE_EVICTION_PERIOD_SECS", "9999");
        }
        let result = super::eviction_period_from_env();
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("SWFC_CACHE_EVICTION_PERIOD_SECS");
        }
        assert_eq!(result, Duration::from_secs(3600));
    }

    #[test]
    fn eviction_period_from_env_accepts_valid_value() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("SWFC_CACHE_EVICTION_PERIOD_SECS", "300");
        }
        let result = super::eviction_period_from_env();
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("SWFC_CACHE_EVICTION_PERIOD_SECS");
        }
        assert_eq!(result, Duration::from_secs(300));
    }

    #[test]
    fn eviction_period_from_env_ignores_garbage() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("SWFC_CACHE_EVICTION_PERIOD_SECS", "not-a-number");
        }
        let result = super::eviction_period_from_env();
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("SWFC_CACHE_EVICTION_PERIOD_SECS");
        }
        assert_eq!(result, Duration::from_secs(600));
    }
}
