//! Exhaustiveness anchor for `TaskState::is_terminal`.
//!
//! `Session::set_task_state` (conversation crate) and the HTTP task-state
//! handler (server crate) both call `is_terminal()` to enforce the
//! monotonicity invariant: a terminal task must never regress to a
//! non-terminal state. Correctness of that guard depends on
//! `is_terminal()` being up-to-date with the full set of `TaskState`
//! variants.
//!
//! This test uses an exhaustive `match` — **no `_` wildcard, no `..`
//! catch-all** — so that adding a new `TaskState` variant produces
//! E0004 (non-exhaustive patterns) at compile time, forcing the
//! contributor to decide whether the new variant is terminal and to
//! update both `is_terminal()` and this test together.
//!
//! Invariant: `Completed` and `Failed` are terminal. All other current
//! variants (`Pending`, `Ready`, `Running`, `Blocked`) are non-terminal.

use ecaa_workflow_core::dag::{BlockedRecord, TaskState};

/// Asserts the expected `is_terminal()` return value for every
/// `TaskState` variant by name.
///
/// The `match` is intentionally exhaustive: no `_` arm, no `..`
/// patterns that would silently absorb a new variant. Adding a variant
/// to `TaskState` will cause E0004 here, which is the desired behavior.
#[test]
fn is_terminal_covers_all_variants() {
    // Non-terminal states — harness may still transition them forward.
    let pending = TaskState::Pending;
    assert!(!pending.is_terminal(), "Pending must not be terminal");

    let ready = TaskState::Ready;
    assert!(!ready.is_terminal(), "Ready must not be terminal");

    let running = TaskState::Running {
        started_at: "2026-01-01T00:00:00Z".to_string(),
        remote: None,
    };
    assert!(!running.is_terminal(), "Running must not be terminal");

    // Blocked is recoverable (SME unblock returns the task to Ready) —
    // not terminal even though it halts dispatch.
    let blocked = TaskState::Blocked {
        record: BlockedRecord {
            reason: "test".to_string(),
            attempts: vec![],
        },
    };
    assert!(
        !blocked.is_terminal(),
        "Blocked must not be terminal (SME unblock can recover it)"
    );

    // Terminal states — the harness considers these final for a given
    // task dispatch. Monotonicity guard rejects any Completed/Failed →
    // non-terminal transition.
    let completed = TaskState::Completed {
        result: serde_json::Value::Null,
    };
    assert!(completed.is_terminal(), "Completed must be terminal");

    let failed = TaskState::Failed {
        reason: "test".to_string(),
    };
    assert!(failed.is_terminal(), "Failed must be terminal");

    // Exhaustiveness anchor: this match has NO wildcard arm. If a new
    // variant is added to `TaskState`, the compiler will emit E0004
    // here. The contributor must then:
    //   1. Decide whether the new variant is terminal (final dispatch)
    //      or non-terminal (recoverable / transitional).
    //   2. Update `TaskState::is_terminal()` in `crates/core/src/dag.rs`
    //      to handle the new variant explicitly.
    //   3. Add the variant to the assertions above and to this anchor
    //      match, then update the comment in `dag.rs` that lists
    //      non-terminal states.
    //
    // `match` arms below call `is_terminal()` purely to ensure the
    // compiler sees every variant referenced; the logic assertions above
    // already cover correctness.
    let variants: &[&TaskState] = &[&pending, &ready, &running, &blocked, &completed, &failed];
    for state in variants {
        let _ = match state {
            TaskState::Pending => state.is_terminal(),
            TaskState::Ready => state.is_terminal(),
            TaskState::Running { .. } => state.is_terminal(),
            TaskState::Blocked { .. } => state.is_terminal(),
            TaskState::Completed { .. } => state.is_terminal(),
            TaskState::Failed { .. } => state.is_terminal(),
            // NO `_` arm — new variants must be handled explicitly.
        };
    }
}
