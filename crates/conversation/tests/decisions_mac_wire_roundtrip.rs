//! `runtime/decisions.jsonl.mac` must verify against the BYTES of
//! `runtime/decisions.jsonl`.
//!
//! The pair is documented (`crates/core/src/emitter/bagit.rs`) as "a keyed HMAC
//! over `decisions.jsonl`; verified with the session secret, NOT by re-hashing
//! into the payload manifest". The only reader that contract can describe is one
//! that parses a stored line and recomputes `AuditWriter::sign_row` over it — so
//! the signed domain has to be the value recovered from the stored bytes.
//!
//! `serde_json` here is built WITHOUT `float_roundtrip`, so its number parser is
//! not the inverse of its number printer: for some finite f64 values the parser
//! lands one ULP away, and printing that neighbour yields a *different* decimal
//! that parses back to the original — a period-2 orbit with no fixed point. A
//! sidecar writer that signs `to_value(rec)` while the JSONL carries
//! `to_string(rec)` is therefore one parse/print step out of phase with the
//! bytes it is supposed to authenticate, and every reader of that row reports a
//! false tamper on the audit trail.
//!
//! These tests pin the wire-domain contract end-to-end through a real emit, and
//! carry vacuity guards so they cannot pass by accident (e.g. if
//! `float_roundtrip` is later enabled, the guards say so out loud instead of
//! going quietly green).

use ecaa_workflow_conversation::emit::emit_with_conversation_log;
use ecaa_workflow_conversation::session::Session;
use ecaa_workflow_conversation::tools::{dispatch_one, BatchableTool, Tool, ToolContext};
use ecaa_workflow_core::audit_writer::AuditWriter;
use ecaa_workflow_core::decision_log::{DecisionActor, DecisionRecord, DecisionType};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// `4.16e-134` has no fixed point under this `serde_json` build's
/// parse/print orbit; `4.156217311e-134` — same field shape, same decimal
/// exponent — does. The pair is the controlled contrast used by the sibling
/// core test `crates/core/tests/signed_row_wire_roundtrip.rs`.
const NO_WIRE_FIXED_POINT: f64 = 4.16e-134;
const WIRE_FIXED_POINT: f64 = 4.156217311e-134;

fn config_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

/// The value a reader recovers from the bytes `serde_json` writes for `v`.
fn wire_view<T: serde::Serialize>(v: &T) -> Value {
    serde_json::from_str(&serde_json::to_string(v).expect("serializable")).expect("re-parsable")
}

/// True when `f` survives one serde_json print → parse cycle bit-for-bit.
fn is_wire_fixed_point(f: f64) -> bool {
    let printed = serde_json::to_string(&f).expect("float serializes");
    let reparsed: f64 = serde_json::from_str(&printed).expect("float re-parses");
    reparsed.to_bits() == f.to_bits()
}

/// Reachability, measured rather than argued from the shape of the type.
///
/// `DecisionType::CrossVersionDiff.overall_concordance` is a concordance RATIO —
/// exactly the population of `k / n` quotients scanned here. The hazard is NOT
/// confined to exotic magnitudes: at the time of writing 18801 of 180898 such
/// ratios (10.39%) have no serde_json wire fixed point, the smallest denominator
/// being 11 — so an ordinary "10 of 11 rows concordant" amend/branch re-emission
/// reaches it with no adversary involved.
#[test]
fn concordance_ratios_reach_the_non_fixed_point_population() {
    let mut offenders: Vec<(u32, u32)> = Vec::new();
    let mut total = 0u32;
    for n in 2u32..=600 {
        for k in 0u32..=n {
            total += 1;
            if !is_wire_fixed_point(f64::from(k) / f64::from(n)) {
                offenders.push((k, n));
            }
        }
    }
    let percent = 100.0 * offenders.len() as f64 / f64::from(total);
    assert!(
        percent > 1.0,
        "only {}/{total} ({percent:.2}%) k/n concordance ratios fail a serde_json \
         print/parse cycle — the reachability premise of the wire-domain contract has \
         changed (float_roundtrip enabled?); re-derive this test before trusting it",
        offenders.len()
    );
    // Concrete mundane witness: 10/11 = 0.9090909090909091, a concordance value
    // an SME could read off a 11-row cross-version diff.
    assert!(
        offenders.contains(&(10, 11)),
        "expected 10/11 among the non-fixed-point ratios; serde_json's parser \
         behaviour has changed and this test's witness needs re-deriving"
    );
}

