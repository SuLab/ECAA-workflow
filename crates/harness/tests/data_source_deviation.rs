//! A run-time data-source substitution must land in the TYPED audit
//! trail, not only in agent free text.
//!
//! Reproduces the 2026-07-25 himes run (package
//! `95c08fba…-bulk_rnaseq-20260725T200630`): `data_acquisition` could not
//! reach the SME's local counts directory and silently substituted the
//! Bioconductor `airway` package. The substitution was recorded ONLY in
//! `runtime/LOG.jsonl` (`{"decision":"source_selection",…}`),
//! `result.json::provenance_note` and
//! `per_accession_summary.json::provenance` — all free text. The 14
//! records in `runtime/decisions.jsonl` were all intake-side
//! (`append_intake_prose`, `set_intake_field`, `confirm`,
//! `auto_advanced`, `emit_package`, `unblock`, `set_intake_method`); none
//! named the substitution. The deviation happens at EXECUTION time,
//! post-emission, and the agent is forbidden from writing
//! `decisions.jsonl`, so nothing in the typed pipeline could capture it.
//!
//! The fix is a two-halved contract: the agent RECORDS
//! `result.json::source_deviation`, the HARNESS promotes it into a typed
//! `DecisionType::DataSourceDeviation`, and a REQUIRED obligation fails
//! the task when the record is missing or contradicts the package's own
//! `per_accession_summary.json`.

use ecaa_workflow_core::clock::FrozenClock;
use ecaa_workflow_core::decision_log::{
    DecisionActor, DecisionAuthority, DecisionRecord, DecisionType,
};
use ecaa_workflow_harness::validators::{
    promote_source_deviation, source_deviation_recorded, ValidatorOutcome,
};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const TASK: &str = "data_acquisition";

fn artifact_dir(root: &Path) -> PathBuf {
    root.join("runtime").join("outputs").join(TASK)
}

/// Write `runtime/outputs/<TASK>/result.json` with the given body.
fn write_result(root: &Path, body: serde_json::Value) {
    let dir = artifact_dir(root);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("result.json"), body.to_string()).unwrap();
}

fn write_per_accession_summary(root: &Path, body: serde_json::Value) {
    let dir = artifact_dir(root);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("per_accession_summary.json"), body.to_string()).unwrap();
}

/// The himes substitution, in the shape the atom's RECORD-WHAT-YOU-DID
/// clause asks the agent for.
fn himes_deviation() -> serde_json::Value {
    serde_json::json!({
        "requested": "/home/a/.ecaa-workflow/himes-inputs",
        "requested_available": false,
        "used": "Bioconductor airway package",
        "used_kind": "package",
        "used_version": "1.30.0",
        "reason": "the SME-specified local path was absent; the airway package is the \
                   canonical published distribution of the same GSE52778 count matrix",
        "checksums": { "data/himes_GSE52778/counts.tsv": "407b9f8dfe619580b3b0bbaa0464" }
    })
}

/// The summary the real run wrote — it DOES name the substitute source
/// (`source_package`), so it corroborates the deviation record.
fn corroborating_summary() -> serde_json::Value {
    serde_json::json!({
        "accession": "GSE52778",
        "source_package": "airway (Bioconductor)",
        "package_version": "1.30.0",
        "n_samples": 8,
        "provenance": "Retrieved from Bioconductor airway package (v1.30.0) which contains \
                       the Himes et al. 2014 GSE52778 dataset as a SummarizedExperiment."
    })
}

fn decision_lines(root: &Path) -> Vec<DecisionRecord> {
    let path = root.join("runtime").join("decisions.jsonl");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<DecisionRecord>(l).expect("every row parses"))
        .collect()
}

/// The harness — NOT the agent — promotes `result.json::source_deviation`
/// into one typed `DataSourceDeviation` record, carrying every field the
/// free-text note carried plus the checksums. Re-running the promotion
/// (the completion loop re-inspects every Completed task on every pass)
/// must not duplicate the row, and the REQUIRED obligation must pass once
/// the record exists.
#[test]
fn source_deviation_becomes_a_typed_decision() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_result(
        root,
        serde_json::json!({
            "task_id": TASK,
            "status": "completed",
            "provenance_note": "SME-specified local path was absent; used the airway package.",
            "source_deviation": himes_deviation(),
        }),
    );
    write_per_accession_summary(root, corroborating_summary());

    let clock = FrozenClock::default();
    let declared = promote_source_deviation(root, TASK, "session-under-test", &clock);
    assert!(
        declared.is_some(),
        "the promotion must report the declared substitution back to the harness"
    );

    let records = decision_lines(root);
    assert_eq!(
        records.len(),
        1,
        "exactly one decision row must be appended; got {records:?}"
    );
    let record = &records[0];
    assert_eq!(record.session_id, "session-under-test");
    assert_eq!(
        record.actor,
        DecisionActor::Harness,
        "the HARNESS writes this record — the agent may not touch decisions.jsonl"
    );
    assert_eq!(
        record.authority,
        DecisionAuthority::SchemaValidated,
        "derived deterministically from the task artifact, not LLM-inferred"
    );
    assert_eq!(
        record.timestamp, clock.at,
        "the record must take its timestamp from the injected clock (C6)"
    );

    match &record.decision {
        DecisionType::DataSourceDeviation { task_id, deviation } => {
            assert_eq!(task_id.as_str(), TASK);
            assert_eq!(deviation.requested, "/home/a/.ecaa-workflow/himes-inputs");
            assert!(
                !deviation.requested_available,
                "the requested source was absent — that is the whole point of the record"
            );
            assert_eq!(deviation.used, "Bioconductor airway package");
            assert_eq!(deviation.used_kind, "package");
            assert_eq!(deviation.used_version.as_deref(), Some("1.30.0"));
            assert!(deviation.reason.contains("absent"), "{}", deviation.reason);
            assert_eq!(
                deviation
                    .checksums
                    .get("data/himes_GSE52778/counts.tsv")
                    .map(String::as_str),
                Some("407b9f8dfe619580b3b0bbaa0464"),
                "the checksums of the bytes actually analysed must survive into the record"
            );
        }
        other => panic!("expected DataSourceDeviation, got {other:?}"),
    }

    // Idempotent across loop passes AND across harness restarts.
    promote_source_deviation(root, TASK, "session-under-test", &clock);
    promote_source_deviation(root, TASK, "session-under-test", &clock);
    assert_eq!(
        decision_lines(root).len(),
        1,
        "re-entering the completion loop must not duplicate the row"
    );

    // With the record in place the REQUIRED obligation is satisfied.
    assert_eq!(
        source_deviation_recorded(&artifact_dir(root)),
        ValidatorOutcome::Passed,
        "a declared substitution with a matching typed record must pass"
    );
}

