//! HMAC-signed audit-log rows.
//!
//! The agent process runs inside a bwrap sandbox bound to the package
//! root (narrowed by the companion change to scripts/agent-claude.sh).
//! For defense-in-depth, every server-written audit sidecar carries an
//! HMAC-SHA256 row signature over canonical JSON; readers reject rows
//! whose `_mac` field doesn't validate.
//!
//! Per-session secret is regenerated on every emit so agent-written
//! rows from a prior emit cannot validate against the new emit's
//! secret. Secret persists in `session.audit_writer_secret`.

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

/// Per-session HMAC writer/verifier for audit-log rows.
#[derive(Clone)]
pub struct AuditWriter {
    secret: [u8; 32],
}

impl AuditWriter {
    /// Generate a fresh writer with cryptographically-random secret.
    /// Call once per emit; persist `secret` to session state so the
    /// verifier can be reconstructed at read time.
    pub fn for_session() -> Self {
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        AuditWriter { secret }
    }

    /// Reconstruct from a previously-generated secret.
    pub fn with_secret(secret: [u8; 32]) -> Self {
        AuditWriter { secret }
    }

    /// Inspector: returns the 32-byte secret (for persistence into
    /// `session.audit_writer_secret`).
    pub fn secret(&self) -> [u8; 32] {
        self.secret
    }

    /// Sign a JSON value. Output is hex-encoded HMAC-SHA256 over the
    /// canonical JSON representation (sorted keys, no whitespace).
    pub fn sign_row(&self, row: &serde_json::Value) -> String {
        let canonical = canonical_json(row);
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("32-byte secret");
        mac.update(canonical.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Write `row` to `writer` as a signed JSONL line. The row is
    /// extended with a `_mac` field containing the hex HMAC.
    pub fn write_signed_row<W: std::io::Write>(
        &self,
        writer: &mut W,
        row: &serde_json::Value,
    ) -> std::io::Result<()> {
        let mac = self.sign_row(row);
        let mut signed = row.clone();
        if let Some(obj) = signed.as_object_mut() {
            obj.insert("_mac".into(), serde_json::Value::String(mac));
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "row must be a JSON object",
            ));
        }
        let line = serde_json::to_string(&signed)?;
        writeln!(writer, "{line}")?;
        Ok(())
    }

    /// Verify a signed row. Returns the row with `_mac` stripped iff
    /// the HMAC validates; otherwise [`AuditError`].
    ///
    /// Every rejection path emits a `target = "ecaa::audit_tamper"`
    /// warn-level event so operators can alert on non-zero rates via
    /// log scrapers. The conceptual metrics counter is
    /// `ecaa_audit_tamper_total`; the structured-log channel is the
    /// source of truth until a metrics framework is wired in.
    pub fn verify_row(
        &self,
        signed_row: &serde_json::Value,
    ) -> Result<serde_json::Value, AuditError> {
        let mut obj = match signed_row.as_object().cloned() {
            Some(obj) => obj,
            None => {
                tracing::warn!(
                    target: "ecaa::audit_tamper",
                    rejection = "not_an_object",
                    "audit-log row failed HMAC verification: payload is not a JSON object"
                );
                return Err(AuditError::NotAnObject);
            }
        };
        let presented_mac = match obj
            .remove("_mac")
            .and_then(|v| v.as_str().map(String::from))
        {
            Some(mac) => mac,
            None => {
                tracing::warn!(
                    target: "ecaa::audit_tamper",
                    rejection = "missing_mac",
                    "audit-log row failed HMAC verification: _mac field absent"
                );
                return Err(AuditError::MissingMac);
            }
        };
        let inner = serde_json::Value::Object(obj);
        let expected_mac = self.sign_row(&inner);
        // Constant-time compare via subtle.
        use subtle::ConstantTimeEq;
        if presented_mac
            .as_bytes()
            .ct_eq(expected_mac.as_bytes())
            .into()
        {
            Ok(inner)
        } else {
            tracing::warn!(
                target: "ecaa::audit_tamper",
                rejection = "mac_mismatch",
                "audit-log row failed HMAC verification: \
                 row may have been tampered or written by an unauthorized writer"
            );
            Err(AuditError::MacMismatch)
        }
    }
}

impl std::fmt::Debug for AuditWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditWriter")
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Error)]
/// AuditError discriminant.
pub enum AuditError {
    #[error("row is not a JSON object")]
    /// NotAnObject variant.
    NotAnObject,
    #[error("row missing _mac field")]
    /// MissingMac variant.
    MissingMac,
    #[error("HMAC mismatch — row may have been tampered or written by an unauthorized writer")]
    /// MacMismatch variant.
    MacMismatch,
}

