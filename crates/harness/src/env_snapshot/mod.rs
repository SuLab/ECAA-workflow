//! Compute-environment snapshot — shared types and top-level orchestrator.
//!
//! `cache_scan` detects whether the session cache already contains installed
//! packages before the snapshot build is attempted.
//!
//! `snapshot_environment` is the main entry point called at end-of-run
//! finalize (Task 6).  It decides whether to snapshot, builds the image, and
//! stores it — returning a [`SnapshotOutcome`] that is always a value (never
//! a panic or propagated error).

use std::io;
use std::path::PathBuf;

pub mod build;
pub mod cache_scan;
pub mod record;
pub mod store;

/// Options controlling whether and where a snapshot is captured.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotOpts {
    pub enabled: bool,
    pub registry: Option<String>,
    pub base_digest: String,
    pub source_date_epoch: i64,
    pub cache_dir: PathBuf,
}

/// Where a captured snapshot was stored.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreLocation {
    Registry(String),
    LocalCas(PathBuf),
}

/// Outcome of a snapshot attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum SnapshotOutcome {
    Captured {
        digest: String,
        location: StoreLocation,
        note: Option<String>,
    },
    SkippedNoInstalls,
    Failed {
        reason: String,
    },
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

/// Attempt to capture a compute-environment snapshot.
///
/// Decision logic:
/// 1. If `opts.enabled` is `false` → [`SnapshotOutcome::SkippedNoInstalls`].
/// 2. If the cache directory contains no installed packages →
///    [`SnapshotOutcome::SkippedNoInstalls`].
/// 3. Build the snapshot image; on failure → [`SnapshotOutcome::Failed`].
/// 4. Store the image according to [`store::select_store`]; on registry-push
///    failure, retry with the local-CAS fallback and return a
///    [`SnapshotOutcome::Captured`] with a `note` explaining the fallback.
///    If the fallback also fails → [`SnapshotOutcome::Failed`].
///
/// This function is **non-fatal**: it never panics or propagates errors to the
/// caller.
pub fn snapshot_environment(opts: &SnapshotOpts) -> SnapshotOutcome {
    snapshot_environment_with(opts, build::build_image, store::store_image)
}

