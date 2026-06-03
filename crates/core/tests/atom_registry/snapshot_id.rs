//! M4 (stub) — deterministic registry snapshot id. Phase 2 (W1) embeds
//! the returned value into the emitted workflow-typed.json so the typed
//! artifact is self-describing against the registry that produced it.

use ecaa_workflow_core::atom::AtomDefinition;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use std::path::{Path, PathBuf};

fn config_stage_atoms() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/stage-atoms")
}

#[test]
fn snapshot_id_is_deterministic_across_loads() {
    let a = AtomRegistry::load_from_dir(&config_stage_atoms()).unwrap();
    let b = AtomRegistry::load_from_dir(&config_stage_atoms()).unwrap();
    let id_a = a.snapshot_id();
    let id_b = b.snapshot_id();
    assert_eq!(id_a, id_b, "snapshot id must be stable across loads");
    assert!(id_a.starts_with("atomreg:sha256:"), "got: {id_a}");
    // 16 lowercase hex chars after the prefix.
    let hex = id_a.trim_start_matches("atomreg:sha256:");
    assert_eq!(hex.len(), 16, "got hex {hex:?}");
    assert!(hex
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

#[test]
fn snapshot_id_changes_when_an_atom_id_or_version_changes() {
    // Build two tiny registries that differ only in one atom's version.
    let base = AtomRegistry::from_atoms(vec![AtomDefinition::test_default("x")]);
    let mut bumped_atom = AtomDefinition::test_default("x");
    bumped_atom.version = "2.0.0".into();
    let bumped = AtomRegistry::from_atoms(vec![bumped_atom]);
    assert_ne!(
        base.snapshot_id(),
        bumped.snapshot_id(),
        "a version change must change the snapshot id"
    );
}
