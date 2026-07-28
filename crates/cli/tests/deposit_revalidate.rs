//! Post-seal revalidation — `deposit-check --revalidate`.
//!
//! The deposit-readiness attestation records a verdict computed AT EXPORT
//! TIME; every downstream consumer re-reads that verdict rather than the
//! evidence behind it. Post-seal revalidation re-runs the offline-checkable
//! subset of the package's own assertions against the sealed tree, and its
//! load-bearing class is the one BagIt integrity structurally cannot cover: a
//! per-task report asserting `artifact_presence.X: PASS
//! (runtime/outputs/<task>/X.tsv)` for a file the sealed tree does not
//! contain. A manifest walk only ever sees files that exist, so an artifact
//! absent from BOTH the tree and the manifest verifies clean.
//!
//! The scan + report writer live in `crates/core`
//! (`deposit_readiness::run_post_seal_revalidation`), per the repo's
//! never-fork-logic-between-a-binary-and-core rule; the CLI flag is a thin
//! shim over it. These tests drive the core entry point directly so the gate
//! semantics are locked independently of the flag plumbing.
//!
//! Deliberately modality-agnostic: the fixture stages are `analysis_stage` /
//! `validate_analysis_stage` with a generic `results_table.tsv`, so nothing
//! here encodes a differential-expression (or any other) shape.

use std::path::Path;

use ecaa_workflow_core::clock::WallClock;
use ecaa_workflow_core::deposit_readiness::{
    revalidate_post_seal, run_post_seal_revalidation, POST_SEAL_VALIDATION_FILE,
};
use tempfile::TempDir;

/// Write `runtime/outputs/<task_id>/<name>` with `body` as its contents.
fn write_output(root: &Path, task_id: &str, name: &str, body: &str) {
    let dir = root.join("runtime").join("outputs").join(task_id);
    std::fs::create_dir_all(&dir).expect("creating task output dir");
    std::fs::write(dir.join(name), body).expect("writing task output");
}

/// A `validate_*` companion report asserting that two upstream artifacts are
/// present, in the shape the agent-authored validators actually emit (an
/// `artifact_presence.*` check id plus a free-form `detail` carrying the
/// path).
fn presence_report(validated_stage: &str) -> String {
    serde_json::json!({
        "task_id": format!("validate_{validated_stage}"),
        "validated_stage": validated_stage,
        "n_checks": 2,
        "n_pass": 2,
        "n_fail": 0,
        "validation_result": "PASS",
        "checks": [
            {
                "id": "artifact_presence.results_table",
                "description": "expected artifact results_table.tsv exists",
                "passed": true,
                "detail": format!(
                    "path: runtime/outputs/{validated_stage}/results_table.tsv, exists: True"
                ),
            },
            {
                "id": "artifact_presence.summary_json",
                "description": "expected artifact summary.json exists",
                "passed": true,
                "detail": format!(
                    "path: runtime/outputs/{validated_stage}/summary.json, exists: True"
                ),
            }
        ]
    })
    .to_string()
}

/// Stage a minimal sealed-tree fixture. `with_results_table` controls whether
/// the artifact the validation report asserts is actually present.
fn stage_deposit(with_results_table: bool) -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();

    write_output(
        root,
        "analysis_stage",
        "result.json",
        r#"{"status":"completed"}"#,
    );
    write_output(root, "analysis_stage", "summary.json", r#"{"n_rows":3}"#);
    if with_results_table {
        write_output(
            root,
            "analysis_stage",
            "results_table.tsv",
            "id\tvalue\na\t1\n",
        );
    }
    write_output(
        root,
        "validate_analysis_stage",
        "validation_report.json",
        &presence_report("analysis_stage"),
    );
    write_output(
        root,
        "validate_analysis_stage",
        "result.json",
        r#"{"validation_result":"PASS","validated_stage":"analysis_stage"}"#,
    );
    tmp
}

/// The gate's reason for existing: the package's own report asserts a file
/// that is not in the sealed tree. The scan must name it, `passed` must be
/// false, and `--strict` must refuse the package — while the CONSISTENT claim
/// in the same report is not flagged.
#[test]
fn revalidate_detects_a_presence_claim_for_an_absent_file() {
    let tmp = stage_deposit(false);
    let root = tmp.path();

    let report = revalidate_post_seal(root, &WallClock);
    assert!(
        report.claims_checked >= 2,
        "the scan must actually recover the report's presence claims (a scan that \
         found nothing would pass vacuously): {report:?}"
    );
    assert_eq!(
        report
            .missing_claims
            .iter()
            .map(|c| c.claimed_path.as_str())
            .collect::<Vec<_>>(),
        vec!["runtime/outputs/analysis_stage/results_table.tsv"],
        "only the absent artifact may be flagged: {report:?}"
    );
    assert_eq!(report.missing_claims[0].task_id, "validate_analysis_stage");
    assert_eq!(report.missing_claims[0].source, "validation_report.json");
    assert!(!report.passed);
    assert!(!report.presence_claims_hold());

    // Non-strict: the finding is recorded, the package is not refused.
    let written = run_post_seal_revalidation(root, false, &WallClock)
        .expect("non-strict revalidation records the finding without refusing");
    assert_eq!(written.missing_claims.len(), 1);

    // The report is written into the sealed tree either way.
    let report_path = root.join(POST_SEAL_VALIDATION_FILE);
    let raw = std::fs::read_to_string(&report_path).expect("post-seal report must be written");
    assert!(
        raw.contains("results_table.tsv"),
        "the written report must name the absent artifact: {raw}"
    );

    // Strict: the package is refused, and the error names the missing claim.
    let err = run_post_seal_revalidation(root, true, &WallClock)
        .expect_err("--strict must refuse a package that asserts an artifact it does not contain");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("results_table.tsv") && msg.contains("post-seal revalidation"),
        "the refusal must be actionable: {msg}"
    );
}

