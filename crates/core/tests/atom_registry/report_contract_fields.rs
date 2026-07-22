//! Task C — reporting-contract fields declared on `reporting` +
//! `final_reporting` must actually load through `AtomRegistry` as typed
//! `AtomDefinition` fields (not silently dropped at deserialization).
//! Sibling to `result_schema_declared.rs`, which covers the
//! result-schema-bearing terminal analytical atoms.

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
fn reporting_atom_declares_report_contract() {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms()).expect("registry loads");
    let reporting = reg.get("reporting").expect("reporting atom present");
    assert!(
        reporting.interpretation_exempt_from_word_budget,
        "reporting must be exempt from the narrative word-budget cap"
    );
    assert!(
        !reporting.required_report_sections.is_empty(),
        "reporting must declare required_report_sections"
    );
    assert!(
        reporting
            .required_report_sections
            .iter()
            .any(|s| s == "primary_results"),
        "reporting's required_report_sections must include primary_results; got {:?}",
        reporting.required_report_sections
    );
    assert!(
        !reporting.required_tables.is_empty(),
        "reporting must declare required_tables"
    );
}

#[test]
fn final_reporting_atom_declares_report_contract() {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms()).expect("registry loads");
    let final_reporting = reg
        .get("final_reporting")
        .expect("final_reporting atom present");
    assert!(
        final_reporting.interpretation_exempt_from_word_budget,
        "final_reporting must be exempt from the narrative word-budget cap"
    );
    assert!(
        !final_reporting.required_report_sections.is_empty(),
        "final_reporting must declare required_report_sections"
    );
    assert!(
        final_reporting
            .required_report_sections
            .iter()
            .any(|s| s == "primary_results"),
        "final_reporting's required_report_sections must include primary_results; got {:?}",
        final_reporting.required_report_sections
    );
    assert!(
        !final_reporting.required_tables.is_empty(),
        "final_reporting must declare required_tables"
    );
}
