//! `differential_expression` accepts EITHER a raw or a normalized count
//! matrix as a method-neutral one-of (some tools — DESeq2/edgeR — fit a
//! count-GLM on raw counts; rank-based tools consume normalized counts).
//! The atom declares both ports plus an `input_groups: counts` one-of
//! constraint over them (`min_bound: 1`) rather than the compiler picking
//! a method by only wiring one substrate.

use ecaa_workflow_core::atom::InputGroupKind;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::workflow_contracts::port::Cardinality;
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
fn de_atom_declares_raw_and_normalized_one_of() {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms())
        .expect("registry must load with the differential_expression atom present");
    let de = reg
        .get("differential_expression")
        .expect("differential_expression atom must be in the catalog");

    let names: Vec<&str> = de.inputs.iter().map(|p| p.name.as_str()).collect();
    assert!(
        names.contains(&"raw_counts") && names.contains(&"normalized_counts"),
        "differential_expression must declare both raw_counts and normalized_counts inputs; inputs={names:?}"
    );

    let group = de
        .input_groups
        .iter()
        .find(|g| g.name == "counts")
        .expect("differential_expression must declare a `counts` input_group");
    assert_eq!(
        group.members.len(),
        2,
        "the `counts` group must cover exactly the two count-matrix ports; members={:?}",
        group.members
    );
    assert_eq!(group.kind, InputGroupKind::OneOf);
    assert_eq!(group.min_bound, 1);

    let raw = de
        .inputs
        .iter()
        .find(|p| p.name == "raw_counts")
        .expect("raw_counts port must be present");
    assert_eq!(raw.statistical_state.as_deref(), Some("raw_counts"));

    let normalized = de
        .inputs
        .iter()
        .find(|p| p.name == "normalized_counts")
        .expect("normalized_counts port must be present");
    assert_eq!(normalized.statistical_state.as_deref(), Some("normalized"));

    // Both count ports are individually optional — the group (not the
    // per-port cardinality) enforces "at least one must bind".
    assert!(matches!(raw.cardinality, Cardinality::Optional));
    assert!(matches!(normalized.cardinality, Cardinality::Optional));

    // experimental_design stays a required, unchanged third input.
    assert!(
        names.contains(&"experimental_design"),
        "experimental_design input must be kept unchanged; inputs={names:?}"
    );
}
