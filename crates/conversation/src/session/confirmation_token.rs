//! Per-emit confirmation token. Replaces `user_confirmed: bool`.
//!
//! A `ConfirmationToken` is bound to a specific `(emission_id,
//! summary_hash)` pair. `emit_package` succeeds only if the persisted
//! token matches the pending emission_id AND the current summary hash.
//! Every state transition that changes the emit shape (amend, branch,
//! sensitivity, certain Blocked variants) clears the token, forcing
//! re-confirmation.
//!
//! ### Representation choices
//!
//! - `summary_hash` is stored as **`String` (hex SHA-256)** rather than
//!   `[u8; 32]` to match the existing
//!   [`crate::session::ConfirmationCard::summary_hash`] shape (also
//!   hex), so cards on the conversation tail compare directly to the
//!   token without per-call hex-encode/decode. The plan's `[u8; 32]`
//!   choice would have cascaded into the card type, the
//!   `DecisionType::Confirm { summary_hash: Option<String> }` audit
//!   record, and the UI's TS binding — all of which are already
//!   hex-string today.
//! - `granted_by` carries an [`AuditActor`] (a conversation-local mirror
//!   of `server::auth::principal::AuditActor`) so the token is bound to
//!   a specific identity (closes P1-224's general pattern) without
//!   pulling the server crate into the conversation dep arrow.

use crate::audit_actor::AuditActor;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// Per-emit latch token that binds the SME's `/confirm` click to a
/// specific pending emission. Prevents the LLM from calling
/// `emit_package` again after the SME confirmes plan A if the plan
/// was subsequently amended (plan B would need a fresh confirm).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ConfirmationToken {
    /// The pending emission this token authorizes. Must match
    /// `Session::pending_emission_id` at emit-time or the precondition
    /// fails (forces re-confirmation).
    #[ts(type = "string")]
    pub emission_id: Uuid,

    /// Hex SHA-256 of the SME-confirmed plan summary. Must match the
    /// current summary hash at emit-time; drift indicates the plan was
    /// amended after confirmation. Encoded as a lowercase 64-char hex
    /// string to match [`crate::session::ConfirmationCard::summary_hash`].
    pub summary_hash: String,

    /// When the SME's /confirm arrived server-side.
    #[ts(type = "string")]
    pub granted_at: DateTime<Utc>,

    /// Who granted (always derived from RequestPrincipal — never from
    /// request body, per C1 / closes P1-224). ts-rs skip because
    /// AuditActor is a Rust-shape enum that doesn't need to round-trip
    /// to the UI today; the UI consumes the token only for its
    /// existence (the "is confirmed?" check) and the audit log carries
    /// actor identity separately.
    #[ts(skip)]
    pub granted_by: AuditActor,

    /// Single-use latch. Set to `true` by `consume()` when
    /// `emit_package` succeeds. A consumed token causes `authorizes()`
    /// to return `false`, so a second `emit_package` call with the
    /// same token is rejected — the SME must re-confirm before
    /// re-emitting (e.g. after a cancelled + retried emit).
    ///
    /// `#[serde(default)]` keeps existing on-disk tokens (which have no
    /// `consumed` key) deserializing as `consumed = false`, preserving
    /// backward compatibility.
    #[serde(default)]
    pub consumed: bool,
}

impl ConfirmationToken {
    /// Construct a fresh token. `summary_hash` must be the lowercase hex
    /// SHA-256 of the `ConfirmationCard::summary_markdown` text the SME
    /// saw at click time.
    pub fn new(
        emission_id: Uuid,
        summary_hash: impl Into<String>,
        granted_at: DateTime<Utc>,
        granted_by: AuditActor,
    ) -> Self {
        Self {
            emission_id,
            summary_hash: summary_hash.into(),
            granted_at,
            granted_by,
            consumed: false,
        }
    }

    /// True iff this token authorizes the given pending emission and
    /// summary. Both must match AND the token must not have been
    /// consumed by a prior successful emit. Returns `false` on any
    /// mismatch or if already consumed so the caller can return
    /// `PreconditionFailure` without leaking which condition failed
    /// (the SME re-confirms either way).
    pub fn authorizes(&self, pending_emission: Uuid, current_summary: &str) -> bool {
        !self.consumed
            && self.emission_id == pending_emission
            && self.summary_hash == current_summary
    }

    /// Mark the token as consumed. Called by `emit_package_post_ok`
    /// after a successful emit so any replay of `emit_package` with
    /// the same token fails the precondition. A consumed token cannot
    /// be un-consumed; the SME must click Confirm again to mint a new
    /// token.
    pub fn consume(&mut self) {
        self.consumed = true;
    }

    /// True iff this token has already been used to authorize a
    /// successful emit.
    pub fn is_consumed(&self) -> bool {
        self.consumed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit_actor::AuditActor;
    use chrono::Utc;
    use uuid::Uuid;

    fn make_token(emission_id: Uuid, summary: &str) -> ConfirmationToken {
        ConfirmationToken::new(emission_id, summary, Utc::now(), AuditActor::System)
    }

    #[test]
    fn fresh_token_authorizes() {
        let id = Uuid::new_v4();
        let tok = make_token(id, "abc");
        assert!(tok.authorizes(id, "abc"));
        assert!(!tok.is_consumed());
    }

    #[test]
    fn consumed_token_does_not_authorize() {
        let id = Uuid::new_v4();
        let mut tok = make_token(id, "abc");
        tok.consume();
        assert!(tok.is_consumed());
        // Replay attempt with same (emission_id, summary) must fail.
        assert!(
            !tok.authorizes(id, "abc"),
            "consumed token must not authorize"
        );
    }

    #[test]
    fn token_rejects_wrong_summary() {
        let id = Uuid::new_v4();
        let tok = make_token(id, "abc");
        assert!(!tok.authorizes(id, "different"));
    }

    #[test]
    fn token_rejects_wrong_emission_id() {
        let id = Uuid::new_v4();
        let tok = make_token(id, "abc");
        assert!(!tok.authorizes(Uuid::new_v4(), "abc"));
    }

    #[test]
    fn serde_roundtrip_preserves_consumed() {
        let id = Uuid::new_v4();
        let mut tok = make_token(id, "abc");
        tok.consume();
        let json = serde_json::to_string(&tok).unwrap();
        let deser: ConfirmationToken = serde_json::from_str(&json).unwrap();
        assert!(deser.is_consumed());
        assert!(!deser.authorizes(id, "abc"));
    }

    #[test]
    fn legacy_token_without_consumed_field_defaults_false() {
        // Simulates loading a pre-E4 session JSON that has no `consumed` key.
        let id = Uuid::new_v4();
        let json = serde_json::json!({
            "emission_id": id,
            "summary_hash": "abc",
            "granted_at": "2026-05-16T00:00:00Z",
            "granted_by": "System",
        });
        let tok: ConfirmationToken = serde_json::from_value(json).unwrap();
        assert!(
            !tok.is_consumed(),
            "legacy token must default consumed=false"
        );
        assert!(tok.authorizes(id, "abc"));
    }
}
