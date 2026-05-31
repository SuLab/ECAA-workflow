//! C1 Phase-1 Task 1: harness-runtime sidecars are validated when PRESENT
//! and only skipped when ABSENT at emit time.
//!
//! Before this change, `locate_sidecar` returned `SkippedHarness` for the
//! two `SidecarSource::HarnessRuntime` sidecars (subgraph E
//! `validation-reports.jsonl` + subgraph Q `verifier-decisions.jsonl`)
//! unconditionally — even a present, malformed file slipped past the
//! conformance gate. Now a present file is validated regardless of source;
//! only an absent harness-runtime file is recorded as
//! `skipped_pending_harness`.
//!
//! `#[serial]` because the validator reads process-global env vars.

use ecaa_workflow_conversation::emit::validation::{validate_emitted_package, ValidationMode};
use serial_test::serial;
use std::path::{Path, PathBuf};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("ecaa-conformance/tests/fixtures/minimal-package")
        .canonicalize()
        .expect("minimal-package fixture must exist")
}

/// Recursively copy the canonical minimal-package fixture into a fresh
/// tempdir so we can mutate harness-runtime sidecars without touching the
/// in-repo fixture.
fn clone_fixture(dst: &Path) {
    let src = fixture_path();
    fn copy_dir(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).expect("create destination");
        for entry in std::fs::read_dir(src).expect("read fixture dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let target = dst.join(entry.file_name());
            if path.is_dir() {
                copy_dir(&path, &target);
            } else {
                std::fs::copy(&path, &target).expect("copy fixture file");
            }
        }
    }
    copy_dir(&src, dst);
}

fn clear_validation_env() {
    std::env::remove_var("ECAA_VALIDATE_ON_EMIT");
    std::env::remove_var("ECAA_VALIDATION_BLOCK_ON_FAIL");
    std::env::remove_var("ECAA_CONFORMANCE_MODE");
}

#[test]
#[serial]
fn present_harness_runtime_sidecar_is_validated() {
    clear_validation_env();
    std::env::set_var("ECAA_VALIDATE_ON_EMIT", "schema_only");

    let tmp = tempfile::tempdir().expect("create tempdir");
    let pkg = tmp.path();
    clone_fixture(pkg);

    // Subgraph E (`validation-reports.jsonl`) is the still-absent
    // harness-runtime sidecar → must count as `skipped_pending_harness`.
    std::fs::remove_file(pkg.join("runtime/validation-reports.jsonl"))
        .expect("remove validation-reports.jsonl");

    // Subgraph Q (`verifier-decisions.jsonl`) is present-but-malformed →
    // must be validated (and fail), NOT skipped.
    std::fs::write(
        pkg.join("runtime/verifier-decisions.jsonl"),
        "{not-valid-json\n",
    )
    .expect("write malformed verifier-decisions.jsonl");

    let summary = validate_emitted_package(pkg).expect("warn-only: must not block");
    assert_eq!(summary.mode, ValidationMode::SchemaOnly);

    // The present malformed Q sidecar is validated and reported as a failure.
    assert!(
        summary
            .schema_validation
            .failed
            .iter()
            .any(|f| f.sidecar == "runtime/verifier-decisions.jsonl"),
        "present malformed verifier-decisions.jsonl must be validated and fail; got failed={:?}",
        summary.schema_validation.failed
    );

    // Exactly the one still-absent harness-runtime sidecar is skipped.
    assert_eq!(
        summary.schema_validation.skipped_pending_harness, 1,
        "only the absent validation-reports.jsonl should be skipped; got {}",
        summary.schema_validation.skipped_pending_harness
    );

    clear_validation_env();
}

#[test]
#[serial]
fn absent_harness_runtime_sidecars_are_skipped_not_failed() {
    clear_validation_env();
    std::env::set_var("ECAA_VALIDATE_ON_EMIT", "schema_only");

    let tmp = tempfile::tempdir().expect("create tempdir");
    let pkg = tmp.path();
    clone_fixture(pkg);

    // Remove BOTH harness-runtime sidecars: a clean compile-time package
    // before any task has executed.
    std::fs::remove_file(pkg.join("runtime/validation-reports.jsonl"))
        .expect("remove validation-reports.jsonl");
    std::fs::remove_file(pkg.join("runtime/verifier-decisions.jsonl"))
        .expect("remove verifier-decisions.jsonl");

    let summary = validate_emitted_package(pkg).expect("warn-only: must not block");
    assert_eq!(summary.schema_validation.skipped_pending_harness, 2);
    assert!(
        !summary
            .schema_validation
            .failed
            .iter()
            .any(|f| f.sidecar.contains("validation-reports.jsonl")
                || f.sidecar.contains("verifier-decisions.jsonl")),
        "absent harness-runtime sidecars must NOT be recorded as failures; got {:?}",
        summary.schema_validation.failed
    );

    clear_validation_env();
}
