//! Archetype-count drift gate.
//!
//! `config/archetypes/*.yaml` is the `ArchetypeRegistry` source of truth.
//! The loader excludes `_*.yaml` schema sidecars and `*.slots.yaml`
//! companions: a `<id>.slots.yaml` is folded onto its parent archetype as
//! a `SlotManifest`, NOT counted as its own archetype. CLAUDE.md cites
//! integer counts for both the composer-native archetypes and the slot
//! companions, both of which have rotted between releases; coupling the
//! live registry size to in-repo constants catches drift either way:
//!
//! - Adding an archetype YAML without bumping the constant ⇒ fails.
//! - Bumping a constant without adding/removing a YAML ⇒ fails.
//!
//! Mirrors `crates/core/tests/atom_registry/atom_count_baseline.rs`, but
//! counts via the REAL `ArchetypeRegistry::load_from_dir` loader rather
//! than a raw directory glob, so the gate exercises the same exclusion +
//! slot-folding rules the runtime composer uses.
//!
//! GUARDS ARCHETYPE-REGISTRY-SIZE DRIFT: bump the relevant constant
//! INTENTIONALLY in the same change that adds or removes an archetype YAML
//! (or a `.slots.yaml` companion) under `config/archetypes/`. Do not "fix"
//! a failure by blindly editing the number — a mismatch means the registry
//! changed and that change must be deliberate.

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use std::path::{Path, PathBuf};

/// Expected number of composer-native archetypes loaded from
/// `config/archetypes/` (the loader already excludes `_*` schema sidecars
/// and `*.slots.yaml` companions). Bump only when an archetype is
/// intentionally added or removed.
const EXPECTED_ARCHETYPES: usize = 30;

/// Expected number of archetypes carrying a `<id>.slots.yaml` companion
/// (folded onto the parent as a `SlotManifest` by the loader). Bump only
/// when a `.slots.yaml` companion is intentionally added or removed.
const EXPECTED_ARCHETYPE_SLOTS: usize = 3;

fn config_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

fn load_registry() -> ArchetypeRegistry {
    ArchetypeRegistry::load_from_dir(&config_root().join("archetypes"))
        .expect("ArchetypeRegistry::load_from_dir must succeed for config/archetypes/")
}

#[test]
fn archetype_count_matches_baseline() {
    let reg = load_registry();
    let actual = reg.len();
    assert_eq!(
        actual, EXPECTED_ARCHETYPES,
        "ArchetypeRegistry loaded {actual} archetypes but expected \
         {EXPECTED_ARCHETYPES}. This gate guards archetype-registry-size \
         drift: if you intentionally added or removed an archetype YAML under \
         config/archetypes/, bump `EXPECTED_ARCHETYPES` in this test in the \
         same change."
    );
}

#[test]
fn archetype_slot_companion_count_matches_baseline() {
    let reg = load_registry();
    let actual = reg.iter().filter(|(_, a)| a.slots.is_some()).count();
    assert_eq!(
        actual, EXPECTED_ARCHETYPE_SLOTS,
        "{actual} archetypes carry a .slots.yaml companion but expected \
         {EXPECTED_ARCHETYPE_SLOTS}. This gate guards slot-companion drift: \
         if you intentionally added or removed a `<id>.slots.yaml` companion \
         under config/archetypes/, bump `EXPECTED_ARCHETYPE_SLOTS` in this \
         test in the same change."
    );
}
