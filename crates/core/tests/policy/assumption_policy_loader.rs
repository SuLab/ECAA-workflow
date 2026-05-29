use ecaa_workflow_core::assumption_policy::*;

#[test]
fn loads_canonical_table() {
    let table =
        AssumptionPolicyTable::load_from_path("../../config/assumption-policy.yaml").unwrap();
    assert_eq!(table.version, "1.0.0");
    assert!(table.is_blocking(DefectClass::PrivacyViolation, PolicyPrivacyClass::Phi));
    assert!(!table.is_blocking(
        DefectClass::StrandednessUnknown,
        PolicyPrivacyClass::Research
    ));
}

#[test]
fn missing_file_errors() {
    let r = AssumptionPolicyTable::load_from_path("nope.yaml");
    assert!(matches!(r, Err(AssumptionPolicyError::NotFound(_))));
}
