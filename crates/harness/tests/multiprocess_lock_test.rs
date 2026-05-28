//! Process harness lock.
//!
//! Spike 16.0.C showed that two harness processes can legitimately
//! share a `session_id` (server-spawned dispatch + a manual CLI
//! `cargo run -p ecaa-workflow-harness -- --session-id...`).
//! Both then race on `WORKFLOW.json`, the dispatch WAL, and the
//! AWS instance tags.
//!
//! `SessionLock::acquire` takes a non-blocking POSIX `flock(LOCK_EX |
//! LOCK_NB)` on `~/.scripps-workflow/locks/<session_id>.lock`. The
//! second concurrent caller for the same id must fail; once the first
//! is dropped, the third caller succeeds.

use ecaa_workflow_harness::multiprocess_lock::SessionLock;
use std::sync::Mutex;

// All tests mutate HOME + (in one case) ECAA_HARNESS_DEBUG_ALLOW_MULTI_PROCESS,
// which is process-global. Serialize so parallel test threads don't
// clobber each other's redirect before the assertions run.
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn second_acquire_for_same_session_id_fails() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    // SAFETY: tests serialized via ENV_LOCK; we redirect HOME before
    // the lock helper consults it.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("HOME", scratch.path());
    }

    let sid = format!("phase-20-1-test-{}", std::process::id());
    let first = SessionLock::acquire(&sid).expect("first acquire must succeed");
    let second = SessionLock::acquire(&sid);
    assert!(
        second.is_err(),
        "second acquire for the same session_id must fail while the first is held"
    );
    drop(first);
    let third =
        SessionLock::acquire(&sid).expect("third acquire after dropping the first must succeed");
    drop(third);
}

#[test]
fn different_session_ids_do_not_collide() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("HOME", scratch.path());
    }

    let sid_a = format!("phase-20-1-a-{}", std::process::id());
    let sid_b = format!("phase-20-1-b-{}", std::process::id());
    let _a = SessionLock::acquire(&sid_a).expect("session-a acquire");
    let _b = SessionLock::acquire(&sid_b).expect("session-b acquire must succeed independently");
}

#[test]
fn debug_env_var_bypasses_lock() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let scratch = tempfile::tempdir().unwrap();
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("HOME", scratch.path());
        std::env::set_var("ECAA_HARNESS_DEBUG_ALLOW_MULTI_PROCESS", "1");
    }
    let sid = format!("phase-20-1-bypass-{}", std::process::id());
    let first = SessionLock::acquire(&sid).expect("first acquire (bypass)");
    let second =
        SessionLock::acquire(&sid).expect("second acquire must succeed when bypass env-var is set");
    drop(second);
    drop(first);
    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("ECAA_HARNESS_DEBUG_ALLOW_MULTI_PROCESS");
    }
}
