//! Counts-first intake: `data_acquisition` offers a typed raw count
//! matrix output alongside the read-first `raw_reads` output. Optional
//! cardinality — counts-first runs provide it, read-first runs don't —
//! so no archetype is forced to consume it.

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
fn data_acquisition_offers_typed_raw_counts_output() {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms())
        .expect("registry must load with the data_acquisition atom present");
    let da = reg
        .get("data_acquisition")
        .expect("data_acquisition atom must be in the catalog");

    let rc = da
        .outputs
        .iter()
        .find(|p| p.name == "raw_count_matrix")
        .expect("data_acquisition must offer a typed raw count matrix output");

    assert_eq!(rc.statistical_state.as_deref(), Some("raw_counts"));
    assert_eq!(
        rc.physical_format.as_ref().map(|f| f.iri.as_str()),
        Some("format:3475")
    );
}
