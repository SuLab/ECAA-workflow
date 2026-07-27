//! An absent-value cell must not fail the literature obligations, and a table
//! that genuinely does not parse must SAY SO.
//!
//! A real deposit reported six REQUIRED contextualization obligations as
//! `failed: row 0: {"cause_kind":"literature_claim","row_index":0,
//! "artifact":"claims_evidence_matrix.csv","kind":"evidence_artifact_missing"}`.
//! Every one was a false positive with a single shared cause: the contextualize
//! step wrote `redistributable=NA` on its `not_assessed` rows, the lenient bool
//! deserializer accepted `""`/`TRUE`/`0` but not `NA`, ONE such cell failed the
//! whole CSV deserialize, and every runner mapped that parse error onto a
//! row-0 `evidence_artifact_missing` — pointing the SME at evidence files that
//! were never missing.
//!
//! Two defects, two tests:
//!   1. `na_redistributable_does_not_fail_obligations` — the NA rows parse and
//!      every obligation passes.
//!   2. `unparseable_cell_reports_table_parse_error_not_missing_artifact` — a
//!      genuinely malformed cell still fails (loudly), but as
//!      `table_parse_error` naming the row and column, not as a phantom
//!      missing artifact.

use ecaa_workflow_harness::literature_validators::{
    literature_runners, RedistributableOrMarkedRunner,
};
use ecaa_workflow_harness::validators::{ValidatorOutcome, ValidatorRunner};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

const HEADER: &str = "finding_id,entity,entity_kind,pmid,prior_pmids,concordance_flag,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified";

/// One cited, asserting row — real evidence, real quote, real PMID — so the
/// evidence-backed obligations have something to actually evaluate.
const CITED_ROW: &str = "gene_1,CRISPLD2,gene,28123456,,same_direction,CRISPLD2 expression was increased in treated samples,0,pmc_oa_full_text,sha256:aa,2026-05-14T00:00:00Z,TRUE,TRUE";

const SNAPSHOT: &str =
    "<article>CRISPLD2 expression was increased in treated samples relative to control.</article>";

const MANIFEST: &str = r#"{"schema_version":1,"entries":[{"pmid":"28123456","source_kind":"pmc_oa_full_text","path":"28123456.xml","sha256_binary":"aa","sha256_extracted_text":"cc","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":88,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}]}"#;

/// A `not_assessed` row: the producer performed no retrieval for this entity,
/// so every evidence column is empty and the two typed flags carry the
/// NA-family sentinel the producer's language spells absence with.
fn not_assessed_row(entity: &str, redistributable: &str) -> String {
    format!("finding_{entity},{entity},gene,,,not_assessed,,,,,,{redistributable},NA")
}

/// Lay down the contextualize task dir the runners expect:
/// `<root>/runtime/outputs/contextualize_findings_with_literature/` with the
/// claims matrix, the evidence manifest, and the cited snapshot on disk.
/// Returns the artifact path (the task dir) the `ValidatorRunner` receives.
fn scaffold(root: &Path, data_rows: &[String]) -> PathBuf {
    let task = root
        .join("runtime/outputs")
        .join("contextualize_findings_with_literature");
    let evidence = task.join("evidence");
    std::fs::create_dir_all(&evidence).unwrap();

    let mut csv = String::from(HEADER);
    for row in data_rows {
        csv.push('\n');
        csv.push_str(row);
    }
    csv.push('\n');
    std::fs::write(task.join("claims_evidence_matrix.csv"), csv).unwrap();
    std::fs::write(evidence.join("manifest.json"), MANIFEST).unwrap();
    std::fs::write(evidence.join("28123456.xml"), SNAPSHOT).unwrap();
    task
}

/// The obligations whose runner deserializes the whole claims table — exactly
/// the six the deposit reported as failed at row 0.
const TABLE_READING_OBLIGATIONS: &[&str] = &[
    "pmid_resolves",
    "evidence_quote_substring_match",
    "redistributable_or_marked",
    "claim_row_has_finding_id",
    "concordance_flag_in_closed_set",
    "direction_supported_by_quote",
];

