//! Subfield-catalog-count drift gate.
//!
//! `config/ensemble-subfields/*.yaml` is the `SubfieldCatalog` source of
//! truth (the `_*.json` schema sidecar and the nested `personas/`
//! directory are excluded by the loader). This test couples the live
//! catalog size to a single in-repo constant, catching drift in either
//! direction:
//!
//! - Adding a subfield manifest without bumping the constant ⇒ fails.
//! - Bumping the constant without adding a manifest ⇒ fails.
//!
//! Mirrors `crates/core/tests/modality_count_baseline.rs`, but counts via
//! the REAL `SubfieldCatalog::load_from_dir` loader rather than a raw
//! directory glob, so the gate exercises the same exclusion rules
//! (`_*` sidecars, stem==id, schema validation) the runtime uses.
//!
//! GUARDS SUBFIELD-CATALOG-SIZE DRIFT: bump `EXPECTED_SUBFIELDS`
//! INTENTIONALLY in the same change that adds or removes a subfield
//! manifest under `config/ensemble-subfields/`. Do not "fix" a failure by
//! blindly editing the number — a mismatch means the catalog changed and
//! that change must be deliberate.

use ecaa_workflow_core::ensemble_subfield::SubfieldCatalog;
use std::path::{Path, PathBuf};

/// Expected number of subfields loaded from `config/ensemble-subfields/`.
/// Bump this only when a subfield manifest is intentionally added or
/// removed.
const EXPECTED_SUBFIELDS: usize = 18;

fn config_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

#[test]
fn ensemble_subfield_count_matches_baseline() {
    let catalog = SubfieldCatalog::load_from_dir(&config_root().join("ensemble-subfields"))
        .expect("SubfieldCatalog::load_from_dir must succeed for config/ensemble-subfields/");
    let actual = catalog.len();
    assert_eq!(
        actual, EXPECTED_SUBFIELDS,
        "SubfieldCatalog loaded {actual} subfields but expected \
         {EXPECTED_SUBFIELDS}. This gate guards subfield-catalog-size \
         drift: if you intentionally added or removed a subfield manifest \
         under config/ensemble-subfields/, bump `EXPECTED_SUBFIELDS` in \
         this test in the same change."
    );
}