/// The over-block guard: a deposit whose every presence claim resolves must
/// pass, under `--strict` too, and must still have inspected something.
#[test]
fn revalidate_passes_on_a_consistent_deposit() {
    let tmp = stage_deposit(true);
    let root = tmp.path();

    let report = run_post_seal_revalidation(root, true, &WallClock)
        .expect("a consistent deposit must survive --strict revalidation");
    assert!(
        report.claims_checked >= 2,
        "a vacuous scan must not be reported as a pass: {report:?}"
    );
    assert!(
        report.missing_claims.is_empty(),
        "no claim may be flagged on a consistent deposit: {report:?}"
    );
    assert!(report.reporting_required_failures.is_empty());
    assert!(report.contract_obligation_failures.is_empty());
    assert!(report.passed, "{report:?}");

    let raw = std::fs::read_to_string(root.join(POST_SEAL_VALIDATION_FILE))
        .expect("post-seal report must be written");
    let parsed: serde_json::Value = serde_json::from_str(&raw).expect("report must be valid JSON");
    assert_eq!(parsed["passed"], serde_json::Value::Bool(true));
    assert_eq!(parsed["schema_version"], "0.1");
}

/// Stage the shape a real re-executable deposit has: the artifact is absent
/// because the export tier gate dropped it, and `export` recorded that drop on
/// the very report that cites it.
fn stage_deposit_with_export_disclosure(dropped_at_export: bool, on_own_report: bool) -> TempDir {
    let tmp = stage_deposit(false);
    let root = tmp.path();
    let block = serde_json::json!({
        "note": "Written by `ecaa-workflow export`.",
        "unavailable": [{
            "path": "runtime/outputs/analysis_stage/results_table.tsv",
            "available": false,
            "dropped_at_export": dropped_at_export,
        }],
    });
    if on_own_report {
        // What `export` actually does: the block lands on EVERY report whose
        // citations the drop invalidated — here the validation report that
        // claims the artifact.
        let mut doc: serde_json::Value =
            serde_json::from_str(&presence_report("analysis_stage")).expect("valid fixture");
        doc.as_object_mut()
            .expect("object")
            .insert("export_reconciliation".to_string(), block);
        write_output(
            root,
            "validate_analysis_stage",
            "validation_report.json",
            &doc.to_string(),
        );
    } else {
        // Disclosed by a DIFFERENT task's report than the one making the claim.
        write_output(
            root,
            "analysis_stage",
            "result.json",
            &serde_json::json!({ "status": "completed", "export_reconciliation": block })
                .to_string(),
        );
    }
    tmp
}

/// A claim whose target the export deliberately dropped, disclosed on the same
/// report, is reconciled — not a missing claim. Re-flagging it reported the
/// package as dishonest about precisely the thing it was honest about, and
/// refused every real `--profile re-executable` deposit under `--strict`
/// (`intermediates/` and `view_data/` are always dropped).
#[test]
fn revalidate_honors_an_export_dropped_disclosure() {
    let tmp = stage_deposit_with_export_disclosure(true, true);
    let root = tmp.path();

    let report = revalidate_post_seal(root, &WallClock);
    assert!(
        report.claims_checked >= 2,
        "a vacuous scan must not be reported as a pass: {report:?}"
    );
    assert!(
        report.missing_claims.is_empty(),
        "a disclosed export drop is not a missing claim: {report:?}"
    );
    assert!(
        report
            .reconciled_claims
            .iter()
            .any(|c| c.claimed_path == "runtime/outputs/analysis_stage/results_table.tsv"),
        "the drop must still be SURFACED, not silently swallowed: {report:?}"
    );
    assert!(report.presence_claims_hold());
    assert!(report.passed, "{report:?}");

    run_post_seal_revalidation(root, true, &WallClock)
        .expect("--strict must accept a deposit whose absent artifacts are disclosed drops");
}

/// The exemption is evidence-gated: `available: false` alone does not excuse a
/// dangling citation, because a producer would then excuse itself by asserting
/// the very thing under test. Only `dropped_at_export` counts.
#[test]
fn revalidate_rejects_an_unavailable_entry_that_is_not_an_export_drop() {
    let tmp = stage_deposit_with_export_disclosure(false, true);
    let root = tmp.path();

    let report = revalidate_post_seal(root, &WallClock);
    assert!(
        report
            .missing_claims
            .iter()
            .any(|c| c.claimed_path == "runtime/outputs/analysis_stage/results_table.tsv"),
        "an `available: false` entry without `dropped_at_export` must stay missing: {report:?}"
    );
    assert!(report.reconciled_claims.is_empty(), "{report:?}");
    assert!(!report.presence_claims_hold());
    run_post_seal_revalidation(root, true, &WallClock)
        .expect_err("--strict must still refuse an undisclosed absence");
}

/// The exemption is scoped to the report making the claim: a drop admitted by
/// one task's report must not excuse another task's dangling citation.
#[test]
fn revalidate_does_not_let_one_report_excuse_another() {
    let tmp = stage_deposit_with_export_disclosure(true, false);
    let root = tmp.path();

    let report = revalidate_post_seal(root, &WallClock);
    assert!(
        report
            .missing_claims
            .iter()
            .any(|c| c.task_id == "validate_analysis_stage"),
        "the validate task's claim is undisclosed on ITS OWN report: {report:?}"
    );
    run_post_seal_revalidation(root, true, &WallClock)
        .expect_err("--strict must refuse a claim disclosed only on someone else's report");
}
