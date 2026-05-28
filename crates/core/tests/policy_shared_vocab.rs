//! Tests for the `$shared` vocabulary substitution mechanism in
//! `policy_schema::load_and_validate`. Verifies that values listed in
//! `config/downstream-policy/_shared-vocab.json` are substituted into
//! each referencing policy before schema validation, producing
//! byte-identical resolved values across all consumers.

use scripps_workflow_core::policy_schema::load_and_validate;
use std::fs;
use std::path::{Path, PathBuf};

fn policies_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy")
}

/// Canonical freshness-acceptable-statuses list from `_shared-vocab.json`.
fn canonical_freshness_statuses() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!("current"),
        serde_json::json!("fresh"),
        serde_json::json!("pinned"),
        serde_json::json!("up to date"),
        serde_json::json!("uptodate"),
    ]
}

/// Canonical contradiction-blocking-statuses list from `_shared-vocab.json`.
fn canonical_contradiction_statuses() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!("contradicted"),
        serde_json::json!("mixed"),
        serde_json::json!("unresolved"),
        serde_json::json!("retracted"),
    ]
}

/// After loading `best-practice-scoring-policy.json`, `freshness.acceptableStatuses`
/// must equal the 5-item canonical list defined in `_shared-vocab.json`.
#[test]
fn scoring_policy_freshness_statuses_resolved() {
    let path = policies_dir().join("best-practice-scoring-policy.json");
    let v = load_and_validate(&path).expect("scoring policy loads and validates");
    let actual = v["freshness"]["acceptableStatuses"]
        .as_array()
        .expect("freshness.acceptableStatuses is an array");
    assert_eq!(
        actual,
        &canonical_freshness_statuses(),
        "freshness.acceptableStatuses in scoring policy does not match shared vocab"
    );
}

/// After loading `best-practice-validation-contract.json`,
/// `strongClaimRequirements.freshnessAcceptableStatuses` must equal the same
/// 5-item canonical list.
#[test]
fn validation_contract_freshness_statuses_resolved() {
    let path = policies_dir().join("best-practice-validation-contract.json");
    let v = load_and_validate(&path).expect("validation contract loads and validates");
    let actual = v["strongClaimRequirements"]["freshnessAcceptableStatuses"]
        .as_array()
        .expect("strongClaimRequirements.freshnessAcceptableStatuses is an array");
    assert_eq!(
        actual,
        &canonical_freshness_statuses(),
        "freshnessAcceptableStatuses in validation contract does not match shared vocab"
    );
}

/// All policies that reference `freshnessAcceptableStatuses` or
/// `contradictionBlockingStatuses` must resolve to byte-identical content —
/// ensuring they can't drift apart.
#[test]
fn freshness_and_contradiction_statuses_byte_identical_across_policies() {
    let scoring_path = policies_dir().join("best-practice-scoring-policy.json");
    let contract_path = policies_dir().join("best-practice-validation-contract.json");

    let scoring = load_and_validate(&scoring_path).expect("scoring policy loads");
    let contract = load_and_validate(&contract_path).expect("validation contract loads");

    let freshness_from_scoring = scoring["freshness"]["acceptableStatuses"]
        .as_array()
        .expect("freshness.acceptableStatuses is array");
    let freshness_from_contract = contract["strongClaimRequirements"]
        ["freshnessAcceptableStatuses"]
        .as_array()
        .expect("strongClaimRequirements.freshnessAcceptableStatuses is array");

    assert_eq!(
        freshness_from_scoring, freshness_from_contract,
        "freshnessAcceptableStatuses diverged between scoring policy and validation contract"
    );
    assert_eq!(
        freshness_from_scoring,
        &canonical_freshness_statuses(),
        "freshness statuses don't match canonical list"
    );

    let contradiction_from_scoring = scoring["contradiction"]["blockingStatuses"]
        .as_array()
        .expect("contradiction.blockingStatuses is array");
    let contradiction_from_contract = contract["strongClaimRequirements"]
        ["contradictionBlockingStatuses"]
        .as_array()
        .expect("strongClaimRequirements.contradictionBlockingStatuses is array");

    assert_eq!(
        contradiction_from_scoring, contradiction_from_contract,
        "contradictionBlockingStatuses diverged between scoring policy and validation contract"
    );
    assert_eq!(
        contradiction_from_scoring,
        &canonical_contradiction_statuses(),
        "contradiction statuses don't match canonical list"
    );
}

