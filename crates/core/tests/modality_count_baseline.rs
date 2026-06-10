//! Modality-count drift gate.
//!
//! `config/modalities/*.yaml` is the `ModalityRegistry` source of truth
//! (the `_*.json` / `_*.yaml` schema sidecars are excluded by the loader).
//! CLAUDE.md cites an integer count of keyword-routable modalities that
//! has rotted between releases; coupling the live registry size to a
//! single in-repo constant catches drift in either direction:
//!
//! - Adding a modality manifest without bumping the constant ⇒ fails.
//! - Bumping the constant without adding a manifest ⇒ fails.
//!
//! Mirrors `crates/core/tests/atom_registry/atom_count_baseline.rs`, but
//! counts via the REAL `ModalityRegistry::load_from_dir` loader rather
//! than a raw directory glob, so the gate exercises the same exclusion
//! rules (`_*` sidecars, stem==id, schema validation) the runtime uses.
//!
//! GUARDS MODALITY-REGISTRY-SIZE DRIFT: bump `EXPECTED_MODALITIES`
//! INTENTIONALLY in the same change that adds or removes a modality
//! manifest under `config/modalities/`. Do not "fix" a failure by blindly
//! editing the number — a mismatch means the registry changed and that
//! change must be deliberate.

use ecaa_workflow_core::modality_registry::ModalityRegistry;
use std::path::{Path, PathBuf};

/// Expected number of modalities loaded from `config/modalities/`. Bump
/// this only when a modality manifest is intentionally added or removed.
const EXPECTED_MODALITIES: usize = 23;

fn config_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

#[test]
fn modality_count_matches_baseline() {
    let reg = ModalityRegistry::load_from_dir(&config_root().join("modalities"))
        .expect("ModalityRegistry::load_from_dir must succeed for config/modalities/");
    let actual = reg.len();
    assert_eq!(
        actual, EXPECTED_MODALITIES,
        "ModalityRegistry loaded {actual} modalities but expected \
         {EXPECTED_MODALITIES}. This gate guards modality-registry-size drift: \
         if you intentionally added or removed a modality manifest under \
         config/modalities/, bump `EXPECTED_MODALITIES` in this test in the \
         same change."
    );
}
