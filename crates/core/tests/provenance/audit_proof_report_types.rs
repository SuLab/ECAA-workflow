use ecaa_workflow_core::audit_proof::{
    AuditProofReport, InvariantId, InvariantStatus, InvariantVerdict,
};

#[test]
fn report_has_six_invariants_in_canonical_order() {
    let report = AuditProofReport::empty();
    let ids: Vec<InvariantId> = report.verdicts.iter().map(|v| v.id).collect();
    assert_eq!(
        ids,
        vec![
            InvariantId::ClaimCompleteness,
            InvariantId::DecisionJustification,
            InvariantId::EvidenceCoverage,
            InvariantId::EquivalenceFailure,
            InvariantId::CrossGraphIntegrity,
            InvariantId::SubstrateValidity,
        ]
    );
}

#[test]
fn empty_report_serializes_deterministically() {
    let report = AuditProofReport::empty();
    let json = serde_json::to_string_pretty(&report).unwrap();
    // No timestamps; no random IDs; six verdict slots
    assert!(json.contains("\"claim_completeness\""));
    assert!(json.contains("\"unverified\""));
}

#[allow(dead_code)]
fn _verdict_constructible() -> InvariantVerdict {
    InvariantVerdict {
        id: InvariantId::ClaimCompleteness,
        status: InvariantStatus::Pass,
        detail: None,
        n_inspected: 0,
        n_violations: 0,
    }
}
