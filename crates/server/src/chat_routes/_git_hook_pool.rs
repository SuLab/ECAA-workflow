//! Bounded executor for fire-and-forget git-commit hooks.
//!
//! Without this pool every emit/branch/amend/proposal-promotion call
//! site would fire a raw `tokio::task::spawn_blocking(hook_commit_clone)`
//! and drop the JoinHandle. Tokio's blocking pool is ~512 slots; a
//! hanging `git add`, remote push, or SSH-key prompt could exhaust
//! the pool and starve every other blocking call on the server
//! (tokio::fs, child::wait, etc.).
//!
//! The pool bounds concurrent hooks at a fixed count + enforces a
//! per-hook timeout, and DROPS rather than queues over-capacity hooks
//! so a burst can never block the spawn() call or back-pressure the
//! request path. The drop is observability-surfaced via a `warn!` log
//! tagged with the originating `trigger`.
//!
//! When a hook is dropped (pool saturated) or times out, call sites
//! that supply a `DropNotifier` via [`GitHookPool::spawn_with_sink`]
//! receive a `(trigger, reason)` callback so they can fan the event
//! out onto the per-session SSE broadcast channel as a
//! `SsePayload::ProvenanceCommitDropped` event. This gives operators
//! a visible gap in the recovery-point timeline rather than a silent
//! stderr line.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

/// Callback invoked when a hook is dropped (pool saturated or timeout).
/// Receives `(trigger, reason)` where `trigger` is the operation name
/// ("emit", "amend", "branch", …) and `reason` is either
/// `"pool_saturated"` or `"timeout_secs=<n>"`.
pub type DropNotifier = Arc<dyn Fn(&str, &str) + Send + Sync + 'static>;

/// Bounded executor for fire-and-forget git-commit hooks. Drops
/// over-capacity hooks rather than queueing them; applies a wall-clock
/// timeout to each hook so a hung git operation releases its slot.
#[derive(Clone)]
pub struct GitHookPool {
    sem: Arc<Semaphore>,
    timeout: Duration,
}

impl GitHookPool {
    /// `max_concurrent` is the semaphore capacity — hooks beyond this
    /// budget are dropped with a `warn!` log. `per_hook_timeout` is
    /// the wall-clock budget given to each `spawn_blocking` body; the
    /// pool releases the slot when the timeout elapses regardless of
    /// the inner thread's progress, so a hung git operation cannot
    /// permanently consume a slot.
    pub fn new(max_concurrent: usize, per_hook_timeout: Duration) -> Self {
        Self {
            sem: Arc::new(Semaphore::new(max_concurrent)),
            timeout: per_hook_timeout,
        }
    }

    /// Submit a fire-and-forget git-hook. The hook's result is
    /// logged at warn/error level; the caller cannot recover from a
    /// failure (the request path that triggered the hook already
    /// committed to the user-visible state change). `trigger` is a
    /// static identifier ("emit", "amend", "branch", …) included in
    /// every log line for cross-request correlation.
    ///
    /// Non-blocking: the semaphore acquisition uses `try_acquire_owned`
    /// so a saturated pool drops the hook immediately rather than
    /// stalling the request thread. The actual `spawn_blocking` is
    /// dispatched from inside a `tokio::spawn` task so the caller's
    /// future never yields on hook dispatch.
    ///
    /// Drop events are only logged to stderr. Use
    /// [`GitHookPool::spawn_with_sink`] when operators need an SSE
    /// event for the provenance-gap signal.
    pub fn spawn(
        &self,
        trigger: &'static str,
        hook: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
    ) {
        self.spawn_with_sink(trigger, hook, None);
    }

