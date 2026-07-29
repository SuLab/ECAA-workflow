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

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
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

#[test]
fn qc_preprocessing_separates_metrics_from_filtered_counts() {
    let reg = AtomRegistry::load_from_dir(&config_root().join("stage-atoms"))
        .expect("AtomRegistry must load the QC preprocessing contract");
    let qc = reg
        .get("qc_preprocessing")
        .expect("qc_preprocessing must be present");

    let metrics = qc
        .outputs
        .first()
        .expect("QC metrics must remain the primary plotting and validation output");
    assert_eq!(metrics.name, "qc_metrics");
    assert_eq!(metrics.semantic_type.stable_id(), "ecaax:qc_metrics");
    assert_eq!(
        metrics.physical_format.as_ref().map(|f| f.iri.as_str()),
        Some("format:3475")
    );

    let filtered = qc
        .outputs
        .iter()
        .find(|port| port.name == "filtered_count_matrix")
        .expect("QC must expose the filtered matrix to downstream normalization");
    assert_eq!(filtered.semantic_type.stable_id(), "data:3917");
    assert_eq!(
        filtered.physical_format.as_ref().map(|f| f.iri.as_str()),
        Some("format:3475")
    );
    assert_eq!(filtered.statistical_state.as_deref(), Some("raw_counts"));
    assert_eq!(filtered.normalization_state.as_deref(), Some("raw"));

    assert!(
        qc.description
            .contains("do not query a live annotation service"),
        "QC instructions must keep replay-critical annotation lookup inside the retained package"
    );
}
