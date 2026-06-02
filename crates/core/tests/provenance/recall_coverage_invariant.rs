//! Inv 1 (claim_completeness) is recall-anchored: it fails when a signed
//! coverage block shows a Required entry not Addressed, and passes when
//! every Required entry is Addressed.
use ecaa_workflow_core::audit_proof::invariants::claim_completeness::check_claim_completeness;
use ecaa_workflow_core::audit_proof::loader::LoadedPackage;
use ecaa_workflow_core::audit_writer::AuditWriter;
use ecaa_workflow_core::claim_contract::ClaimContract;
use ecaa_workflow_core::claim_extractor::Claim;
use ecaa_workflow_core::claim_sink::persist_signed_verdicts;
use ecaa_workflow_core::claim_verifier::{
    ClaimStatus, ClaimStrength, ClaimVerdict, ClaimVerificationReport,
};
use ecaa_workflow_core::coverage::{CoverageResult, EntityCoverage};
use ecaa_workflow_types::invariants::InvariantStatus;
use std::collections::BTreeMap;

fn verdict(entity: &str, table: Option<&str>, status: ClaimStatus) -> ClaimVerdict {
    ClaimVerdict {
        claim: Claim {
            entity: entity.into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: table.map(String::from),
            excerpt: String::new(),
            contract: ClaimContract::NumericTableLookup,
        },
        status,
        strength: ClaimStrength::Exploratory,
    }
}

fn report_one_verified() -> ClaimVerificationReport {
    ClaimVerificationReport {
        n_checked: 1,
        n_verified: 1,
        n_mismatch: 0,
        n_unverifiable: 0,
        verdicts: vec![verdict(
            "differential_expression",
            Some("differential_expression"),
            ClaimStatus::Verified,
        )],
        runtime_decision_log_path: None,
    }
}

fn coverage(
    addressed: usize,
    unverifiable: usize,
    absent: usize,
    per: &[(&str, EntityCoverage)],
) -> CoverageResult {
    let mut per_entity = BTreeMap::new();
    for (k, v) in per {
        per_entity.insert((*k).to_string(), *v);
    }
    CoverageResult {
        required_total: addressed + unverifiable + absent,
        required_addressed: addressed,
        required_unverifiable: unverifiable,
        required_absent: absent,
        per_entity,
    }
}

#[test]
fn all_required_addressed_passes_inv1() {
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    let cov = coverage(
        1,
        0,
        0,
        &[("differential_expression", EntityCoverage::Addressed)],
    );
    persist_signed_verdicts(
        dir.path(),
        "differential_expression",
        &report_one_verified(),
        Some(&cov),
        &w,
    )
    .unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    let v = check_claim_completeness(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn absent_required_fails_inv1() {
    // F2/F5: a Required expectation with no addressing claim ⇒ Inv 1 Fail,
    // even when the verdicts that ARE present are all clean.
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    let cov = coverage(0, 0, 1, &[("variant_calling", EntityCoverage::Absent)]);
    persist_signed_verdicts(
        dir.path(),
        "differential_expression",
        &report_one_verified(),
        Some(&cov),
        &w,
    )
    .unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    let v = check_claim_completeness(&pkg);
    assert_eq!(
        v.status,
        InvariantStatus::Fail,
        "Required-absent recall gap must FAIL Inv 1"
    );
    assert!(v.n_violations >= 1);
}

#[test]
fn unverifiable_required_fails_inv1() {
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    let cov = coverage(
        0,
        1,
        0,
        &[("variant_calling", EntityCoverage::Unverifiable)],
    );
    persist_signed_verdicts(
        dir.path(),
        "differential_expression",
        &report_one_verified(),
        Some(&cov),
        &w,
    )
    .unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    let v = check_claim_completeness(&pkg);
    assert_eq!(
        v.status,
        InvariantStatus::Fail,
        "Required-unverifiable must FAIL Inv 1"
    );
}

#[test]
fn no_coverage_block_preserves_phase1_behavior() {
    // A sink without a coverage block (un-recall-anchored, e.g. a task with
    // no manifest entries) keeps the Phase-1 verdict-only predicate.
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    persist_signed_verdicts(
        dir.path(),
        "differential_expression",
        &report_one_verified(),
        None,
        &w,
    )
    .unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    let v = check_claim_completeness(&pkg);
    assert_eq!(
        v.status,
        InvariantStatus::Pass,
        "no coverage ⇒ Phase-1 verdict-only pass"
    );
}