/// `redistributable=NA` on `not_assessed` rows is an ABSENT value, not a broken
/// table: every literature obligation must still evaluate, and pass.
#[test]
fn na_redistributable_does_not_fail_obligations() {
    let dir = TempDir::new().unwrap();
    let rows = vec![
        CITED_ROW.to_string(),
        not_assessed_row("ACSL5", "NA"),
        not_assessed_row("SPARCL1", "NA"),
        not_assessed_row("DUSP1", "NA"),
    ];
    let artifact_path = scaffold(dir.path(), &rows);

    let mut passed: Vec<&str> = Vec::new();
    for runner in literature_runners() {
        let id = runner.obligation_id();
        match runner.run(&artifact_path) {
            ValidatorOutcome::Passed => passed.push(id),
            // A soft skip (no truth source for the cross-task gene-symbol check)
            // is not a failure and is not what this test is about.
            ValidatorOutcome::Errored { .. } | ValidatorOutcome::Unimplemented { .. } => {}
            ValidatorOutcome::Failed { message } => {
                panic!("obligation {id} must not fail on absent-value (NA) cells; got: {message}")
            }
        }
    }

    for obligation in TABLE_READING_OBLIGATIONS {
        assert!(
            passed.contains(obligation),
            "obligation {obligation} reads the claims table and must PASS on NA rows; passed set: {passed:?}"
        );
    }
}

/// Split the `row N: {json}` validator message into its row prefix and its
/// structured cause.
fn parse_message(message: &str) -> (String, serde_json::Value) {
    let brace = message
        .find('{')
        .unwrap_or_else(|| panic!("validator message must embed a JSON cause: {message}"));
    let prefix = message[..brace].trim().to_string();
    let cause: serde_json::Value = serde_json::from_str(&message[brace..])
        .unwrap_or_else(|e| panic!("cause must be JSON ({e}): {message}"));
    (prefix, cause)
}

/// A genuinely malformed cell still fails the whole table — that part is
/// deliberate, a half-parsed claims matrix would silently drop claims from
/// every gate. What must change is the HONESTY of the report: name the row and
/// the column, do not call it a missing evidence artifact.
#[test]
fn unparseable_cell_reports_table_parse_error_not_missing_artifact() {
    let dir = TempDir::new().unwrap();
    let rows = vec![
        CITED_ROW.to_string(),
        not_assessed_row("ACSL5", "NA"),
        // Third data row (0-based index 2): not absent, not a bool — malformed.
        not_assessed_row("SPARCL1", "maybe"),
    ];
    let artifact_path = scaffold(dir.path(), &rows);

    let message = match RedistributableOrMarkedRunner.run(&artifact_path) {
        ValidatorOutcome::Failed { message } => message,
        other => panic!("a malformed bool cell must still fail the obligation; got: {other:?}"),
    };

    let (prefix, cause) = parse_message(&message);
    assert_eq!(
        cause["cause_kind"], "table_parse_error",
        "an unparseable table must report itself as such, not as a claim failure: {message}"
    );
    assert_ne!(
        cause["kind"], "evidence_artifact_missing",
        "the parse error must stop masquerading as a missing evidence artifact: {message}"
    );
    assert_eq!(
        cause["row_index"], 2,
        "the cause must name the OFFENDING row (0-based data row 2), not row 0: {message}"
    );
    assert_eq!(
        prefix, "row 2:",
        "the message prefix must agree with the cause's row: {message}"
    );
    assert_eq!(
        cause["column"], "redistributable",
        "the cause must name the offending column: {message}"
    );
    assert_eq!(
        cause["artifact"], "claims_evidence_matrix.csv",
        "the cause must name the table it failed to parse: {message}"
    );
    assert!(
        cause["detail"]
            .as_str()
            .is_some_and(|d| d.contains("invalid bool literal")),
        "the cause must carry the underlying parse detail: {message}"
    );
}
