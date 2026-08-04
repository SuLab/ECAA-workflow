//! The signed verdict ledger must never lose a row it was asked to persist.
//!
//! `serde_json` here is built WITHOUT the `float_roundtrip` feature, so its
//! number parser is not the inverse of its number printer. For some finite f64
//! values the parser lands one ULP away, and printing that neighbour yields a
//! different decimal that parses back to the original — a period-2 orbit with
//! no fixed point. A writer that signs an in-memory value and then writes a
//! *re-serialization* of it puts the MAC one parse/print step out of phase with
//! the bytes on disk, its own self-verification rejects the row, and the task
//! disappears from the signed sink while the unsigned plaintext sidecar keeps
//! the full record — the trusted artifact ends up quieter than the untrusted
//! one.
//!
//! `AuditWriter::write_signed_row` therefore signs the value a reader recovers
//! from the exact bytes it writes. These tests pin that, and pin the loud
//! failure path that must fire if a row ever fails to land anyway.

use ecaa_workflow_core::audit_writer::AuditWriter;
use ecaa_workflow_core::claim_sink::{
    persist_signed_verdicts, unpersisted_tasks, SIGNED_SINK_REL, UNPERSISTED_MARKER_REL,
};
use ecaa_workflow_core::claim_verifier::ClaimVerificationReport;
use serde_json::{json, Value};

/// `4.16e-134` is one of the two decimals in the executed `final_reporting`
/// payload whose parse/print orbit under this `serde_json` build has no fixed
/// point. Its partner `4.156217311e-134` is a fixed point, which makes the pair
/// a controlled contrast: same field, same shape, same decimal exponent.
const NO_WIRE_FIXED_POINT: &str = "4.16e-134";
const WIRE_FIXED_POINT: &str = "4.156217311e-134";

/// Reduced fixture in the shape `claim_sink::build_sink_doc` emits: the sink
/// header plus one projected verdict row, trimmed from the real
/// `final_reporting#claim-8` numeric-table verdict that could not be persisted.
fn sink_doc(claimed: Value, observed: Value) -> Value {
    json!({
        "schema_version": "1",
        "source": "runtime-verifier",
        "task_id": "final_reporting",
        "ecaa_version": "0.2",
        "min_reader_version": "0.2",
        "ablated": false,
        "n_checked": 1,
        "n_verified": 1,
        "n_mismatch": 0,
        "n_unverifiable": 0,
        "n_pending": 0,
        "n_suspicious": 0,
        "class_counts": {
            "numeric_table": 1, "directional": 0,
            "entity_presence": 0, "literature_quotation": 0,
        },
        "verifier_version": "2.0",
        "verdicts": [{
            "claim_id": "final_reporting#claim-0",
            "status": "verified",
            "class": "numeric_table",
            "entity": "SPARCL1",
            "entity_column": "entity",
            "entity_value": "SPARCL1",
            "measurement_column": "analysis_significance",
            "comparison_operator": "order_of_magnitude",
            "claimed_value": claimed,
            "observed_value": observed,
            "relative_tolerance": 1.0,
            "parse_coverage": 1.0,
            "source_table": "claims_evidence_matrix.csv",
            "supported_by": ["runtime/outputs/reporting/claims_evidence_matrix.csv"],
            "checked_against": ["runtime/outputs/reporting/claims_evidence_matrix.csv"],
            "contradicts": [],
            "attempted_sources": [],
            "text": "SPARCL1 heads the enriched ranking at padj = 4.16e-134",
            "verdict_detail": null,
            "verifier_version": "2.0",
        }],
    })
}

/// Sign `row` the way a naive writer would — over the in-memory value rather
/// than over the value recovered from the bytes it writes — and report whether
/// the result survives verification. Used only to prove these tests are
/// exercising a live hazard rather than passing vacuously.
fn naive_sign_is_self_consistent(writer: &AuditWriter, row: &Value) -> bool {
    let normalized: Value = serde_json::from_slice(&serde_json::to_vec(row).unwrap()).unwrap();
    let mac = writer.sign_row(&normalized);
    let mut signed = normalized;
    signed
        .as_object_mut()
        .unwrap()
        .insert("_mac".into(), Value::String(mac));
    let line = serde_json::to_string(&signed).unwrap();
    let reparsed: Value = serde_json::from_str(&line).unwrap();
    writer.verify_row(&reparsed).is_ok()
}

fn write_line(writer: &AuditWriter, row: &Value) -> std::io::Result<String> {
    let mut buf = Vec::new();
    writer.write_signed_row(&mut buf, row)?;
    Ok(String::from_utf8(buf).expect("signed row is UTF-8"))
}

