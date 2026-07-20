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
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
                aggregate_kind: None,
                aggregate_column: None,
                aggregate_rowset: None,
                aggregate_value: None,
                collection: None,
                term: None,
                keyed_column: None,
                keyed_value: None,
        },
        status,
        strength: ClaimStrength::Exploratory,
        audit: None,
    }
}

fn report_one_verified() -> ClaimVerificationReport {
    ClaimVerificationReport {
        n_checked: 1,
        n_verified: 1,
        n_mismatch: 0,
        n_unverifiable: 0,
        n_pending: 0,
        n_suspicious: 0,
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

#[test]
fn accumulator_gap_survives_later_coverage_less_task() {
    // F2 at-rest ERASURE regression: an earlier confirmatory task records a
    // Required-absent gap; a later un-anchored task appends a coverage-less
    // row. The cross-task UNION must still surface the gap (Inv 1 Fail) — the
    // last-writer REPLACE used to overwrite it to a clean Pass.
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    // Task 1: a recall gap.
    let gap = coverage(0, 0, 1, &[("variant_calling", EntityCoverage::Absent)]);
    persist_signed_verdicts(
        dir.path(),
        "call_variants",
        &report_one_verified(),
        Some(&gap),
        &w,
    )
    .unwrap();
    // Task 2: un-anchored (no manifest) ⇒ no coverage block.
    persist_signed_verdicts(
        dir.path(),
        "render_report",
        &report_one_verified(),
        None,
        &w,
    )
    .unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    let v = check_claim_completeness(&pkg);
    assert_eq!(
        v.status,
        InvariantStatus::Fail,
        "recorded recall gap must survive a later coverage-less task (no erasure)"
    );
    assert!(v.n_violations >= 1);
}

#[test]
fn accumulator_later_task_resolves_earlier_gap() {
    // RESOLUTION: a later task that DOES address the entity resolves the
    // earlier gap (best-outcome-resolves), so the union grades Pass.
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    let gap = coverage(0, 0, 1, &[("variant_calling", EntityCoverage::Absent)]);
    persist_signed_verdicts(
        dir.path(),
        "call_variants",
        &report_one_verified(),
        Some(&gap),
        &w,
    )
    .unwrap();
    let fixed = coverage(1, 0, 0, &[("variant_calling", EntityCoverage::Addressed)]);
    persist_signed_verdicts(
        dir.path(),
        "call_variants_rerun",
        &report_one_verified(),
        Some(&fixed),
        &w,
    )
    .unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    let v = check_claim_completeness(&pkg);
    assert_eq!(
        v.status,
        InvariantStatus::Pass,
        "a later Addressed verdict resolves the earlier Absent gap"
    );
}

#[test]
fn accumulator_single_row_grades_unchanged() {
    // BACKWARD-COMPAT: a single appended row must grade exactly as before the
    // accumulator (the 1-row loader path returns the inner doc as-is).
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    let gap = coverage(0, 0, 1, &[("variant_calling", EntityCoverage::Absent)]);
    persist_signed_verdicts(
        dir.path(),
        "call_variants",
        &report_one_verified(),
        Some(&gap),
        &w,
    )
    .unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    assert!(!pkg.claims_tampered);
    let v = check_claim_completeness(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
}

#[test]
fn accumulator_tampered_row_among_many_sets_flag() {
    // TAMPER: any tampered row in the appended sink ⇒ claims_tampered ⇒
    // Inv 1 Fail, even if the other rows verify.
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    let ok1 = coverage(
        1,
        0,
        0,
        &[("differential_expression", EntityCoverage::Addressed)],
    );
    persist_signed_verdicts(
        dir.path(),
        "diff_expr",
        &report_one_verified(),
        Some(&ok1),
        &w,
    )
    .unwrap();
    let ok2 = coverage(1, 0, 0, &[("variant_calling", EntityCoverage::Addressed)]);
    persist_signed_verdicts(
        dir.path(),
        "call_variants",
        &report_one_verified(),
        Some(&ok2),
        &w,
    )
    .unwrap();

    // Tamper one row's payload after signing (the proven flip used elsewhere).
    let path = dir
        .path()
        .join("runtime/verification-reports/claim-verification.signed.json");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, raw.replacen("verified", "pending", 1)).unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    assert!(
        pkg.claims_tampered,
        "a tampered appended row must set claims_tampered"
    );
    let v = check_claim_completeness(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
}
