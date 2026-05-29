//! v3 P7 — integration tests for [`replay_provenance`].
//!
//! Fixtures live under `tests/wrroc-fixtures/`. Each fixture is a
//! minimal WRROC shell (a `runtime/` directory carrying the IR
//! artifacts + manifest) keyed by the version state it represents:
//!
//! - `v0-u32-legacy` — manifest with all `0.0.0` versions; replay
//! against the current manifest must pass because the starter
//! migrator chain (`MigrationRegistry::with_starters()`) covers
//! `0.0.0 → 1.0.0` for every IR type.
//! - `v1-semver-current` — manifest with all `1.0.0` versions; replay
//! against current is a trivial pass.

use ecaa_workflow_core::migration::{
    replay_provenance, MigrationRegistry, ReplayError, SchemaVersionsManifest,
};
use semver::Version;
use std::path::Path;

fn fixture(name: &str) -> std::path::PathBuf {
    // Resolve from the workspace root regardless of where the test
    // runs from (cargo runs integration tests with cwd = crate root
    // for the crate the test belongs to).
    let crate_dir = env!("CARGO_MANIFEST_DIR");
    Path::new(crate_dir)
        .ancestors()
        .nth(2)
        .expect("ancestor")
        .join("tests/wrroc-fixtures")
        .join(name)
}

#[test]
fn replay_v0_u32_legacy_migrates_cleanly() {
    let result = replay_provenance(
        &fixture("v0-u32-legacy"),
        &SchemaVersionsManifest::current(),
        &MigrationRegistry::with_starters(),
    );
    assert!(
        result.is_ok(),
        "v0-u32-legacy fixture must replay cleanly via the starter migrator chain, got {:?}",
        result.err()
    );
}

#[test]
fn replay_v1_semver_current_is_trivial_pass() {
    let result = replay_provenance(
        &fixture("v1-semver-current"),
        &SchemaVersionsManifest::current(),
        &MigrationRegistry::with_starters(),
    );
    assert!(
        result.is_ok(),
        "expected trivial pass, got {:?}",
        result.err()
    );
}

#[test]
fn replay_v_unknown_returns_migration_required() {
    // Construct a fake target manifest with a version no registered
    // migrator chain covers. The replay must surface
    // `MigrationRequired`, not panic.
    let mut manifest = SchemaVersionsManifest::current();
    manifest.workflow_intent = Version::new(99, 0, 0);
    let result = replay_provenance(
        &fixture("v1-semver-current"),
        &manifest,
        &MigrationRegistry::with_starters(),
    );
    assert!(
        matches!(result, Err(ReplayError::MigrationRequired { .. })),
        "expected MigrationRequired, got {:?}",
        result
    );
}

#[test]
fn replay_missing_sidecar_surfaces_typed_error() {
    let dir = tempfile::tempdir().unwrap();
    let result = replay_provenance(
        dir.path(),
        &SchemaVersionsManifest::current(),
        &MigrationRegistry::with_starters(),
    );
    assert!(matches!(result, Err(ReplayError::MissingSidecar { .. })));
}