/// Vacuity guard for the fixtures the emit test below uses: the "no fixed
/// point" constant must really lack one, and the control must really have one.
#[test]
fn fixture_constants_bracket_the_hazard() {
    assert!(
        !is_wire_fixed_point(NO_WIRE_FIXED_POINT),
        "fixture no longer exercises the parse/print phase error — pick a value whose \
         serde_json parse/print orbit has no fixed point, or delete these tests"
    );
    assert!(
        is_wire_fixed_point(WIRE_FIXED_POINT),
        "control fixture must NOT exercise the phase error"
    );
}

/// Sign the way a writer that signs the IN-MEMORY value would, and report
/// whether a reader parsing the stored line would accept it. Used only to prove
/// the tests below exercise a live hazard rather than passing vacuously.
fn naive_sign_matches_wire(writer: &AuditWriter, rec: &DecisionRecord) -> bool {
    let in_memory = serde_json::to_value(rec).expect("record serializes");
    let from_wire = wire_view(rec);
    writer.sign_row(&in_memory) == writer.sign_row(&from_wire)
}

fn cross_version_diff_record(concordance: f64) -> DecisionRecord {
    DecisionRecord::new(
        "11111111-1111-1111-1111-111111111111",
        DecisionType::CrossVersionDiff {
            parent_package: "/tmp/pkg-v1".into(),
            child_package: "/tmp/pkg-v2".into(),
            overall_concordance: concordance,
            n_discordant: 0,
        },
        DecisionActor::Harness,
        None,
    )
}

async fn boot_session() -> Session {
    let mut session = Session::test_fixture_with_dag();
    let ctx = ToolContext::new(config_dir(), "claude-sonnet-5");
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq from human IVD samples comparing degenerated and healthy"
                .into(),
        }),
        &mut session,
        &ctx,
    )
    .await;
    session
}

/// Read the emitted pair and verify every MAC line the way the only reader the
/// documented contract can describe does: parse the stored JSONL line, recompute
/// `sign_row` over that parsed value, compare.
fn verify_pair_against_stored_bytes(pkg: &Path, secret: [u8; 32]) -> usize {
    let jsonl = std::fs::read_to_string(pkg.join("runtime/decisions.jsonl"))
        .expect("runtime/decisions.jsonl present after emit");
    let macs = std::fs::read_to_string(pkg.join("runtime/decisions.jsonl.mac"))
        .expect("runtime/decisions.jsonl.mac present after emit");
    let rows: Vec<&str> = jsonl.lines().filter(|l| !l.trim().is_empty()).collect();
    let mac_lines: Vec<&str> = macs.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(
        rows.len(),
        mac_lines.len(),
        "the sidecar must carry exactly one MAC per stored decision row"
    );
    let verifier = AuditWriter::with_secret(secret);
    for (i, (line, mac)) in rows.iter().zip(mac_lines.iter()).enumerate() {
        let parsed: Value = serde_json::from_str(line).expect("one JSON object per JSONL line");
        assert_eq!(
            &verifier.sign_row(&parsed),
            mac,
            "decisions.jsonl.mac line {i} does not authenticate the bytes stored on \
             line {i} of decisions.jsonl — a holder of the session secret sees a \
             false tamper on the audit trail"
        );
    }
    rows.len()
}

