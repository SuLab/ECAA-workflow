//! Tests that the harness promotes auto-advanced discover_* decisions into
//! `runtime/decisions.jsonl` (Decision subgraph) so audit-proof
//! `decision_justification` is no longer Unverified on standalone runs.
//!
//! Drives `scheduler::promote_auto_advance_decisions` directly against a
//! checked-in fixture (`tests/fixtures/auto-advance-pkg`) that contains:
//!   - `runtime/outputs/discover_normalisation/decision.json`
//!     with `auto_advanced = true`
//!   - `runtime/.sme-auto-approve-discoveries` allow-entry for
//!     "normalisation"
//!
//! Asserts:
//!   1. After one call: `runtime/decisions.jsonl` contains ≥1 line that
//!      parses to a `DecisionRecord` with `actor == Harness` and
//!      `authority == SchemaValidated`.
//!   2. Idempotency: calling a second time with the SAME `already_recorded`
//!      set leaves the line count unchanged (no duplicate appended).

use ecaa_workflow_core::decision_log::{DecisionActor, DecisionAuthority, DecisionRecord};
use ecaa_workflow_harness::scheduler::promote_auto_advance_decisions;
use std::path::Path;

/// Recursively copy `src` → `dst`.
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

fn stage_auto_advance_pkg() -> (tempfile::TempDir, std::path::PathBuf) {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/auto-advance-pkg");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pkg");
    copy_tree(&fixture, &root);
    (tmp, root)
}

fn read_decision_records(pkg: &Path) -> Vec<DecisionRecord> {
    let path = pkg.join("runtime").join("decisions.jsonl");
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<DecisionRecord>(l).expect("valid DecisionRecord"))
        .collect()
}

#[test]
fn auto_advanced_stage_is_promoted_to_decisions_jsonl() {
    let (_tmp, root) = stage_auto_advance_pkg();
    let mut already_recorded = std::collections::BTreeSet::new();

    // First call: should write exactly 1 record for discover_normalisation.
    promote_auto_advance_decisions(&root, "test-session-id", &mut already_recorded);

    let records = read_decision_records(&root);
    assert!(
        !records.is_empty(),
        "decisions.jsonl must contain at least 1 record after promoting auto-advanced stage"
    );

    // Every record written by the harness must carry Harness actor + SchemaValidated.
    let harness_records: Vec<&DecisionRecord> = records
        .iter()
        .filter(|r| r.actor == DecisionActor::Harness)
        .collect();
    assert!(
        !harness_records.is_empty(),
        "at least one record must have actor == Harness; got records: {records:?}"
    );
    for r in &harness_records {
        assert_eq!(
            r.authority,
            DecisionAuthority::SchemaValidated,
            "Harness actor must map to SchemaValidated authority; got: {r:?}"
        );
    }
}

#[test]
fn idempotent_on_second_call_with_same_guard_set() {
    let (_tmp, root) = stage_auto_advance_pkg();
    let mut already_recorded = std::collections::BTreeSet::new();

    // First call writes 1 record.
    promote_auto_advance_decisions(&root, "test-session-id", &mut already_recorded);
    let count_after_first = read_decision_records(&root).len();
    assert!(count_after_first >= 1, "first call must write at least 1 record");

    // Second call with the SAME guard set must not append any new lines.
    promote_auto_advance_decisions(&root, "test-session-id", &mut already_recorded);
    let count_after_second = read_decision_records(&root).len();
    assert_eq!(
        count_after_second,
        count_after_first,
        "second call with same already_recorded must not append duplicates; \
         first={count_after_first} second={count_after_second}"
    );
}

#[test]
fn non_auto_advanced_decision_json_is_not_promoted() {
    let (_tmp, root) = stage_auto_advance_pkg();

    // Write a second task dir with auto_advanced = false.
    let extra_dir = root.join("runtime/outputs/discover_integration");
    std::fs::create_dir_all(&extra_dir).unwrap();
    std::fs::write(
        extra_dir.join("decision.json"),
        br#"{"auto_advanced": false, "method": "harmony"}"#,
    )
    .unwrap();

    let mut already_recorded = std::collections::BTreeSet::new();
    promote_auto_advance_decisions(&root, "test-session-id", &mut already_recorded);

    let records = read_decision_records(&root);
    // Only discover_normalisation (auto_advanced=true) should be promoted.
    // discover_integration (auto_advanced=false) must not appear.
    let integration_records: Vec<&DecisionRecord> = records
        .iter()
        .filter(|r| {
            let json = serde_json::to_value(&r.decision).unwrap();
            json.get("stage")
                .and_then(|v| v.as_str())
                .map(|s| s.contains("discover_integration"))
                .unwrap_or(false)
        })
        .collect();
    assert!(
        integration_records.is_empty(),
        "non-auto-advanced stage must not appear in decisions.jsonl; found: {integration_records:?}"
    );
}