#[test]
fn row_whose_float_has_no_wire_fixed_point_still_persists() {
    let writer = AuditWriter::for_session();
    let doc = sink_doc(
        serde_json::from_str(NO_WIRE_FIXED_POINT).unwrap(),
        serde_json::from_str(WIRE_FIXED_POINT).unwrap(),
    );

    // Vacuity guard: this fixture must still trip the naive writer, otherwise
    // the regression it pins has been neutralised elsewhere (e.g. by enabling
    // serde_json's `float_roundtrip`) and this test proves nothing.
    assert!(
        !naive_sign_is_self_consistent(&writer, &doc),
        "fixture no longer exercises the parse/print phase error — pick a value \
         whose serde_json parse/print orbit has no fixed point, or delete this test"
    );

    let line = write_line(&writer, &doc).expect("a computed verdict row must always persist");
    let parsed: Value = serde_json::from_str(line.trim_end()).expect("one JSON row per line");
    let inner = writer
        .verify_row(&parsed)
        .expect("the row a reader parses must verify against the writer's MAC");
    assert_eq!(inner["task_id"], json!("final_reporting"));
    assert_eq!(inner["verdicts"].as_array().unwrap().len(), 1);
}

#[test]
fn both_orbit_representatives_persist() {
    // The live verifier holds an f64 from Rust's exact `str::parse`; a value
    // re-read from an on-disk sidecar comes back from serde_json's parser one
    // ULP away. Both are legitimate inputs and both must persist.
    let writer = AuditWriter::for_session();
    let exact: f64 = NO_WIRE_FIXED_POINT.parse().unwrap();
    let via_serde = serde_json::from_str::<Value>(NO_WIRE_FIXED_POINT)
        .unwrap()
        .as_f64()
        .unwrap();
    assert_ne!(
        exact.to_bits(),
        via_serde.to_bits(),
        "fixture assumes serde_json's parser disagrees with Rust's exact parser"
    );
    for value in [exact, via_serde] {
        let doc = sink_doc(json!(value), json!(1.0));
        let line = write_line(&writer, &doc).expect("both representatives must persist");
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        writer.verify_row(&parsed).expect("must verify");
    }
}

#[test]
fn wire_fixed_point_row_is_unaffected() {
    // Control: a payload whose floats already have a wire fixed point signed
    // fine before and must still sign fine, byte-identically across repeats.
    let writer = AuditWriter::for_session();
    let doc = sink_doc(
        serde_json::from_str(WIRE_FIXED_POINT).unwrap(),
        serde_json::from_str("4.97e-14").unwrap(),
    );
    assert!(
        naive_sign_is_self_consistent(&writer, &doc),
        "control fixture must NOT exercise the phase error"
    );
    let first = write_line(&writer, &doc).expect("control must persist");
    let second = write_line(&writer, &doc).expect("control must persist");
    assert_eq!(first, second, "signed bytes must be reproducible");
}

#[test]
fn caller_supplied_mac_is_refused_not_signed_over() {
    // A pre-existing `_mac` would be signed as payload and then overwritten,
    // leaving the two sides covering different objects. Refuse it explicitly
    // instead of emitting a row no reader can verify.
    let writer = AuditWriter::for_session();
    let row = json!({"task_id": "t", "_mac": "deadbeef"});
    let error = write_line(&writer, &row).expect_err("must refuse a pre-signed row");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(
        error.to_string().contains("_mac"),
        "error must name the offending field: {error}"
    );
}

#[test]
fn unpersisted_row_is_recorded_durably_and_cleared_on_success() {
    // A task that verified but whose signed row could not be written must be
    // visible to a downstream reader, never silently absent from the sink.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let writer = AuditWriter::for_session();
    let sink = root.join(SIGNED_SINK_REL);

    // Force the ledger write to fail with the sink's parent dir still usable:
    // the final rename cannot replace a directory.
    std::fs::create_dir_all(&sink).unwrap();
    let report = ClaimVerificationReport::empty();
    persist_signed_verdicts(root, "task_a", &report, None, &writer)
        .expect_err("write onto a directory must fail");
    persist_signed_verdicts(root, "task_b", &report, None, &writer)
        .expect_err("write onto a directory must fail");

    assert!(
        root.join(UNPERSISTED_MARKER_REL).exists(),
        "a failed persist must leave a durable marker"
    );
    assert_eq!(
        unpersisted_tasks(root),
        vec!["task_a".to_string(), "task_b".to_string()],
        "every short task must be listed"
    );

    // Marker rows are keyed by task: re-failing the same task does not
    // duplicate it.
    persist_signed_verdicts(root, "task_a", &report, None, &writer).expect_err("still failing");
    assert_eq!(
        unpersisted_tasks(root),
        vec!["task_a".to_string(), "task_b".to_string()],
        "marker rows must be replaced per task, not appended"
    );

    // Clearing the obstruction lets task_a land; only task_a's marker goes.
    std::fs::remove_dir(&sink).unwrap();
    persist_signed_verdicts(root, "task_a", &report, None, &writer).expect("must persist now");
    assert_eq!(
        unpersisted_tasks(root),
        vec!["task_b".to_string()],
        "a successful persist clears only its own task's marker"
    );

    // Last short task recovers → the alarm file is removed entirely rather
    // than left behind as an empty file a reader has to interpret.
    persist_signed_verdicts(root, "task_b", &report, None, &writer).expect("must persist now");
    assert!(unpersisted_tasks(root).is_empty());
    assert!(
        !root.join(UNPERSISTED_MARKER_REL).exists(),
        "no task short ⇒ no alarm file"
    );
}