    /// Like [`GitHookPool::spawn`] but accepts an optional
    /// [`DropNotifier`] that is called when the hook is dropped (pool
    /// saturated) or when it exceeds the per-hook wall-clock timeout.
    /// The notifier fires *after* the existing `tracing::warn!` so the
    /// stderr log is always emitted regardless of whether a notifier is
    /// wired up.
    ///
    /// Call sites that have access to a per-session SSE broadcaster
    /// should pass a notifier that fans a
    /// `SsePayload::ProvenanceCommitDropped` event so operators can see
    /// gaps in the recovery-point timeline without scraping stderr.
    pub fn spawn_with_sink(
        &self,
        trigger: &'static str,
        hook: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
        on_drop: Option<DropNotifier>,
    ) {
        let sem = self.sem.clone();
        let timeout = self.timeout;
        tokio::spawn(async move {
            // try_acquire_owned never awaits — saturated pool drops
            // the hook on the spot and the warn log lets the operator
            // know a git event was elided.
            let _permit = match sem.try_acquire_owned() {
                Ok(p) => p,
                Err(_) => {
                    tracing::warn!(trigger, "git hook dropped: pool saturated");
                    if let Some(notify) = on_drop {
                        notify(trigger, "pool_saturated");
                    }
                    return;
                }
            };
            let res = tokio::time::timeout(timeout, tokio::task::spawn_blocking(hook)).await;
            match res {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => {
                    tracing::warn!(trigger, error = %e, "git hook failed");
                }
                Ok(Err(e)) => {
                    tracing::error!(trigger, error = %e, "git hook join error (panic)");
                }
                Err(_) => {
                    let reason = format!("timeout_secs={}", timeout.as_secs());
                    tracing::warn!(
                        trigger,
                        timeout_secs = timeout.as_secs(),
                        "git hook timed out"
                    );
                    if let Some(notify) = on_drop {
                        notify(trigger, &reason);
                    }
                }
            }
            // _permit drops here, releasing the slot.
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn spawn_runs_the_hook_under_capacity() {
        let pool = GitHookPool::new(4, Duration::from_secs(5));
        let counter = Arc::new(AtomicU32::new(0));
        let c1 = counter.clone();
        pool.spawn("test", move || {
            c1.fetch_add(1, Ordering::Relaxed);
            Ok(())
        });
        // Let the spawned task run.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            counter.load(Ordering::Relaxed),
            1,
            "hook never ran under-capacity"
        );
    }

    #[tokio::test]
    async fn saturated_pool_drops_excess_hooks() {
        // Capacity 1; hold the slot with a long-running hook, then
        // attempt to enqueue a second that should be dropped.
        let pool = GitHookPool::new(1, Duration::from_secs(30));
        let counter = Arc::new(AtomicU32::new(0));
        let c1 = counter.clone();
        pool.spawn("test_hold", move || {
            std::thread::sleep(Duration::from_millis(500));
            c1.fetch_add(1, Ordering::Relaxed);
            Ok(())
        });
        // Give the first hook time to grab the semaphore.
        tokio::time::sleep(Duration::from_millis(100)).await;
        let c2 = counter.clone();
        pool.spawn("test_dropped", move || {
            // SHOULD NOT RUN — pool is at capacity, the spawn path's
            // try_acquire_owned should fail and the hook gets logged
            // + dropped.
            c2.fetch_add(100, Ordering::Relaxed);
            Ok(())
        });
        // Wait for the first hook to finish + the second to have had
        // its chance.
        tokio::time::sleep(Duration::from_millis(800)).await;
        let observed = counter.load(Ordering::Relaxed);
        assert_eq!(
            observed, 1,
            "expected only the first hook (counter=1) to run; \
             dropped hook somehow executed (counter={})",
            observed
        );
    }

    /// Fill a capacity-1 pool with one long-running hook (holding the
    /// only permit), then submit a second hook with a `DropNotifier`.
    /// The second must be rejected immediately (pool saturated) and the
    /// notifier must fire with `reason = "pool_saturated"`.
    #[tokio::test]
    async fn git_hook_pool_queue_full_emits_dropped_event() {
        use std::sync::atomic::AtomicBool;

        // Capacity 1 — a single long-running hook fills the pool.
        let pool = GitHookPool::new(1, Duration::from_secs(30));

        // Hold the only permit for 600 ms so the second spawn sees a
        // saturated pool.
        pool.spawn("test_hold", move || {
            std::thread::sleep(Duration::from_millis(600));
            Ok(())
        });

        // Give the first hook time to acquire the semaphore.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Set up the notifier that captures the drop signal.
        let notified = Arc::new(AtomicBool::new(false));
        let trigger_got: Arc<std::sync::Mutex<String>> =
            Arc::new(std::sync::Mutex::new(String::new()));
        let reason_got: Arc<std::sync::Mutex<String>> =
            Arc::new(std::sync::Mutex::new(String::new()));
        let n = notified.clone();
        let tg = trigger_got.clone();
        let rg = reason_got.clone();

        let notifier: DropNotifier = Arc::new(move |trigger: &str, reason: &str| {
            *tg.lock().unwrap() = trigger.to_string();
            *rg.lock().unwrap() = reason.to_string();
            n.store(true, Ordering::SeqCst);
        });

        // Spawn the second hook — pool is full, it must be dropped.
        pool.spawn_with_sink("emit", || Ok(()), Some(notifier));

        // The notifier should fire quickly (the async task that checks
        // the semaphore runs without any blocking work before the drop).
        tokio::time::sleep(Duration::from_millis(200)).await;

        assert!(
            notified.load(Ordering::SeqCst),
            "DropNotifier was not called for a saturated-pool drop"
        );
        assert_eq!(
            *trigger_got.lock().unwrap(),
            "emit",
            "trigger passed to notifier must match the spawn call"
        );
        assert_eq!(
            *reason_got.lock().unwrap(),
            "pool_saturated",
            "reason must be 'pool_saturated' for a queue-full drop"
        );
    }

    /// Spawn a hook that sleeps longer than the pool's timeout and assert
    /// the `DropNotifier` fires with `reason = "timeout_secs=<n>"`.
    #[tokio::test]
    async fn git_hook_pool_timeout_emits_dropped_event() {
        use std::sync::atomic::AtomicBool;

        // 100 ms timeout so the test stays fast.
        let pool = GitHookPool::new(4, Duration::from_millis(100));

        let notified = Arc::new(AtomicBool::new(false));
        let reason_got: Arc<std::sync::Mutex<String>> =
            Arc::new(std::sync::Mutex::new(String::new()));
        let n = notified.clone();
        let rg = reason_got.clone();

        let notifier: DropNotifier = Arc::new(move |_trigger: &str, reason: &str| {
            *rg.lock().unwrap() = reason.to_string();
            n.store(true, Ordering::SeqCst);
        });

        pool.spawn_with_sink(
            "amend",
            || {
                // Sleep longer than the 100 ms timeout.
                std::thread::sleep(Duration::from_millis(500));
                Ok(())
            },
            Some(notifier),
        );

        // Wait longer than the timeout + some scheduling slack.
        tokio::time::sleep(Duration::from_millis(400)).await;

        assert!(
            notified.load(Ordering::SeqCst),
            "DropNotifier was not called after a hook timeout"
        );
        // Reason format is "timeout_secs=<n>" where n=0 because 100ms
        // rounds to 0 in as_secs(). The key invariant is the prefix.
        assert!(
            reason_got.lock().unwrap().starts_with("timeout_secs="),
            "reason must start with 'timeout_secs=' for a timeout drop; got {:?}",
            *reason_got.lock().unwrap()
        );
    }
}
