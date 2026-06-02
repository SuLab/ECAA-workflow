//! Loader reads + verifies the signed verdict sink; tamper sets the flag.
use ecaa_workflow_core::audit_proof::loader::LoadedPackage;
use ecaa_workflow_core::audit_writer::AuditWriter;
use ecaa_workflow_core::claim_contract::ClaimContract;
use ecaa_workflow_core::claim_extractor::Claim;
use ecaa_workflow_core::claim_sink::persist_signed_verdicts;
use ecaa_workflow_core::claim_verifier::{
    ClaimStatus, ClaimStrength, ClaimVerdict, ClaimVerificationReport,
};

fn report() -> ClaimVerificationReport {
    let c = Claim {
        entity: "TP53".into(),
        direction: None,
        effect_size: None,
        pvalue: None,
        source_table: Some("results/tables/de.csv".into()),
        excerpt: String::new(),
        contract: ClaimContract::NumericTableLookup,
    };
    ClaimVerificationReport {
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
    }
}

#[test]
fn valid_signed_sink_populates_claims() {
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    persist_signed_verdicts(dir.path(), "diff_expr", &report(), &w).unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    assert!(!pkg.claims_tampered);
    let verdicts = pkg
        .claims
        .unwrap()
        .get("verdicts")
        .unwrap()
        .as_array()
        .unwrap()
        .len();
    assert_eq!(
        verdicts, 1,
        "signed sink verdicts must populate, not the empty stub"
    );
}

#[test]
fn tampered_sink_sets_flag() {
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    let path = persist_signed_verdicts(dir.path(), "diff_expr", &report(), &w).unwrap();
    // Tamper: flip a byte in the verdicts payload after signing.
    let line = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, line.replace("verified", "pending")).unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    assert!(
        pkg.claims_tampered,
        "HMAC mismatch must set claims_tampered"
    );
}

#[test]
fn absent_sink_falls_back_to_stub() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
    std::fs::write(
        dir.path().join("runtime/claim-verification.json"),
        r#"{"schema_version":"1","n_checked":0,"verdicts":[]}"#,
    )
    .unwrap();
    let w = AuditWriter::for_session();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    assert!(!pkg.claims_tampered);
    assert_eq!(
        pkg.claims
            .unwrap()
            .get("verdicts")
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
}
