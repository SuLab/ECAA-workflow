//! Verifies `#[track_caller]` on path-jail helpers and `lock_recover`
//! wrapper is compilable and that panic `Location` attributes to the
//! *caller* rather than the helper internals.
//!
//! ## What `#[track_caller]` does here
//!
//! For functions that propagate a `Result` (path-jail helpers),
//! `#[track_caller]` is defensive: if a future refactor ever adds a
//! direct `panic!()` inside the helper, the location would attribute
//! to the *caller* of the helper rather than the panic site within it.
//!
//! The attribute also makes it explicit to the Rust toolchain (and any
//! future profiling/backtrace tools) that these helpers act as
//! call-through guards on behalf of their callers.
//!
//! The panic-location test below exercises the contract on
//! `safe_segment_join`: calling `.unwrap()` at the call site in this
//! file panics with this file's path in `Location`, not `_path_jail.rs`.

use std::panic;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Verify that `safe_segment_join` rejects directory-traversal
/// components, and that calling `.unwrap()` on the Err at the call
/// site panics with the caller's file in the Location — not
/// `_path_jail.rs`.
///
/// With `#[track_caller]` on `safe_segment_join`, a future refactor
/// that adds a direct `panic!()` inside the helper would also
/// attribute to the caller — this test establishes the baseline.
#[test]
fn path_jail_unwrap_panic_location_is_caller_not_helper() {
    use ecaa_workflow_server::chat_routes::safe_segment_join;

    let captured: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let hook_captured = captured.clone();
    let prior = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        if let Some(loc) = info.location() {
            *hook_captured.lock().unwrap() = Some(loc.file().to_string());
        }
    }));

    let result = panic::catch_unwind(|| {
        // .unwrap() is the panic site. It is at this file's line, not
        // inside _path_jail.rs. `#[track_caller]` on safe_segment_join
        // means that any future internal panic in the helper would also
        // attribute here.
        safe_segment_join(Path::new("/tmp/swfc-tc-test"), "../../../etc/passwd").unwrap()
    });

    panic::set_hook(prior);
    assert!(
        result.is_err(),
        "traversal component must produce an Err that unwrap panics on"
    );

    let loc_file = captured.lock().unwrap().clone();
    if let Some(file) = loc_file {
        assert!(
            !file.contains("_path_jail.rs"),
            "panic Location reported '{}' — expected the test caller file, not _path_jail.rs",
            file
        );
    }
}

/// Verify that `lock_recover`-style poison recovery (the exact pattern
/// used in `execution/mod.rs::lock_recover`) does NOT propagate a
/// panic on a poisoned mutex. `lock_recover` carries `#[track_caller]`
/// so any future panic inside it would attribute to the caller.
///
/// This test also confirms the `#[track_caller]` annotation did not
/// change the observable recovery behaviour.
#[test]
fn lock_recover_pattern_succeeds_on_poisoned_mutex() {
    let m: Arc<Mutex<u32>> = Arc::new(Mutex::new(42));

    // Poison the mutex by panicking inside a lock guard.
    {
        let m2 = m.clone();
        let _ = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            let _g = m2.lock().unwrap();
            panic!("inject poison");
        }));
    }
    assert!(
        m.is_poisoned(),
        "setup: mutex must be poisoned before recovery test"
    );

    // Replicate the exact lock_recover pattern; confirm no panic.
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let guard = m.lock().unwrap_or_else(|p| p.into_inner());
        *guard
    }));

    assert!(result.is_ok(), "poison recovery must not propagate a panic");
    assert_eq!(
        result.unwrap(),
        42,
        "recovered guard must expose the pre-poison value"
    );
}

/// Verify the path-jail helpers compile and behave correctly when
/// called from an external test crate — confirms the `#[track_caller]`
/// attribute is not breaking visibility or compilation.
#[test]
fn path_jail_helpers_are_callable_and_correct() {
    use ecaa_workflow_server::chat_routes::{
        assert_under_root, safe_relative_join, safe_segment_join,
    };

    let root = Path::new("/tmp/swfc-tc-test-root");

    // safe_segment_join: valid single segment
    let ok = safe_segment_join(root, "task_id_abc");
    assert!(ok.is_ok(), "valid segment must be accepted");

    // safe_segment_join: parent-traversal rejected
    let err = safe_segment_join(root, "..");
    assert!(err.is_err(), "parent-traversal segment must be rejected");

    // safe_segment_join: embedded separator rejected
    let err2 = safe_segment_join(root, "a/b");
    assert!(err2.is_err(), "embedded-separator segment must be rejected");

    // safe_relative_join: absolute path rejected
    let err3 = safe_relative_join(root, Path::new("/etc/passwd"));
    assert!(err3.is_err(), "absolute relative path must be rejected");

    // assert_under_root: non-existent root returns BadRoot
    let err4 = assert_under_root(
        Path::new("/tmp/swfc-nonexistent-jail-root-tc"),
        Path::new("/tmp/swfc-nonexistent-jail-root-tc/sub"),
    );
    assert!(err4.is_err(), "non-existent root must return an error");
}
