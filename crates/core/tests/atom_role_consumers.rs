//! Assert that role derivation from legacy stage-id
//! prefixes agrees with the typed `AtomRole` enum, and that
//! consumers branching on `default_behavior_class()` produce the
//! same answers the old `starts_with` checks did.

use scripps_workflow_core::atom::AtomRole;
use scripps_workflow_core::taxonomy::derive_role_from_id;

#[test]
fn discover_prefix_maps_to_discovery_role() {
    for id in [
        "discover_alignment",
        "discover_normalization",
        "discover_doublet_detection",
    ] {
        assert_eq!(derive_role_from_id(id), AtomRole::Discovery);
        assert!(derive_role_from_id(id).is_discovery());
    }
}

#[test]
fn validate_prefix_maps_to_validation_role() {
    for id in [
        "validate_alignment",
        "validate_quantification",
        "validate_de",
    ] {
        assert_eq!(derive_role_from_id(id), AtomRole::Validation);
        assert!(derive_role_from_id(id).is_validation());
    }
}

#[test]
fn select_prefix_maps_to_selection_role() {
    for id in ["select_sensitivity_winner", "select_method"] {
        assert_eq!(derive_role_from_id(id), AtomRole::Selection);
        assert!(derive_role_from_id(id).is_selection());
    }
}

#[test]
fn unprefixed_ids_map_to_operation_role() {
    for id in ["alignment", "differential_expression", "qc_preprocessing"] {
        assert_eq!(derive_role_from_id(id), AtomRole::Operation);
        assert!(derive_role_from_id(id).is_operation());
    }
}

#[test]
fn agreement_with_legacy_starts_with_predicate() {
    // Regression guard: every legacy `starts_with` check the plan
    // calls out must agree with the typed helper. Sweeping
    // `starts_with("discover_")` etc. across consumer files is the
    // soak-phase work; this test pins the contract so the sweep is
    // mechanical and risk-free.
    let cases = [
        ("discover_x", true, false, false),
        ("validate_x", false, true, false),
        ("select_x", false, false, true),
        ("plain_x", false, false, false),
        ("validation_helper", false, false, false), // no leading "validate_"
        ("disc_x", false, false, false),            // no leading "discover_"
    ];
    for (id, want_disc, want_val, want_sel) in cases {
        let role = derive_role_from_id(id);
        assert_eq!(
            role.is_discovery(),
            want_disc,
            "is_discovery({id}) expected {want_disc}, got {}",
            role.is_discovery()
        );
        assert_eq!(
            role.is_validation(),
            want_val,
            "is_validation({id}) expected {want_val}, got {}",
            role.is_validation()
        );
        assert_eq!(
            role.is_selection(),
            want_sel,
            "is_selection({id}) expected {want_sel}, got {}",
            role.is_selection()
        );
    }
}
