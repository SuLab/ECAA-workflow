//! Speculative AtomRole variant round-trip + behavior-
//! class fallback.
//!
//! Validates that the five new variants (`Selection` + speculative
//! `Calibration`/`Pilot`/`Adversarial`/`Monitor`) deserialize from
//! YAML and correctly fall back to their behavior class so existing
//! consumers don't need to add new arms until a real specialization
//! appears.

use scripps_workflow_core::atom::{AtomDefinition, AtomRole};

fn try_parse_role(role_yaml: &str) -> Result<AtomDefinition, serde_yml::Error> {
    let yaml = format!(
        r#"
id: test_atom
version: "1.0.0"
role: {role_yaml}
description: Test atom for AtomRole speculative-variant round-trip.
edam_operation: "operation:0004"
assignee: agent
"#
    );
    serde_yml::from_str(&yaml)
}

#[test]
fn all_new_variants_deserialize() {
    for (yaml_name, expected) in [
        ("operation", AtomRole::Operation),
        ("discovery", AtomRole::Discovery),
        ("validation", AtomRole::Validation),
        ("aggregator", AtomRole::Aggregator),
        ("sizing", AtomRole::Sizing),
        ("selection", AtomRole::Selection),
        ("calibration", AtomRole::Calibration),
        ("pilot", AtomRole::Pilot),
        ("adversarial", AtomRole::Adversarial),
        ("monitor", AtomRole::Monitor),
    ] {
        let atom = try_parse_role(yaml_name);
        // Discovery requires discovery_kind; expect that one specific
        // round-trip to fail at the registry-load gate (not the serde
        // gate). Here we only assert serde-level deserialization.
        let atom = atom.unwrap_or_else(|e| panic!("role '{yaml_name}' must deserialize, got: {e}"));
        assert_eq!(
            atom.role, expected,
            "role '{yaml_name}' deserialized incorrectly"
        );
    }
}

#[test]
fn default_behavior_class_collapses_speculative_to_operation() {
    assert_eq!(
        AtomRole::Selection.default_behavior_class(),
        AtomRole::Selection,
        "Selection is load-bearing — must remain Selection"
    );
    assert_eq!(
        AtomRole::Discovery.default_behavior_class(),
        AtomRole::Discovery
    );
    assert_eq!(
        AtomRole::Validation.default_behavior_class(),
        AtomRole::Validation
    );
    assert_eq!(
        AtomRole::Aggregator.default_behavior_class(),
        AtomRole::Aggregator
    );
    // Speculative variants + Sizing collapse to Operation so existing
    // 5-arm match consumers don't need new arms.
    for spec in [
        AtomRole::Calibration,
        AtomRole::Pilot,
        AtomRole::Adversarial,
        AtomRole::Monitor,
        AtomRole::Sizing,
    ] {
        assert_eq!(
            spec.default_behavior_class(),
            AtomRole::Operation,
            "speculative role {spec:?} must collapse to Operation"
        );
    }
}

#[test]
fn role_predicates_align_with_behavior_class() {
    assert!(AtomRole::Discovery.is_discovery());
    assert!(!AtomRole::Operation.is_discovery());
    assert!(AtomRole::Validation.is_validation());
    assert!(AtomRole::Selection.is_selection());
    assert!(AtomRole::Aggregator.is_aggregator());
    // Speculative variants' predicate behavior.
    assert!(
        AtomRole::Pilot.is_operation(),
        "Pilot collapses to Operation"
    );
    assert!(AtomRole::Calibration.is_operation());
    assert!(!AtomRole::Calibration.is_discovery());
    assert!(!AtomRole::Pilot.is_validation());
}
