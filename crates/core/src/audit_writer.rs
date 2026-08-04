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

/// Row member carrying the hex HMAC. Excluded from the signed payload: the
/// writer appends it after signing, the verifier strips it before recomputing.
const MAC_FIELD: &str = "_mac";

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
    ///
    /// The signed domain is the value as a READER RECOVERS IT FROM THE WIRE,
    /// never an in-memory value that has yet to cross the serialization
    /// boundary. `serde_json`'s number parser is not the exact inverse of its
    /// number printer (the `float_roundtrip` feature is not enabled), so for
    /// some values `from_str(to_string(v)) != v`. Callers that sign a value
    /// they are about to serialize must therefore sign
    /// `from_str(&to_string(v))`, as [`AuditWriter::write_signed_row`] does.
    pub fn sign_row(&self, row: &serde_json::Value) -> String {
        let canonical = canonical_json(row);
        let mut mac = HmacSha256::new_from_slice(&self.secret).expect("32-byte secret");
        mac.update(canonical.as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }

    /// Write `row` to `writer` as a signed JSONL line. The row is
    /// extended with a `_mac` field containing the hex HMAC.
    ///
    /// # Invariant
    ///
    /// **The MAC covers the value a reader recovers from the exact bytes this
    /// call writes.** The row is serialized ONCE; that serialization is the
    /// wire contract, and the signed payload is the value parsed back out of
    /// it. [`AuditWriter::verify_row`] recomputes over exactly the same
    /// wire-recovered value, so signing and verification cannot disagree.
    ///
    /// # Failure this prevents
    ///
    /// `serde_json` is built without the `float_roundtrip` feature, so its
    /// number parser is not the inverse of its number printer: for some finite
    /// f64 values the parser lands one ULP away, and printing that neighbour
    /// yields a *different* decimal that parses back to the original — a
    /// period-2 orbit with no fixed point (e.g. `4.16e-134` ⇄
    /// `4.1599999999999996e-134`). Signing an in-memory value and then writing
    /// a re-serialization of it therefore put the MAC one parse/print step out
    /// of phase with the bytes on disk, and the self-check below rejected the
    /// row — silently truncating the signed verdict ledger for the affected
    /// task while the unsigned plaintext sidecar kept the full record. Signing
    /// the wire-recovered value removes the phase error entirely, for any
    /// float, at any magnitude, without weakening the self-check.
    pub fn write_signed_row<W: std::io::Write>(
        &self,
        writer: &mut W,
        row: &serde_json::Value,
    ) -> std::io::Result<()> {
        if !row.is_object() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "row must be a JSON object",
            ));
        }
        // A caller-supplied `_mac` would be signed as payload and then
        // overwritten by the real signature, so the two sides would cover
        // different objects. Refuse instead of producing an unverifiable row.
        if row.get(MAC_FIELD).is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("row must not already carry a `{MAC_FIELD}` field"),
            ));
        }
        let mut signed = row.clone();
        // The wire bytes are the contract. Serialize the body once, then sign
        // the value recovered from those bytes. Adding `_mac` cannot perturb
        // any sibling member's rendering (each member is serialized
        // independently), so the reader's `_mac`-stripped view is exactly
        // `wire_view`.
        let body = serde_json::to_string(&signed)?;
        let wire_view: serde_json::Value = serde_json::from_str(&body)?;
        let mac = self.sign_row(&wire_view);
        signed
            .as_object_mut()
            .expect("row verified to be an object above")
            .insert(MAC_FIELD.into(), serde_json::Value::String(mac));
        let line = serde_json::to_string(&signed)?;
        // Refuse to persist a row that this same writer cannot recover and
        // verify from its serialized form. The signed verdict ledger is a
        // trust boundary; failing this write is safer than atomically
        // replacing a previously valid ledger with an unverifiable row.
        let reparsed: serde_json::Value = serde_json::from_str(&line)?;
        self.verify_row(&reparsed).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("serialized signed row failed self-verification: {error}"),
            )
        })?;
        writeln!(writer, "{line}")?;
        Ok(())
    }

    /// Verify a signed row. Returns the row with `_mac` stripped iff
    /// the HMAC validates; otherwise [`AuditError`].
    ///
    /// `signed_row` MUST be the value parsed directly from the stored line.
    /// Passing a value that has been round-tripped through an extra
    /// serialize/parse cycle can shift a float onto its parser-neighbour (see
    /// [`AuditWriter::write_signed_row`]) and report a false tamper.
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
            .remove(MAC_FIELD)
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
    fn signed_row_self_verifies_after_json_normalization() {
        let writer = AuditWriter::for_session();
        let row = serde_json::json!({
            "task_id": "arbitrary_analysis",
            "measurements": [
                -0.0,
                f64::MIN_POSITIVE,
                3.32215426150238e-19,
                2.9370788852442,
                u64::MAX,
            ],
            "nested": {
                "β": ["μm", "Δ", "arbitrary modality"],
                "counts": {"checked": 17, "verified": 13},
            },
        });

        let mut buf = Vec::new();
        writer.write_signed_row(&mut buf, &row).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_slice(buf.strip_suffix(b"\n").unwrap()).unwrap();
        let verified = writer.verify_row(&parsed).unwrap();

        // The writer's normalized value is the value a reader reconstructs.
        let normalized: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&row).unwrap()).unwrap();
        assert_eq!(verified, normalized);
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