/// The enforcement half. A `result.json` that claims a substitution with
/// no corresponding decision record is exactly the himes failure mode and
/// must FAIL the obligation — the harness's `has_failures()` path
/// re-blocks the task as `BlockerKind::ValidationFailed`.
#[test]
fn substitution_without_decision_fails_obligation() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_result(
        root,
        serde_json::json!({
            "task_id": TASK,
            "status": "completed",
            "source_deviation": himes_deviation(),
        }),
    );
    write_per_accession_summary(root, corroborating_summary());
    // No promotion call — decisions.jsonl never gets the record.

    match source_deviation_recorded(&artifact_dir(root)) {
        ValidatorOutcome::Failed { message } => {
            assert!(
                message.contains("decisions.jsonl"),
                "the failure must name the missing audit surface; got: {message}"
            );
            assert!(
                message.contains("Bioconductor airway package"),
                "the failure must name the substitute source; got: {message}"
            );
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// Second failure arm: the deviation record exists, but the source it
/// names contradicts the package's own `per_accession_summary.json`. A
/// deposit reviewer reading the summary would conclude the data came from
/// GEO; the deviation record says otherwise. One of the two is wrong, so
/// the task may not stand.
#[test]
fn substitution_contradicting_per_accession_summary_fails_obligation() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_result(
        root,
        serde_json::json!({
            "task_id": TASK,
            "status": "completed",
            "source_deviation": himes_deviation(),
        }),
    );
    // The summary claims a GEO supplementary download — nothing to do
    // with the airway package the deviation record names.
    write_per_accession_summary(
        root,
        serde_json::json!({
            "accession": "GSE52778",
            "source": "GEO supplementary FPKM tables",
            "provenance": "Downloaded GSE52778_RAW.tar from the GEO FTP mirror."
        }),
    );
    promote_source_deviation(root, TASK, "session-under-test", &FrozenClock::default());

    match source_deviation_recorded(&artifact_dir(root)) {
        ValidatorOutcome::Failed { message } => assert!(
            message.contains("contradicts"),
            "the failure must call out the self-contradiction; got: {message}"
        ),
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// A declared deviation that does not say what was actually used is
/// unauditable and must fail rather than pass vacuously.
#[test]
fn empty_source_deviation_block_fails_obligation() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_result(
        root,
        serde_json::json!({
            "task_id": TASK,
            "source_deviation": { "reason": "something went wrong" },
        }),
    );
    match source_deviation_recorded(&artifact_dir(root)) {
        ValidatorOutcome::Failed { message } => {
            assert!(message.contains("unauditable"), "got: {message}")
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// Regression. The overwhelmingly common case is a task that read exactly
/// the source it was asked for. It must cost nothing: no decision row, no
/// `decisions.jsonl` created, and a passing obligation. A false-positive
/// here would append a spurious provenance record to every package.
#[test]
fn no_deviation_writes_no_decision() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    write_result(
        root,
        serde_json::json!({
            "task_id": TASK,
            "status": "completed",
            "summary": "Fetched GSE52778 from GEO exactly as requested.",
            "artifacts": ["cohort_manifest.tsv", "per_accession_summary.json"],
        }),
    );
    write_per_accession_summary(
        root,
        serde_json::json!({ "accession": "GSE52778", "source": "GEO", "n_samples": 8 }),
    );

    let declared =
        promote_source_deviation(root, TASK, "session-under-test", &FrozenClock::default());
    assert!(
        declared.is_none(),
        "a task that declared no substitution must report none"
    );
    assert!(
        !root.join("runtime").join("decisions.jsonl").exists(),
        "no substitution must mean no write at all — decisions.jsonl must not be created"
    );
    assert_eq!(
        source_deviation_recorded(&artifact_dir(root)),
        ValidatorOutcome::Passed,
        "no declared deviation = nothing to reconcile"
    );
}

/// A task whose `result.json` is absent soft-skips (`Errored`) rather
/// than failing: the generic missing-artifact guard owns that case, and
/// `has_failures()` counts only `Failed`, so this obligation must not
/// double-report it.
#[test]
fn missing_result_json_soft_skips() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(artifact_dir(root)).unwrap();
    assert!(matches!(
        source_deviation_recorded(&artifact_dir(root)),
        ValidatorOutcome::Errored { .. }
    ));
}
