//! run_audit_proof_with_verifier reads the signed sink → claim_completeness
//! is non-vacuous in the emitted report.
use ecaa_workflow_core::audit_proof::run_audit_proof_with_verifier;
use ecaa_workflow_core::audit_writer::AuditWriter;
use ecaa_workflow_core::claim_contract::ClaimContract;
use ecaa_workflow_core::claim_extractor::Claim;
use ecaa_workflow_core::claim_sink::persist_signed_verdicts;
use ecaa_workflow_core::claim_verifier::{
    ClaimStatus, ClaimStrength, ClaimVerdict, ClaimVerificationReport,
};
use ecaa_workflow_types::invariants::{InvariantId, InvariantStatus};

#[test]
fn report_claim_completeness_is_non_vacuous_with_sink() {
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    let c = Claim {
        entity: "TP53".into(),
        direction: None,
        effect_size: None,
        pvalue: None,
        source_table: Some("results/tables/de.csv".into()),
        excerpt: String::new(),
        contract: ClaimContract::NumericTableLookup,
        literature_evidence: None,
    };
    let rep = ClaimVerificationReport {
        n_checked: 1,
        n_verified: 1,
        n_mismatch: 0,
        n_unverifiable: 0,
        verdicts: vec![ClaimVerdict {
            claim: c,
            status: ClaimStatus::Verified,
            strength: ClaimStrength::default(),
        }],
        runtime_decision_log_path: None,
    };
    persist_signed_verdicts(dir.path(), "diff_expr", &rep, None, &w).unwrap();

    // Use the project's existing no-op WRROC validator + WallClock.
    let validator = ecaa_workflow_core::wrroc_validator::NoopWrrocValidator;
    let clock = ecaa_workflow_core::clock::WallClock;
    let report = run_audit_proof_with_verifier(dir.path(), &validator, &clock, Some(&w)).unwrap();

    let cc = report
        .verdicts
        .iter()
        .find(|v| v.id == InvariantId::ClaimCompleteness)
        .unwrap();
    assert_eq!(
        cc.n_inspected, 1,
        "claim_completeness must inspect the signed verdict"
    );
    assert_eq!(cc.status, InvariantStatus::Pass);
}
