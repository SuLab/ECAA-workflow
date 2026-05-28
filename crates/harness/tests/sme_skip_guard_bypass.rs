//! Integration regression: when the SME has selected a skip option on a
//! literature stage (e.g. `emit_skip_sentinel_row`), the empty-result
//! guard helper must classify the intent so the silent-completion guard
//! bypasses the validator re-block path.
//!
//! Reproduces the `review_prior_work` agent-vs-guard loop described in
//! the bug report:
//!   1. SME picks "skip with deviation" → server writes sme-decisions.json
//!   2. Agent emits empty / 1-row sentinel CSV
//!   3. Guard reads sme-decisions.json → returns EmitSentinel
//!   4. Silent-completion guard short-circuits → no re-block
//!
//! The guard's actual short-circuit lives in `crates/harness/src/main.rs`;
//! this test pins the helper that gates that short-circuit. Two
//! additional tests cover the negative path (no skip → strict guard
//! applies) and the documented-deviation variant.

use scripps_workflow_harness::sme_skip::{detect_intent, SmeSkipIntent};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn write_sme_decisions(pkg: &Path, task_id: &str, json: &str) {
    let out = pkg.join("runtime/outputs").join(task_id);
    fs::create_dir_all(&out).unwrap();
    fs::write(out.join("sme-decisions.json"), json).unwrap();
}

fn write_sentinel_csv(pkg: &Path, task_id: &str) {
    let out = pkg.join("runtime/outputs").join(task_id);
    fs::create_dir_all(&out).unwrap();
    fs::write(
        out.join("prior_claims_matrix.csv"),
        "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
sentinel,gene,,,0,none,sha256:0,2026-05-24T00:00:00Z,true,false\n",
    )
    .unwrap();
}

#[test]
fn sme_skip_with_emit_sentinel_authorizes_completion() {
    // Set up a package with review_prior_work emitting a sentinel CSV
    // after SME selected emit_skip_sentinel_row on the blocker.
    let tmp = TempDir::new().unwrap();
    write_sme_decisions(
        tmp.path(),
        "review_prior_work",
        r#"{
            "task_id": "review_prior_work",
            "timestamp": "2026-05-24T00:00:00Z",
            "decisions": [
                {"id": "review_prior_work_disposition", "chosen": "emit_skip_sentinel_row"}
            ],
            "rationale": "SME declined to register a corpus and approved sentinel-row completion"
        }"#,
    );
    write_sentinel_csv(tmp.path(), "review_prior_work");

    let intent = detect_intent(tmp.path(), "review_prior_work");
    assert_eq!(
        intent,
        SmeSkipIntent::EmitSentinel,
        "guard must classify SME's explicit skip choice so the silent-completion path bypasses re-block"
    );
    assert!(intent.is_skip(), "EmitSentinel must register as a skip");
}

#[test]
fn no_sme_decisions_file_means_strict_guard_path() {
    // Negative path: when the SME has NOT acknowledged a skip, the helper
    // must return None so the guard runs its normal strict checks (which
    // would re-block the empty sentinel CSV).
    let tmp = TempDir::new().unwrap();
    write_sentinel_csv(tmp.path(), "review_prior_work");

    let intent = detect_intent(tmp.path(), "review_prior_work");
    assert_eq!(
        intent,
        SmeSkipIntent::None,
        "no sme-decisions.json must NOT permissively accept empty/sentinel completion"
    );
}

#[test]
fn supply_local_corpus_choice_is_not_a_skip() {
    // SME picked a non-skip recovery option (supply a local corpus). The
    // guard must continue to enforce real rows once the corpus arrives.
    let tmp = TempDir::new().unwrap();
    write_sme_decisions(
        tmp.path(),
        "review_prior_work",
        r#"{"decisions":[{"id":"q1","chosen":"supply_local_corpus"}]}"#,
    );

    assert_eq!(
        detect_intent(tmp.path(), "review_prior_work"),
        SmeSkipIntent::None,
        "non-skip recovery options must not bypass the literature contract"
    );
}

#[test]
fn drop_stage_choice_authorizes_completion() {
    // SME requested removing the stage; guard must not loop in the
    // interim window before the amendment path removes the task.
    let tmp = TempDir::new().unwrap();
    write_sme_decisions(
        tmp.path(),
        "review_prior_work",
        r#"{"decisions":[{"id":"q1","chosen":"drop_stage_from_workflow"}]}"#,
    );

    assert_eq!(
        detect_intent(tmp.path(), "review_prior_work"),
        SmeSkipIntent::DropStage,
    );
}
