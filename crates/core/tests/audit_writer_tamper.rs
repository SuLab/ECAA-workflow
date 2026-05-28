//! Integration tests for the audit-writer tamper-rejection contract.

use scripps_workflow_core::audit_writer::{AuditError, AuditWriter};

#[test]
fn agent_forged_row_rejected_at_read() {
    let writer = AuditWriter::for_session();
    let secret = writer.secret();

    // Server writes a legitimate row.
    let real = serde_json::json!({
        "timestamp": "2026-05-16T00:00:00Z",
        "decision": {"kind": "confirm"},
        "actor": {"user": "alice"},
    });
    let mut buf = Vec::new();
    writer.write_signed_row(&mut buf, &real).unwrap();

    // Agent (post-emit, with RW on package via bwrap bind) writes a
    // forged row without _mac (or with bogus _mac).
    let forged = serde_json::json!({
        "timestamp": "2026-05-16T00:01:00Z",
        "decision": {"kind": "select_sensitivity_winner", "winner": "evil"},
        "actor": {"user": "ceo"},
    });
    buf.extend_from_slice(serde_json::to_string(&forged).unwrap().as_bytes());
    buf.extend_from_slice(b"\n");

    // Server re-reads the file post-restart using the persisted secret.
    let reader = AuditWriter::with_secret(secret);
    let mut accepted = 0;
    let mut rejected = 0;
    for line in std::str::from_utf8(&buf).unwrap().lines() {
        let row: serde_json::Value = serde_json::from_str(line).unwrap();
        match reader.verify_row(&row) {
            Ok(_) => accepted += 1,
            Err(AuditError::MissingMac | AuditError::MacMismatch) => rejected += 1,
            Err(e) => panic!("unexpected error: {e}"),
        }
    }
    assert_eq!(accepted, 1, "exactly the server-written row should verify");
    assert_eq!(rejected, 1, "the forged row should be rejected");
}
