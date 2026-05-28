//! Execution / harness kickoff endpoints. `start_execution` spawns
//! the harness subprocess against an emitted package; `get_execution`
//! returns current PID / status; `spawn_harness_for_session` is the
//! low-level helper invoked both from the REST handler and from the
//! `ServiceEventSink`. `get_dag` lives here too because it's primarily
//! consumed by the Jobs tab (execution-adjacent). Helpers
//! `resume_blocked_tasks_in_workflow` and `fail_blocked_tasks_in_workflow`
//! rewrite task state in the emitted package when the SME unblocks /
//! aborts a session.
//!
//! Domain split into per-action submodules to keep each
//! file under the §S5.9 400-LOC modularity cap:
//! - `start.rs` — `start_execution` + `spawn_harness_for_session`
//! + auto-relaunch helpers.
//! - `pause_resume.rs` — pause + resume sentinel handlers.
//! - `stop_kill.rs` — cooperative stop + hard kill.
//! - `status.rs` — `get_execution` + `get_dag`.
//!
//! `mod.rs` is merge-only: re-exports public handlers + helpers and
//! aggregates each submodule's `routes()` builder + `ROUTES` inventory.

mod pause_resume;
mod start;
mod status;
mod stop_kill;

// Re-export public handlers so `chat_routes/mod.rs::pub use execution::{...}`
// keeps working unchanged.
pub use pause_resume::{post_pause_execution, post_resume_execution};
pub use start::start_execution;
pub use status::{get_dag, get_execution};
pub use stop_kill::{post_kill_execution, post_stop_execution};

// Re-export shared helpers + types at the `execution` namespace so
// sibling modules (`turns`, `tasks`, `dispositions`, `remediation`,
// `event_sink`) keep referencing them as
// `execution::maybe_auto_relaunch_harness` / `execution::SpawnHarnessError`
// / `execution::resume_blocked_tasks_in_workflow` etc. Visibility is
// `pub(super)` (= `pub(in chat_routes)`) to mirror the pre-split
// `pub(super)` envelope on the originals.
pub(super) use start::{
    fail_blocked_tasks_in_workflow, maybe_auto_relaunch_harness, resume_blocked_tasks_in_workflow,
    spawn_harness_for_session, SpawnHarnessError,
};

/// Uniform poison-recovery for execution-handle mutexes (RC-21).
/// A panic inside any `lock()` (e.g. the watcher task at
/// `start.rs::child.wait()` panicking) poisons the `Mutex` and turns
/// every subsequent `lock().unwrap()` into a permanent 500 on
/// status/pause/stop/kill. `PoisonError::into_inner` preserves the
/// inner state (the inner data wasn't actually corrupted — the panic
/// was on the holder side, not the data side) so the recovery is
/// behaviorally identical to a fresh lock for the read-and-decide
/// uses on `ExecutionHandle`.
///
/// Used by `pause_resume.rs`, `status.rs`, `start.rs`,
/// `stop_kill.rs`, and `branches.rs`.
#[track_caller]
pub(super) fn lock_recover<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

// Convenience constructors for `ExecutionHandle`. The two
// constructors below pin "fresh / unset" semantics for the
// `pause_requested` / `stop_requested` / `paused_at` /
// `stop_requested_at` fields in one place — every call site (the
// production spawn path, branches.rs running/exited, and the
// status/pause_resume/stop_kill test helpers) initialises those four
// the same way, and divergence would silently change lifecycle
// behavior.
use super::app_state::EXIT_STATUS_UNSET;
use super::ExecutionHandle;
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex};

impl ExecutionHandle {
    /// Construct a handle representing a freshly-spawned, still-running
    /// harness. `exit_status` is initialised to the `EXIT_STATUS_UNSET`
    /// sentinel; the production spawn path clones the Arc into the
    /// watcher task which then `exit_status_set`s the reaped code.
    ///
    /// The `pause_requested` / `stop_requested` atomics start `false`
    /// and the timestamp mutexes start `None`. Lifecycle endpoints
    /// flip these in place.
    pub fn for_running(
        pid: u32,
        pgid: u32,
        package_dir: std::path::PathBuf,
        agent_command: String,
    ) -> Self {
        Self {
            pid,
            pgid,
            started_at: chrono::Utc::now(),
            package_dir,
            agent_command,
            exit_status: Arc::new(AtomicI64::new(EXIT_STATUS_UNSET)),
            pause_requested: Arc::new(AtomicBool::new(false)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            paused_at: Arc::new(Mutex::new(None)),
            stop_requested_at: Arc::new(Mutex::new(None)),
        }
    }

    /// Construct a handle representing a process that has already
    /// exited with the given status code. Used by tests that need to
    /// assert "exited" status / branches.rs's recent-sessions
    /// fixture; production never constructs this shape (the watcher
    /// task in `start.rs::spawn_harness_for_session` mutates the
    /// `exit_status` field of a long-lived handle when the child
    /// reaps).
    ///
    /// Useful defaults are baked in: `package_dir` is `/tmp/fake-pkg`,
    /// `agent_command` is `/bin/true`. Callers that care about the
    /// path can construct via `for_running(...)` and then set the
    /// `exit_status` directly.
    pub fn for_exited(pid: u32, pgid: u32, exit_status_value: i32) -> Self {
        Self {
            pid,
            pgid,
            started_at: chrono::Utc::now(),
            package_dir: std::path::PathBuf::from("/tmp/fake-pkg"),
            agent_command: "/bin/true".to_string(),
            exit_status: Arc::new(AtomicI64::new(exit_status_value as i64)),
            pause_requested: Arc::new(AtomicBool::new(false)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            paused_at: Arc::new(Mutex::new(None)),
            stop_requested_at: Arc::new(Mutex::new(None)),
        }
    }
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/chat/session/:id/dag"),
    ("POST", "/api/chat/session/:id/start-execution"),
    ("GET", "/api/chat/session/:id/execution"),
    ("POST", "/api/chat/session/:id/execution/pause"),
    ("POST", "/api/chat/session/:id/execution/resume"),
    ("POST", "/api/chat/session/:id/execution/stop"),
    ("POST", "/api/chat/session/:id/execution/kill"),
];

pub(super) fn routes() -> axum::Router<super::ChatAppState> {
    axum::Router::new()
        .route("/api/chat/session/:id/dag", axum::routing::get(get_dag))
        .route(
            "/api/chat/session/:id/start-execution",
            axum::routing::post(start_execution),
        )
        .route(
            "/api/chat/session/:id/execution",
            axum::routing::get(get_execution),
        )
        .route(
            "/api/chat/session/:id/execution/pause",
            axum::routing::post(post_pause_execution),
        )
        .route(
            "/api/chat/session/:id/execution/resume",
            axum::routing::post(post_resume_execution),
        )
        .route(
            "/api/chat/session/:id/execution/stop",
            axum::routing::post(post_stop_execution),
        )
        .route(
            "/api/chat/session/:id/execution/kill",
            axum::routing::post(post_kill_execution),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn lock_recover_returns_inner_state_after_panic_poisons_mutex() {
        let m = Mutex::new(42_u32);
        // Poison the mutex by panicking inside a `lock()` guard.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = m.lock().unwrap();
            panic!("inject poison");
        }));
        assert!(
            m.is_poisoned(),
            "test scaffolding precondition: mutex must be poisoned"
        );
        // The bare `m.lock().unwrap()` would now panic-on-Err. The
        // recovery helper returns the inner value (which the panicking
        // closure never modified — 42).
        let guard = lock_recover(&m);
        assert_eq!(*guard, 42, "poison recovery must return the inner value");
    }
}