/// Canonical JSON: BTreeMap (sorted keys), no whitespace. Deterministic
/// across runs and platforms. Required so two writers with the same
/// secret produce identical HMAC for identical logical content.
fn canonical_json(v: &serde_json::Value) -> String {
    let canonical = sort_keys(v.clone());
    serde_json::to_string(&canonical).expect("serializable")
}

fn sort_keys(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let sorted: std::collections::BTreeMap<String, serde_json::Value> =
                map.into_iter().map(|(k, v)| (k, sort_keys(v))).collect();
            serde_json::to_value(sorted).expect("BTreeMap serializable")
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(sort_keys).collect())
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_sign_verify() {
        let writer = AuditWriter::for_session();
        let row =
            serde_json::json!({"kind": "Confirm", "user": "alice", "ts": "2026-05-16T12:00:00Z"});
        let mac = writer.sign_row(&row);

        let mut buf = Vec::new();
        writer.write_signed_row(&mut buf, &row).unwrap();
        let line = std::str::from_utf8(&buf).unwrap().trim_end();
        let parsed: serde_json::Value = serde_json::from_str(line).unwrap();

        let verified = writer.verify_row(&parsed).unwrap();
        assert_eq!(verified, row);

        // The mac in the line should match what sign_row returned.
        assert!(line.contains(&mac));
    }

    #[test]
    fn tampered_row_rejected() {
        let writer = AuditWriter::for_session();
        let row = serde_json::json!({"kind": "Confirm", "user": "alice"});

        let mut buf = Vec::new();
        writer.write_signed_row(&mut buf, &row).unwrap();
        let mut parsed: serde_json::Value =
            serde_json::from_str(std::str::from_utf8(&buf).unwrap().trim_end()).unwrap();

        // Tamper: change user field after signing.
        parsed
            .as_object_mut()
            .unwrap()
            .insert("user".into(), serde_json::Value::String("evil".into()));

        assert!(matches!(
            writer.verify_row(&parsed),
            Err(AuditError::MacMismatch)
        ));
    }

    #[test]
    fn unsigned_row_rejected() {
        let writer = AuditWriter::for_session();
        let row = serde_json::json!({"kind": "Confirm", "user": "alice"});
        assert!(matches!(
            writer.verify_row(&row),
            Err(AuditError::MissingMac)
        ));
    }

    #[test]
    fn cross_secret_rejected() {
        let writer_a = AuditWriter::for_session();
        let writer_b = AuditWriter::for_session();
        let row = serde_json::json!({"kind": "Confirm"});

        let mut buf = Vec::new();
        writer_a.write_signed_row(&mut buf, &row).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(std::str::from_utf8(&buf).unwrap().trim_end()).unwrap();

        // Writer A's signature should not verify against writer B.
        assert!(matches!(
            writer_b.verify_row(&parsed),
            Err(AuditError::MacMismatch)
        ));
    }

    #[test]
    fn canonical_json_sorts_keys() {
        let v1 = serde_json::json!({"b": 1, "a": 2});
        let v2 = serde_json::json!({"a": 2, "b": 1});
        assert_eq!(canonical_json(&v1), canonical_json(&v2));
    }

    #[test]
    fn canonical_json_recurses_into_nested_objects() {
        let v1 = serde_json::json!({"outer": {"z": 1, "a": 2}});
        let v2 = serde_json::json!({"outer": {"a": 2, "z": 1}});
        assert_eq!(canonical_json(&v1), canonical_json(&v2));
    }

    #[test]
    fn debug_redacts_secret() {
        let writer = AuditWriter::with_secret([0x42; 32]);
        let dbg = format!("{writer:?}");
        assert!(!dbg.contains("66"), "secret leaked via Debug");
        assert!(dbg.contains("redacted"));
    }

    #[test]
    fn reconstruct_from_secret_verifies_prior_writes() {
        let writer_a = AuditWriter::for_session();
        let secret = writer_a.secret();
        let row = serde_json::json!({"kind": "Confirm"});

        let mut buf = Vec::new();
        writer_a.write_signed_row(&mut buf, &row).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(std::str::from_utf8(&buf).unwrap().trim_end()).unwrap();

        // Reconstruct writer from secret — verifies same rows.
        let writer_b = AuditWriter::with_secret(secret);
        assert_eq!(writer_b.verify_row(&parsed).unwrap(), row);
    }
}
