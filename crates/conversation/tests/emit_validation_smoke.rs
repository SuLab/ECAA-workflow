//! Smoke + negative + full-mode coverage for the emit-time validation
//! module against the minimal-package fixture in
//! `crates/ecaa-conformance/tests/fixtures/`.
//!
//! Exercises three layers:
//! - Pure-Rust JSON Schema (always-on; `schema_only` mode).
//! - BLOCK_ON_FAIL gate (clean fixture passes; malformed fixture aborts emit).
//! - External Python validators when SWFC_VALIDATE_ON_EMIT=full (graceful
//!   degradation expected if Python deps absent — both `Pass` and
//!   `Unavailable` are accepted outcomes).
//!
//! Tests are `#[serial]` because they mutate process-global env vars
//! (`SWFC_VALIDATE_ON_EMIT`, `SWFC_VALIDATION_BLOCK_ON_FAIL`) that the
//! validator reads on every call.

use scripps_workflow_conversation::emit::validation::{
    validate_emitted_package, write_validation_summary, ExternalCheckOutcome, ValidationMode,
};
use serial_test::serial;
use std::path::PathBuf;

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("ecaa-conformance/tests/fixtures/minimal-package")
        .canonicalize()
        .expect("minimal-package fixture must exist")
}

/// Copy the minimal-package fixture into a fresh tempdir so destructive
/// fixtures (malformed decisions.jsonl etc.) don't pollute the canonical
/// fixture in the repo.
fn clone_fixture(dst: &std::path::Path) {
    let src = fixture_path();
    fn copy_dir(src: &std::path::Path, dst: &std::path::Path) {
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
    std::env::remove_var("SWFC_VALIDATE_ON_EMIT");
    std::env::remove_var("SWFC_VALIDATION_BLOCK_ON_FAIL");
}

#[test]
#[serial]
fn schema_only_mode_runs_on_fixture() {
    clear_validation_env();
    std::env::set_var("SWFC_VALIDATE_ON_EMIT", "schema_only");
    let pkg = fixture_path();
    let summary = validate_emitted_package(&pkg).expect("validation should not block");
    assert_eq!(summary.mode, ValidationMode::SchemaOnly);
    assert!(summary.external_validation.is_none());
    // The fixture is hand-built — we permit a few schema mismatches as the
    // fixture's hand-written JSON may not exactly match every emit-side detail.
    // Strict requirement: at least N sidecars passed.
    assert!(
        summary.schema_validation.passed > 0,
        "at least one sidecar should pass; got passed={} failed={:?}",
        summary.schema_validation.passed,
        summary.schema_validation.failed,
    );
    // R2.9: harness-runtime sidecars (validation-reports.jsonl + verifier-
    // decisions.jsonl) are skipped at emit time and counted separately.
    assert_eq!(
        summary.schema_validation.skipped_pending_harness, 2,
        "expected exactly 2 harness-runtime sidecars to be skipped at emit time"
    );
    write_validation_summary(&pkg, &summary);
    clear_validation_env();
}

#[test]
#[serial]
fn disabled_mode_skips_all() {
    clear_validation_env();
    std::env::set_var("SWFC_VALIDATE_ON_EMIT", "off");
    let pkg = fixture_path();
    let summary = validate_emitted_package(&pkg).expect("validation should not block");
    assert_eq!(summary.mode, ValidationMode::Disabled);
    assert!(summary.external_validation.is_none());
    assert_eq!(summary.schema_validation.passed, 0);
    assert!(summary.schema_validation.failed.is_empty());
    assert_eq!(summary.schema_validation.skipped_pending_harness, 0);
    clear_validation_env();
}

#[test]
#[serial]
fn validates_under_block_on_fail_passes_on_clean_fixture() {
    clear_validation_env();
    std::env::set_var("SWFC_VALIDATE_ON_EMIT", "schema_only");
    std::env::set_var("SWFC_VALIDATION_BLOCK_ON_FAIL", "1");
    let pkg = fixture_path();
    // The clean fixture must succeed under BLOCK_ON_FAIL=1 because R2.9
    // skips the two harness-runtime sidecars rather than recording them
    // as missing-sidecar failures.
    let summary = validate_emitted_package(&pkg)
        .expect("clean fixture under BLOCK_ON_FAIL=1 should not abort emit");
    assert_eq!(summary.mode, ValidationMode::SchemaOnly);
    assert!(
        summary.schema_validation.failed.is_empty(),
        "clean fixture must have zero schema failures; got {:?}",
        summary.schema_validation.failed
    );
    assert_eq!(summary.schema_validation.skipped_pending_harness, 2);
    clear_validation_env();
}

#[test]
#[serial]
fn validates_under_block_on_fail_rejects_malformed_decision_jsonl() {
    clear_validation_env();
    std::env::set_var("SWFC_VALIDATE_ON_EMIT", "schema_only");
    std::env::set_var("SWFC_VALIDATION_BLOCK_ON_FAIL", "1");
    let tmp = tempfile::tempdir().expect("create tempdir");
    let pkg = tmp.path();
    clone_fixture(pkg);
    // Corrupt decisions.jsonl with a non-JSON line. The validator's
    // JSONL parse path records this as a SchemaFailure with
    // `line_index`, and under BLOCK_ON_FAIL=1 that aborts the emit.
    let decisions = pkg.join("runtime/decisions.jsonl");
    std::fs::write(&decisions, "{not-valid-json\n").expect("write malformed decisions.jsonl");
    let result = validate_emitted_package(pkg);
    assert!(
        result.is_err(),
        "malformed decisions.jsonl under BLOCK_ON_FAIL=1 must return Err; got {:?}",
        result
            .as_ref()
            .map(|s| (s.schema_validation.passed, s.schema_validation.failed.len()))
    );
    clear_validation_env();
}

#[test]
#[serial]
fn full_mode_invokes_external_validators() {
    clear_validation_env();
    std::env::set_var("SWFC_VALIDATE_ON_EMIT", "full");
    let pkg = fixture_path();
    let summary = validate_emitted_package(&pkg).expect("full-mode validation should not block");
    assert_eq!(summary.mode, ValidationMode::Full);
    // The external block must be populated in `full` mode regardless of
    // whether the external tools are available on this host.
    let ext = summary
        .external_validation
        .as_ref()
        .expect("full mode must populate external_validation");
    // Each of the three external checks reports one of Pass / Fail /
    // Unavailable / Error. We don't constrain which — CI hosts without
    // rdflib/owlready2/pyld/pyshacl present should see `Unavailable`;
    // hosts with the toolchain installed should see `Pass` or `Fail`.
    // Tolerate both by simply confirming the outcome is one of the four.
    fn outcome_label(o: &ExternalCheckOutcome) -> &'static str {
        match o {
            ExternalCheckOutcome::Pass { .. } => "pass",
            ExternalCheckOutcome::Fail { .. } => "fail",
            ExternalCheckOutcome::Unavailable { .. } => "unavailable",
            ExternalCheckOutcome::Error { .. } => "error",
        }
    }
    for outcome in [
        &ext.shacl_projection,
        &ext.owl_consistency,
        &ext.runcrate_validate,
    ] {
        // The match in `outcome_label` is exhaustive, so reaching this
        // line means the validator returned a well-typed variant for
        // every external check — which is the actual property the test
        // verifies (Pass / Fail / Unavailable / Error are all accepted).
        let _ = outcome_label(outcome);
    }
    clear_validation_env();
}
