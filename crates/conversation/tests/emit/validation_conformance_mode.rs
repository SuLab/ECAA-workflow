//! C1 Phase-1 Task 2: `ECAA_CONFORMANCE_MODE` forces block-on-fail and
//! upgrades `Disabled` -> `SchemaOnly`.
//!
//! A conformant build must never silently skip validation, and any schema
//! failure must abort the emit even when `ECAA_VALIDATION_BLOCK_ON_FAIL`
//! is unset.
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
fn conformance_mode_blocks_on_missing_required_sidecar() {
    clear_validation_env();
    // Conformance mode ON; explicit BLOCK_ON_FAIL deliberately UNSET.
    std::env::set_var("ECAA_CONFORMANCE_MODE", "1");

    let tmp = tempfile::tempdir().expect("create tempdir");
    let pkg = tmp.path();
    clone_fixture(pkg);

    // Remove a REQUIRED emit-time sidecar (subgraph E evidence). Its
    // absence is a hard `sidecar missing` failure (not a harness skip).
    std::fs::remove_file(pkg.join("runtime/proofs.jsonl")).expect("remove proofs.jsonl");

    let result = validate_emitted_package(pkg);
    assert!(
        result.is_err(),
        "ECAA_CONFORMANCE_MODE=1 with a missing required sidecar must block emit; got {:?}",
        result
            .as_ref()
            .map(|s| (s.schema_validation.passed, s.schema_validation.failed.len()))
    );

    clear_validation_env();
}

#[test]
#[serial]
fn conformance_mode_does_not_block_a_clean_package() {
    clear_validation_env();
    std::env::set_var("ECAA_CONFORMANCE_MODE", "1");

    // The clean fixture has all required emit-time sidecars and schema-valid
    // present harness-runtime sidecars, so conformance mode must NOT block.
    let pkg = fixture_path();
    let summary =
        validate_emitted_package(&pkg).expect("clean package must pass under conformance mode");
    assert!(
        summary.schema_validation.failed.is_empty(),
        "clean package must have zero schema failures; got {:?}",
        summary.schema_validation.failed
    );

    clear_validation_env();
}

#[test]
#[serial]
fn conformance_mode_upgrades_disabled_to_schema_only() {
    clear_validation_env();
    // Request Disabled, but conformance mode must upgrade it to SchemaOnly:
    // a conformant build never skips validation entirely.
    std::env::set_var("ECAA_VALIDATE_ON_EMIT", "off");
    std::env::set_var("ECAA_CONFORMANCE_MODE", "1");

    let pkg = fixture_path();
    let summary = validate_emitted_package(&pkg).expect("clean package must pass");
    assert_eq!(
        summary.mode,
        ValidationMode::SchemaOnly,
        "ECAA_CONFORMANCE_MODE must upgrade Disabled -> SchemaOnly"
    );
    assert!(
        summary.schema_validation.passed > 0,
        "validation must actually run under conformance mode; got passed={}",
        summary.schema_validation.passed
    );

    clear_validation_env();
}
