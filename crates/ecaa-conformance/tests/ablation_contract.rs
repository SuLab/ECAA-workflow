use scripps_workflow_core::ablation::AblationFlag;

#[test]
fn ablation_contract_one_flag_one_sidecar() {
    // This documents the contract; the actual emission-suppression
    // tests live in tier_21_production_emit.rs and tier_22_ecaa_mode.rs.
    let map: &[(AblationFlag, &str)] = &[
        (AblationFlag::DecisionRecords, "runtime/decisions.jsonl"),
        (
            AblationFlag::ClaimConsistency,
            "runtime/claim-verification.json",
        ),
        (
            AblationFlag::ReexecutionClass,
            "runtime/determinism-shim.json[ablation_engaged]",
        ),
        (AblationFlag::AuditProof, "runtime/audit-proof-report.json"),
        // AmendmentProvenance and TypedBlockers gate inline fields rather
        // than dedicated sidecars; their contract is verified in tier_21.
    ];
    assert!(!map.is_empty());
}
