//! With a signed sink present, Inv 1 inspects real verdicts (non-vacuous)
//! and maps tamper to Fail.
use ecaa_workflow_core::audit_proof::invariants::claim_completeness::check_claim_completeness;
use ecaa_workflow_core::audit_proof::loader::LoadedPackage;
use ecaa_workflow_core::audit_writer::AuditWriter;
use ecaa_workflow_core::claim_contract::ClaimContract;
use ecaa_workflow_core::claim_extractor::Claim;
use ecaa_workflow_core::claim_sink::persist_signed_verdicts;
use ecaa_workflow_core::claim_verifier::{
    ClaimStatus, ClaimStrength, ClaimVerdict, ClaimVerificationReport,
};
use ecaa_workflow_types::invariants::InvariantStatus;

fn claim(entity: &str, table: Option<&str>) -> Claim {
    Claim {
        entity: entity.into(),
        direction: None,
        effect_size: None,
        pvalue: None,
        source_table: table.map(String::from),
        excerpt: String::new(),
        contract: ClaimContract::NumericTableLookup,
        literature_evidence: None,
        matched_pvalue_keyword: None,
        linear_fold: None,
    }
}
fn v(c: Claim, s: ClaimStatus) -> ClaimVerdict {
    ClaimVerdict {
        claim: c,
        status: s,
        strength: ClaimStrength::default(),
    }
}

#[test]
fn verified_claim_is_inspected_and_passes() {
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    let rep = ClaimVerificationReport {
        n_checked: 1,
        n_verified: 1,
        n_mismatch: 0,
        n_unverifiable: 0,
        n_suspicious: 0,
        verdicts: vec![v(
            claim("TP53", Some("results/tables/de.csv")),
            ClaimStatus::Verified,
        )],
        runtime_decision_log_path: None,
    };
    persist_signed_verdicts(dir.path(), "diff_expr", &rep, None, &w).unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    let verdict = check_claim_completeness(&pkg);
    assert_eq!(verdict.status, InvariantStatus::Pass);
    assert_eq!(
        verdict.n_inspected, 1,
        "must be NON-VACUOUS — was always 0 before the sink"
    );
}

#[test]
fn mismatch_claim_is_a_violation() {
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    let rep = ClaimVerificationReport {
        n_checked: 1,
        n_verified: 0,
        n_mismatch: 1,
        n_unverifiable: 0,
        n_suspicious: 0,
        verdicts: vec![v(
            claim("IL6", Some("results/tables/de.csv")),
            ClaimStatus::Mismatch {
                detail: "sign flip".into(),
            },
        )],
        runtime_decision_log_path: None,
    };
    persist_signed_verdicts(dir.path(), "diff_expr", &rep, None, &w).unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    let verdict = check_claim_completeness(&pkg);
    assert_eq!(verdict.status, InvariantStatus::Warn);
    assert_eq!(verdict.n_violations, 1);
}

#[test]
fn tampered_sink_fails_inv1() {
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    let rep = ClaimVerificationReport {
        n_checked: 1,
        n_verified: 1,
        n_mismatch: 0,
        n_unverifiable: 0,
        n_suspicious: 0,
        verdicts: vec![v(
            claim("TP53", Some("results/tables/de.csv")),
            ClaimStatus::Verified,
        )],
        runtime_decision_log_path: None,
    };
    let path = persist_signed_verdicts(dir.path(), "diff_expr", &rep, None, &w).unwrap();
    // Tamper a value that is actually present in the signed payload (the
    // status). The projected sink carries {claim_id, status, supported_by}
    // and the counts — not the original claim entity — so flipping the
    // verdict status is what an attacker would forge, and it breaks the HMAC.
    let line = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, line.replace("verified", "pending")).unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    let verdict = check_claim_completeness(&pkg);
    assert_eq!(
        verdict.status,
        InvariantStatus::Fail,
        "tamper must FAIL, not silently pass"
    );
}