/// A decision record whose float has no wire fixed point must still produce a
/// sidecar that authenticates the stored bytes.
#[tokio::test]
async fn decisions_mac_authenticates_stored_bytes_for_float_without_wire_fixed_point() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("pkg");
    let mut session = boot_session().await;

    let record = cross_version_diff_record(NO_WIRE_FIXED_POINT);
    // Vacuity guard: an in-memory signer must actually disagree with the wire
    // for THIS record, otherwise the test proves nothing about the fix.
    let probe = AuditWriter::with_secret(session.audit_writer_secret);
    assert!(
        !naive_sign_matches_wire(&probe, &record),
        "fixture record no longer exercises the phase error — the MAC over the \
         in-memory value now equals the MAC over the wire-recovered value"
    );
    session.decisions.push(record);

    emit_with_conversation_log(&mut session, &pkg, &config_dir())
        .await
        .expect("emit succeeded");

    let n = verify_pair_against_stored_bytes(&pkg, session.audit_writer_secret);
    assert!(n >= 1, "the fixture decision must be present in the ledger");
    let jsonl = std::fs::read_to_string(pkg.join("runtime/decisions.jsonl")).unwrap();
    assert!(
        jsonl.contains("cross_version_diff"),
        "the float-bearing record must not be dropped from the ledger: {jsonl}"
    );
}

/// Control: a record whose float DOES have a wire fixed point verified before
/// the fix and must still verify, so the fix is a strict extension rather than a
/// change of the signed domain for already-working payloads.
#[tokio::test]
async fn decisions_mac_control_float_with_wire_fixed_point_still_verifies() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("pkg");
    let mut session = boot_session().await;

    let record = cross_version_diff_record(WIRE_FIXED_POINT);
    let probe = AuditWriter::with_secret(session.audit_writer_secret);
    assert!(
        naive_sign_matches_wire(&probe, &record),
        "control record must NOT exercise the phase error"
    );
    session.decisions.push(record);

    emit_with_conversation_log(&mut session, &pkg, &config_dir())
        .await
        .expect("emit succeeded");
    verify_pair_against_stored_bytes(&pkg, session.audit_writer_secret);
}

/// Tamper detection must survive the fix: flipping a byte of a stored decision
/// row still fails verification against the unchanged sidecar. Signing the
/// wire-recovered value must not be mistaken for signing a rounded or
/// normalized value — the MAC stays sensitive to the low bits.
#[tokio::test]
async fn tampering_with_a_stored_decision_row_is_still_detected() {
    let dir = tempfile::tempdir().unwrap();
    let pkg = dir.path().join("pkg");
    let mut session = boot_session().await;
    session
        .decisions
        .push(cross_version_diff_record(NO_WIRE_FIXED_POINT));
    emit_with_conversation_log(&mut session, &pkg, &config_dir())
        .await
        .expect("emit succeeded");

    let jsonl_path = pkg.join("runtime/decisions.jsonl");
    let jsonl = std::fs::read_to_string(&jsonl_path).unwrap();
    let macs = std::fs::read_to_string(pkg.join("runtime/decisions.jsonl.mac")).unwrap();
    let verifier = AuditWriter::with_secret(session.audit_writer_secret);

    let target = jsonl
        .lines()
        .position(|l| l.contains("cross_version_diff"))
        .expect("the float-bearing row is on disk");
    let mut parsed: Value = serde_json::from_str(jsonl.lines().nth(target).unwrap()).unwrap();
    // One-ULP tamper: the smallest possible edit to the signed float. A MAC that
    // had been blinded by rounding-for-signing would accept this.
    let tampered = f64::from_bits(NO_WIRE_FIXED_POINT.to_bits() + 1);
    parsed["decision"]["overall_concordance"] = serde_json::json!(tampered);
    let stored_mac = macs.lines().nth(target).unwrap();
    assert_ne!(
        verifier.sign_row(&parsed),
        stored_mac,
        "a one-ULP edit to a signed float must break its MAC"
    );
}
