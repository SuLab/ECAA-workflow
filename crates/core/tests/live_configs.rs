//! Live-config regression net. Three assertions covering every YAML
//! file the runtime touches:
//!
//! 1. Every `<name>.yaml` under `config/stage-atoms/` loads as a valid
//! `AtomDefinition` via `AtomRegistry::load_from_dir`.
//! 2. Every `<name>.yaml` under `config/archetypes/` loads as a valid
//! `ArchetypeDefinition` via `ArchetypeRegistry::load_from_dir`.
//! 3. The two registries are non-empty.
//!
//! The sibling `taxonomy_validation::all_live_taxonomies_validate` test
//! covers `config/stage-taxonomies/` already. Together these three
//! tests fail loudly on any silent schema drift in the live config tree
//! — the failure mode that produced the "no DAG built"
//! incident, where a taxonomy YAML was committed missing a
//! schema-required field and the chat path silently fell through to
//! `emit_package` before tripping the precondition gate.

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use std::path::{Path, PathBuf};

fn config_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

#[test]
fn every_live_atom_loads() {
    let reg = AtomRegistry::load_from_dir(&config_root().join("stage-atoms"))
        .expect("AtomRegistry::load_from_dir must succeed for every file in config/stage-atoms/");
    assert!(
        !reg.is_empty(),
        "AtomRegistry loaded zero atoms — config/stage-atoms/ likely missing"
    );
}

#[test]
fn every_live_archetype_loads() {
    let reg = ArchetypeRegistry::load_from_dir(&config_root().join("archetypes")).expect(
        "ArchetypeRegistry::load_from_dir must succeed for every file in config/archetypes/",
    );
    assert!(
        !reg.is_empty(),
        "ArchetypeRegistry loaded zero archetypes — config/archetypes/ likely missing"
    );
}
