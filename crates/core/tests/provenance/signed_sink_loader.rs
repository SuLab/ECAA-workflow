//! Loader reads + verifies the signed verdict sink; tamper sets the flag.
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

fn report() -> ClaimVerificationReport {
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
    ClaimVerificationReport {
        n_checked: 1,
        n_verified: 1,
        n_mismatch: 0,
        n_unverifiable: 0,
        n_suspicious: 0,
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
    persist_signed_verdicts(dir.path(), "diff_expr", &report(), None, &w).unwrap();

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
    let path = persist_signed_verdicts(dir.path(), "diff_expr", &report(), None, &w).unwrap();
    // Tamper: flip a byte in the verdicts payload after signing.
    let line = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, line.replace("verified", "pending")).unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    assert!(
        pkg.claims_tampered,
        "HMAC mismatch must set claims_tampered"
    );
}

/// A two-claim confirmatory report, both Verified with backing tables, so a
/// clean (Pass) `check_claim_completeness` over the loaded sink.
fn two_claim_report() -> ClaimVerificationReport {
    let mk = |entity: &str| Claim {
        entity: entity.into(),
        direction: None,
        effect_size: None,
        pvalue: None,
        source_table: Some("results/tables/de.csv".into()),
        excerpt: String::new(),
        contract: ClaimContract::NumericTableLookup,
        literature_evidence: None,
    };
    let v = |entity: &str| ClaimVerdict {
        claim: mk(entity),
        status: ClaimStatus::Verified,
        strength: ClaimStrength::default(),
    };
    ClaimVerificationReport {
        n_checked: 2,
        n_verified: 2,
        n_mismatch: 0,
        n_unverifiable: 0,
        n_suspicious: 0,
        verdicts: vec![v("TP53"), v("IL6")],
        runtime_decision_log_path: None,
    }
}

#[test]
fn double_finalize_does_not_double_count_inspected_claims() {
    // Regression: in a standalone clean pass `finalize_task` is invoked for the
    // SAME completed task twice (the per-task coverage gate AND the end-of-run
    // finalize_package), so `persist_signed_verdicts` — which is append-only —
    // appends two rows carrying IDENTICAL `claim_id`s (`<task>#claim-<i>`).
    // The loader's `union_signed_rows` must dedup by claim_id (keep last), so
    // `check_claim_completeness` inspects the TRUE distinct-claim count (2),
    // not double (4). Without the loader dedup this asserts 4 and FAILS.
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    let rep = two_claim_report();

    // Finalize the same task twice (gate + end-of-run), exactly as the
    // standalone harness path does.
    persist_signed_verdicts(dir.path(), "diff_expr", &rep, None, &w).unwrap();
    persist_signed_verdicts(dir.path(), "diff_expr", &rep, None, &w).unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    assert!(!pkg.claims_tampered, "each appended row is individually signed");

    // The unioned `claims` value carries exactly one row per distinct claim_id.
    let verdicts = pkg
        .claims
        .as_ref()
        .unwrap()
        .get("verdicts")
        .and_then(serde_json::Value::as_array)
        .unwrap();
    assert_eq!(
        verdicts.len(),
        2,
        "two finalizations of a 2-claim task must collapse to 2 distinct rows, not 4"
    );
    let ids: Vec<&str> = verdicts
        .iter()
        .map(|r| r["claim_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec!["diff_expr#claim-0", "diff_expr#claim-1"],
        "distinct claim_ids preserved in stable order; no duplicates"
    );

    // Inv 1 inspects the true distinct-claim count, not double.
    let verdict = check_claim_completeness(&pkg);
    assert_eq!(verdict.status, InvariantStatus::Pass);
    assert_eq!(
        verdict.n_inspected, 2,
        "n_inspected (→ plaintext n_checked) must equal the distinct-claim count, not 2×"
    );
}

#[test]
fn cross_task_distinct_claims_all_preserved() {
    // Guard the dedup against an F2 over-collapse: two DIFFERENT tasks carry
    // distinct `claim_id` prefixes, so every distinct claim must survive the
    // union (the dedup only collapses exact claim_id duplicates).
    let dir = tempfile::tempdir().unwrap();
    let w = AuditWriter::for_session();
    persist_signed_verdicts(dir.path(), "task_a", &two_claim_report(), None, &w).unwrap();
    persist_signed_verdicts(dir.path(), "task_b", &two_claim_report(), None, &w).unwrap();

    let pkg = LoadedPackage::from_root_with_verifier(dir.path(), Some(&w)).unwrap();
    let verdict = check_claim_completeness(&pkg);
    assert_eq!(
        verdict.n_inspected, 4,
        "four distinct claim_ids across two tasks must all be inspected"
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
