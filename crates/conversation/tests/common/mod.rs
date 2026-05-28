//! Shared test helpers. Lives at `tests/common/mod.rs` so Cargo's
//! integration-test discovery doesn't pick it up as its own binary
//! (only `tests/*.rs` is auto-discovered; subdirectories are
//! library-form modules referenced via `#[path = "common/mod.rs"]
//! mod common;` from each consumer).
//!
//! ── `TestEnv` ──────────────────────────────────────────────────────────
//!
//! Replaces the `std::mem::forget(TempDir)` / `Box::leak(TempDir)`
//! pattern that kept tempdirs alive for the lifetime of the test
//! process. The leak idiom hides handles from `Drop` and accumulates
//! GiB of `/tmp` on a long `cargo test` run; valgrind / heaptrack
//! reports them as "definitely lost"; CI containers with constrained
//! `/tmp` (4 GiB on `ubuntu-latest`) eventually exhaust the device.
//!
//! `Arc<TempDir>` solves the original problem (the tempdir must outlive
//! every `SessionStore::open` background task that holds a `Path`
//! reference) without leaking: every clone shares ownership; the
//! actual `TempDir::drop` only fires when the last `Arc` is dropped.
//! When the test ends, every `Arc` clone is dropped, including the
//! one held by `TestEnv`, and the tempdir cleans up normally.

// `unreachable_pub` would flag every public item here because Cargo
// compiles this module via `#[path = "common/mod.rs"] mod common;`
// from each integration-test binary, which makes the parent module
// private. The items genuinely need `pub` so the parent test files
// can call them — `pub(crate)` would not work since each test is a
// separate crate. Allow the lint scope-wide.
#![allow(unreachable_pub)]

use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

/// Owned, RAII-cleaned tempdir for test fixtures. Construct via
/// [`TestEnv::new`]; access the underlying path via [`TestEnv::path`].
/// Clone freely — the inner `Arc<TempDir>` is the shared owner.
#[derive(Clone)]
#[allow(dead_code)]
pub struct TestEnv {
    pub temp: Arc<TempDir>,
}

#[allow(dead_code)]
impl TestEnv {
    /// Allocate a fresh tempdir under the system temp root.
    pub fn new() -> Self {
        Self {
            temp: Arc::new(TempDir::new().expect("tempdir")),
        }
    }

    /// Borrow the underlying path. The borrow lives no longer than
    /// `&self`; clone the `TestEnv` (cheap — bumps the Arc refcount)
    /// when you need to hand a path-bearing handle to a background
    /// task or async closure.
    pub fn path(&self) -> &Path {
        self.temp.path()
    }
}

impl Default for TestEnv {
    fn default() -> Self {
        Self::new()
    }
}