/// `strongClaimMinEvidence: 2` appears in both `best-practice-evidence-policy.json`
/// (`citationMinimum.strongClaimMinEvidence`) and `best-practice-validation-contract.json`
/// (`strongClaimRequirements.minUniqueEvidenceRecords`). Both must resolve to 2.
#[test]
fn strong_claim_min_evidence_resolved_in_both_policies() {
    let evidence_path = policies_dir().join("best-practice-evidence-policy.json");
    let contract_path = policies_dir().join("best-practice-validation-contract.json");

    let evidence = load_and_validate(&evidence_path).expect("evidence policy loads");
    let contract = load_and_validate(&contract_path).expect("validation contract loads");

    let from_evidence = evidence["citationMinimum"]["strongClaimMinEvidence"]
        .as_u64()
        .expect("citationMinimum.strongClaimMinEvidence is a number");
    let from_contract = contract["strongClaimRequirements"]["minUniqueEvidenceRecords"]
        .as_u64()
        .expect("strongClaimRequirements.minUniqueEvidenceRecords is a number");

    assert_eq!(
        from_evidence, 2,
        "evidence policy strongClaimMinEvidence != 2"
    );
    assert_eq!(
        from_contract, 2,
        "validation contract minUniqueEvidenceRecords != 2"
    );
    assert_eq!(
        from_evidence, from_contract,
        "strongClaimMinEvidence diverged between evidence policy and validation contract"
    );
}

/// A policy containing `{{\"$shared\": \"no_such_key\"}}` must cause `load_and_validate`
/// to return a clear `Err`, not silently substitute an empty or null value.
#[test]
fn unknown_shared_key_returns_error_not_silent_empty() {
    let tmp = tempfile::tempdir().unwrap();
    // Write a minimal policy using an unknown $shared key.
    let policy_path = tmp.path().join("bad-shared.json");
    fs::write(
        &policy_path,
        r#"{
  "schemaVersion": "1.0",
  "freshness": {
    "acceptableStatuses": {"$shared": "no_such_key_xyz"}
  }
}"#,
    )
    .unwrap();
    // Copy `_shared-vocab.json` into the tmp dir so the loader finds it.
    let vocab_src = policies_dir().join("_shared-vocab.json");
    fs::copy(&vocab_src, tmp.path().join("_shared-vocab.json")).unwrap();

    let err =
        load_and_validate(&policy_path).expect_err("unknown $shared key must produce an error");
    let msg = format!("{:#}", err);
    assert!(
        msg.contains("no_such_key_xyz") || msg.contains("unknown") || msg.contains("$shared"),
        "error message should mention the unknown key, got: {}",
        msg
    );
}

/// A sentinel object with sibling keys alongside `$shared` must be rejected.
/// Silently discarding the extra keys would lose author intent (e.g., a
/// `$comment` sibling) without any signal.
#[test]
fn shared_sentinel_with_sibling_keys_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let policy_path = tmp.path().join("bad-sibling.json");
    fs::write(
        &policy_path,
        r#"{
  "schemaVersion": "1.0",
  "freshness": {
    "acceptableStatuses": {
      "$shared": "freshnessAcceptableStatuses",
      "$comment": "this comment would be silently dropped"
    }
  }
}"#,
    )
    .unwrap();
    let vocab_src = policies_dir().join("_shared-vocab.json");
    fs::copy(&vocab_src, tmp.path().join("_shared-vocab.json")).unwrap();

    let err = load_and_validate(&policy_path)
        .expect_err("$shared sentinel with extra sibling keys must error");
    let msg = format!("{:#}", err);
    assert!(
        msg.contains("$shared") && msg.contains("sibling"),
        "error message should explain the sibling-key rejection, got: {}",
        msg
    );
}
