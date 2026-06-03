//! P1 — typed `parameters: Vec<ParameterSpec>` on AtomDefinition.
//! Round-trip + schema-negative coverage.

use ecaa_workflow_core::atom::{AtomDefinition, ParameterSpec, ParameterType};
use ecaa_workflow_core::atom_registry::AtomRegistry;
use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/atoms_p1")
}

#[test]
fn parameter_spec_round_trips_through_json() {
    let mut atom = AtomDefinition::test_default("typed_param_atom");
    atom.parameters = vec![ParameterSpec {
        name: "candidate_tools".into(),
        r#type: ParameterType::Enum,
        required: true,
        default: None,
        allowed_values: vec![serde_json::json!("star"), serde_json::json!("hisat2")],
        examples: vec![serde_json::json!("star")],
        description: Some("Aligner candidate set".into()),
    }];
    let json = serde_json::to_string(&atom).expect("serialize");
    let back: AtomDefinition = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.parameters.len(), 1);
    assert_eq!(back.parameters[0].name, "candidate_tools");
    assert_eq!(back.parameters[0].r#type, ParameterType::Enum);
    assert!(back.parameters[0].required);
}

#[test]
fn atom_with_parameters_block_loads_from_registry() {
    let reg = AtomRegistry::load_from_dir(&fixtures_dir())
        .expect("fixture atom with parameters: block must load");
    let atom = reg.get("typed_param_atom").expect("fixture atom present");
    assert_eq!(atom.parameters.len(), 1);
    assert_eq!(atom.parameters[0].name, "candidate_tools");
}

#[test]
fn unknown_key_inside_parameter_entry_is_rejected_by_schema() {
    // The bad fixture carries an unknown `unexpected:` key inside a
    // parameter entry.
    let bad = fixtures_dir().join("..").join("atoms_p1_bad");
    let err = AtomRegistry::load_from_dir(&bad)
        .expect_err("schema must reject an unknown key inside a parameter entry");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("failed schema validation"),
        "expected schema-validation failure, got: {msg}"
    );
}
