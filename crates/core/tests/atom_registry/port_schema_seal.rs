//! P3 — sealed inputs/outputs port-item schema (additionalProperties:false).
//! Regression: every real atom still loads. Negative: a typo'd facet
//! key on a port is rejected.

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

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atoms_p3_bad")
}

#[test]
fn all_real_atoms_still_load_under_sealed_ports() {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms())
        .expect("sealing port-item schema must not break any real atom");
    assert!(
        reg.len() >= 90,
        "expected the full atom catalog, got {}",
        reg.len()
    );
}

#[test]
fn typoed_facet_key_on_a_port_is_rejected() {
    let err = AtomRegistry::load_from_dir(&fixtures_dir())
        .expect_err("a typo'd facet key on a port must be rejected by the sealed schema");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("failed schema validation"),
        "expected schema-validation failure, got: {msg}"
    );
}
