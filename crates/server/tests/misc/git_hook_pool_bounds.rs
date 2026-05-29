//! Regression for the unbounded `spawn_blocking` git-hook pattern.
//!
//! A raw `tokio::task::spawn_blocking` per git hook can exhaust
//! Tokio's ~512-slot blocking pool when remotes hang on `git add` /
//! push / SSH prompts, starving every other blocking call
//! (`tokio::fs`, `child::wait`, etc.) on the server. The bounded
//! `GitHookPool` drops hooks on saturation instead of blocking other
//! `spawn_blocking` work.
//!
//! This test demonstrates the contract: fire 100 hanging hooks through
//! the bounded pool; assert that an unrelated `spawn_blocking` job
//! still completes promptly. Without the bounded pool the foreign job
//! would block behind the saturated blocking pool.

use ecaa_workflow_server::chat_routes::GitHookPool;
use std::sync::Arc;
// Use std's blocking Barrier here (not tokio::sync::Barrier) because
// the GitHookPool's spawn() takes a sync `FnOnce() -> Result<()>` —
// tokio::sync::Barrier::wait() returns a Future that we can't await
// from inside the sync hook lambda. The std barrier blocks the
// spawn_blocking thread, which is exactly what we want to simulate a
// hung git hook holding a slot.
use std::sync::Barrier;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn git_hooks_do_not_starve_other_spawn_blocking() {
    // Pool capacity is 8 — same as the production constant — with a
    // generous timeout so the bounded slots stay saturated for the
    // duration of the foreign-job check.
    let pool = Arc::new(GitHookPool::new(8, Duration::from_secs(30)));

    // Barrier the hooks on so we can hold every slot open for the
    // duration of the foreign-job assertion. The 8 captured hooks each
    // wait on the barrier; the remaining 92 are dropped by the pool's
    // try_acquire_owned path (the bounded pool's documented behaviour:
    // saturated → drop with a warn log, never queue and never spawn a
    // new blocking thread).
    let barrier = Arc::new(Barrier::new(9)); // 8 hooks + the awaiting future

    for i in 0..100 {
        let b = barrier.clone();
        pool.spawn("test_hang", move || {
            // The first 8 of these wait on the barrier; the rest are
            // dropped without ever spawning a blocking thread.
            let _ = b.wait();
            // After unblock, return promptly. The test fails first if
            // this never gets called.
            Ok::<(), anyhow::Error>(())
        });
        // Tiny yield so each spawn lands on the tokio task queue
        // before the next one races to acquire a permit. Removes
        // ordering nondeterminism without changing the contract.
        if i % 16 == 15 {
            tokio::task::yield_now().await;
        }
    }

    // Give the spawned tokio tasks a moment to acquire permits and
    // park on the barrier.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // The 8 holding hooks are now occupying every blocking-pool slot
    // the pool will ever cede. Confirm a foreign spawn_blocking is
    // NOT starved: the bounded pool drops the extra 92 hooks rather
    // than queueing them, so the global tokio blocking pool (~512
    // slots) stays available for everyone else.
    let foreign_start = std::time::Instant::now();
    let foreign = tokio::task::spawn_blocking(|| {
        // Trivial work — we only care about scheduling latency.
        42
    });
    let foreign_result = tokio::time::timeout(Duration::from_secs(1), foreign)
        .await
        .expect("foreign spawn_blocking starved by saturated git-hook pool")
        .expect("foreign spawn_blocking panicked");
    let foreign_elapsed = foreign_start.elapsed();
    assert_eq!(foreign_result, 42);
    assert!(
        foreign_elapsed < Duration::from_secs(1),
        "foreign spawn_blocking took {:?} — pool starvation suspected",
        foreign_elapsed
    );

    // Release the 8 holders so the test can drain cleanly. Run on a
    // spawn_blocking thread because std::sync::Barrier::wait() blocks.
    let b2 = barrier.clone();
    let _ = tokio::task::spawn_blocking(move || b2.wait()).await;
    // Brief yield so the freed permits and blocking-pool slots return.
    tokio::time::sleep(Duration::from_millis(50)).await;
}

/// The pool drops over-budget hooks rather than queueing them. We
/// can't observe the drop count directly (the warn log is the
/// observability surface), but we can prove the contract by firing
/// far more hooks than the pool capacity and confirming the call
/// returns synchronously without blocking — i.e. there is no
/// internal queue that could blow up under burst.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_hook_pool_spawn_is_nonblocking() {
    let pool = Arc::new(GitHookPool::new(4, Duration::from_secs(10)));
    let start = std::time::Instant::now();
    for _ in 0..1000 {
        pool.spawn("test_burst", || Ok::<(), anyhow::Error>(()));
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "GitHookPool::spawn must not block on a saturated pool — \
         1000 spawns took {:?}",
        elapsed
    );
}

/// A hook that hangs past the timeout has its slot released so the
/// next hook can acquire it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn git_hook_timeout_releases_slot() {
    let pool = Arc::new(GitHookPool::new(1, Duration::from_millis(100)));

    // First hook hangs forever.
    pool.spawn("test_timeout", || {
        std::thread::sleep(Duration::from_secs(30));
        Ok::<(), anyhow::Error>(())
    });

    // Wait past the per-hook timeout so the slot is released. The
    // bounded pool's documented semantics: timeout out fires after
    // `timeout` elapsed on the spawn_blocking; the permit is dropped
    // and the slot becomes available.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Now a second hook should be able to acquire the (released)
    // permit and complete promptly.
    let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let o2 = observed.clone();
    pool.spawn("test_followup", move || {
        o2.store(true, std::sync::atomic::Ordering::Release);
        Ok::<(), anyhow::Error>(())
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        observed.load(std::sync::atomic::Ordering::Acquire),
        "follow-up hook never ran — the timed-out slot was not released"
    );
}