/// Testable inner implementation with injected build/store seams.
///
/// Accepts any callable with the same signature as [`build::build_image`] and
/// [`store::store_image`] so that hermetic unit tests can substitute
/// deterministic stubs without invoking docker.
fn snapshot_environment_with<B, S>(opts: &SnapshotOpts, build_fn: B, store_fn: S) -> SnapshotOutcome
where
    B: Fn(&SnapshotOpts) -> io::Result<String>,
    S: Fn(&str, &str, &store::StorePlan) -> io::Result<StoreLocation>,
{
    // Step 1: respect the enabled flag.
    if !opts.enabled {
        return SnapshotOutcome::SkippedNoInstalls;
    }

    // Step 2: only snapshot when something was actually installed.
    if !cache_scan::cache_has_installs(&opts.cache_dir) {
        return SnapshotOutcome::SkippedNoInstalls;
    }

    // Step 3: build the snapshot image.
    let digest = match build_fn(opts) {
        Ok(d) => d,
        Err(e) => {
            return SnapshotOutcome::Failed {
                reason: format!("build failed: {e}"),
            }
        }
    };

    // Step 4: derive tag + cache dir (pure helpers — single source of truth).
    let tag = build::snapshot_image_tag(&opts.base_digest);
    let buildx = build::resolve_buildx_cache_dir();
    let plan = store::select_store(opts.registry.as_deref(), &buildx);

    // Step 5: store the image.
    match store_fn(&tag, &digest, &plan) {
        Ok(location) => SnapshotOutcome::Captured {
            digest,
            location,
            note: None,
        },
        Err(push_err) => {
            // Step 6: if the primary plan was a registry push, fall back to
            // local CAS so the snapshot is not silently lost.
            if matches!(plan, store::StorePlan::Push { .. }) {
                let fallback_plan = store::select_store(None, &buildx);
                match store_fn(&tag, &digest, &fallback_plan) {
                    Ok(location) => SnapshotOutcome::Captured {
                        digest,
                        location,
                        note: Some(format!(
                            "registry push failed ({push_err}); kept local"
                        )),
                    },
                    Err(cas_err) => SnapshotOutcome::Failed {
                        reason: format!(
                            "registry push failed ({push_err}); local CAS also failed: {cas_err}"
                        ),
                    },
                }
            } else {
                SnapshotOutcome::Failed {
                    reason: format!("store failed: {push_err}"),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn base_opts(cache_dir: PathBuf) -> SnapshotOpts {
        SnapshotOpts {
            enabled: true,
            registry: None,
            base_digest: "sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                .to_owned(),
            source_date_epoch: 0,
            cache_dir,
        }
    }

    /// Create a temp dir that satisfies `cache_has_installs` (conda env with bin/).
    fn cache_dir_with_installs() -> tempfile::TempDir {
        let t = tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("conda-envs/myenv/bin")).unwrap();
        std::fs::write(t.path().join("conda-envs/myenv/bin/python"), "x").unwrap();
        t
    }

    /// Create a temp dir that does NOT satisfy `cache_has_installs`.
    fn cache_dir_empty() -> tempfile::TempDir {
        let t = tempdir().unwrap();
        for d in ["conda-envs", "R-libs", "pip"] {
            std::fs::create_dir_all(t.path().join(d)).unwrap();
        }
        t
    }

    // -----------------------------------------------------------------------
    // Test 1: disabled → SkippedNoInstalls, build_fn NOT called
    // -----------------------------------------------------------------------
    #[test]
    fn disabled_returns_skipped_without_calling_build() {
        let t = cache_dir_with_installs();
        let mut opts = base_opts(t.path().to_path_buf());
        opts.enabled = false;

        let build_called = Cell::new(false);
        let outcome = snapshot_environment_with(
            &opts,
            |_o| {
                build_called.set(true);
                Ok("sha256:x".to_owned())
            },
            |_tag, _digest, _plan| {
                Ok(StoreLocation::LocalCas(PathBuf::from("/tmp/fake.tar")))
            },
        );

        assert_eq!(outcome, SnapshotOutcome::SkippedNoInstalls);
        assert!(!build_called.get(), "build_fn must not be called when disabled");
    }

    // -----------------------------------------------------------------------
    // Test 2: enabled but no installs → SkippedNoInstalls, build_fn NOT called
    // -----------------------------------------------------------------------
    #[test]
    fn no_installs_returns_skipped_without_calling_build() {
        let t = cache_dir_empty();
        let opts = base_opts(t.path().to_path_buf());

        let build_called = Cell::new(false);
        let outcome = snapshot_environment_with(
            &opts,
            |_o| {
                build_called.set(true);
                Ok("sha256:x".to_owned())
            },
            |_tag, _digest, _plan| {
                Ok(StoreLocation::LocalCas(PathBuf::from("/tmp/fake.tar")))
            },
        );

        assert_eq!(outcome, SnapshotOutcome::SkippedNoInstalls);
        assert!(!build_called.get(), "build_fn must not be called when no installs present");
    }

    // -----------------------------------------------------------------------
    // Test 3: enabled + installs + build Ok + store Ok → Captured
    // -----------------------------------------------------------------------
    #[test]
    fn enabled_with_installs_and_successful_build_and_store_returns_captured() {
        let t = cache_dir_with_installs();
        let opts = base_opts(t.path().to_path_buf());
        let cas_path = PathBuf::from("/tmp/snapshot.tar");

        let outcome = snapshot_environment_with(
            &opts,
            |_o| Ok("sha256:deadbeef1234".to_owned()),
            |_tag, _digest, _plan| Ok(StoreLocation::LocalCas(cas_path.clone())),
        );

        assert_eq!(
            outcome,
            SnapshotOutcome::Captured {
                digest: "sha256:deadbeef1234".to_owned(),
                location: StoreLocation::LocalCas(cas_path),
                note: None,
            }
        );
    }

    // -----------------------------------------------------------------------
    // Test 4: build_fn returns Err → Failed (NOT a panic, NOT propagated)
    // -----------------------------------------------------------------------
    #[test]
    fn build_failure_returns_failed_non_fatal() {
        let t = cache_dir_with_installs();
        let opts = base_opts(t.path().to_path_buf());

        let outcome = snapshot_environment_with(
            &opts,
            |_o| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "docker build exploded",
                ))
            },
            |_tag, _digest, _plan| {
                Ok(StoreLocation::LocalCas(PathBuf::from("/tmp/fake.tar")))
            },
        );

        assert!(
            matches!(outcome, SnapshotOutcome::Failed { .. }),
            "expected Failed variant, got {outcome:?}"
        );
        if let SnapshotOutcome::Failed { reason } = outcome {
            assert!(
                reason.contains("docker build exploded"),
                "reason should mention the original error: {reason}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Test 5: registry push fails → fallback to local CAS → Captured with note
    // -----------------------------------------------------------------------
    #[test]
    fn registry_push_failure_falls_back_to_local_cas_with_note() {
        let t = cache_dir_with_installs();
        let mut opts = base_opts(t.path().to_path_buf());
        opts.registry = Some("ghcr.io/test/repo".to_owned());

        let cas_path = Arc::new(PathBuf::from("/tmp/fallback.tar"));
        let cas_path_clone = Arc::clone(&cas_path);

        let outcome = snapshot_environment_with(
            &opts,
            |_o| Ok("sha256:cafebabe5678".to_owned()),
            move |_tag, _digest, plan| {
                match plan {
                    store::StorePlan::Push { .. } => Err(std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        "registry unreachable",
                    )),
                    store::StorePlan::LocalCas { .. } => {
                        Ok(StoreLocation::LocalCas((*cas_path_clone).clone()))
                    }
                }
            },
        );

        match &outcome {
            SnapshotOutcome::Captured { digest, location, note } => {
                assert_eq!(digest, "sha256:cafebabe5678");
                assert_eq!(*location, StoreLocation::LocalCas((*cas_path).clone()));
                let note_text = note.as_deref().unwrap_or("");
                assert!(
                    note_text.contains("registry push failed"),
                    "note should mention registry push failed: {note_text}"
                );
                assert!(
                    note_text.contains("kept local"),
                    "note should mention kept local: {note_text}"
                );
            }
            other => panic!("expected Captured with note, got {other:?}"),
        }
    }
}
