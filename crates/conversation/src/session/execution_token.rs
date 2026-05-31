//! Per-button execution token. Mirrors [`ConfirmationToken`]: a
//! single-use latch minted ONLY by the REST `/start-execution` button,
//! verified + consumed at `start_execution` dispatch so an autonomous
//! LLM cannot kick off a harness run without a human press.
//!
//! ### Representation choices
//!
//! Unlike [`ConfirmationToken`] there is no `summary_hash` — execution
//! does not re-hash the plan shape. The token's binding is the
//! emission/run id (`Session::pending_emission_id`), which is the same
//! UUID the confirmation token bound to and which a fresh emit cycle
//! re-mints. `granted_by` carries an [`AuditActor`] so the press is
//! bound to a specific identity without pulling the server crate into
//! the conversation dep arrow.
//!
//! [`ConfirmationToken`]: crate::session::ConfirmationToken

use crate::audit_actor::AuditActor;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

/// Single-use latch authorizing exactly one `start_execution`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ExecutionToken {
    /// The emission/run this token authorizes. Bound at mint time to the
    /// session's current `pending_emission_id`.
    #[ts(type = "string")]
    pub request_id: Uuid,

    /// When the SME pressed the Start button server-side.
    #[ts(type = "string")]
    pub granted_at: DateTime<Utc>,

    /// Who pressed it (derived from RequestPrincipal, never the body).
    /// ts-rs skip because `AuditActor` is a Rust-shape enum that doesn't
    /// need to round-trip to the UI today — the UI consumes the token
    /// only for its existence (the "is execution authorized?" check).
    #[ts(skip)]
    pub granted_by: AuditActor,

    /// Single-use latch; `consume()` sets it and `authorizes()` then
    /// returns false. `#[serde(default)]` keeps legacy on-disk tokens
    /// (which have no `consumed` key) loadable as `consumed = false`.
    #[serde(default)]
    pub consumed: bool,
}

impl ExecutionToken {
    /// Construct a fresh token bound to `request_id` (the session's
    /// current `pending_emission_id`).
    pub fn new(request_id: Uuid, granted_at: DateTime<Utc>, granted_by: AuditActor) -> Self {
        Self {
            request_id,
            granted_at,
            granted_by,
            consumed: false,
        }
    }

    /// True iff unconsumed AND bound to `request`. Returns `false` on
    /// any mismatch or if already consumed so a replayed
    /// `start_execution` fails the precondition (the SME re-presses
    /// Start to mint a new token).
    pub fn authorizes(&self, request: Uuid) -> bool {
        !self.consumed && self.request_id == request
    }

    /// Mark the token as consumed. Called after a successful
    /// `start_execution` dispatch so any replay fails the precondition.
    pub fn consume(&mut self) {
        self.consumed = true;
    }

    /// True iff this token has already authorized a successful dispatch.
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

    fn tok(req: Uuid) -> ExecutionToken {
        ExecutionToken::new(req, Utc::now(), AuditActor::System)
    }

    #[test]
    fn authorizes_matching_unconsumed_request() {
        let r = Uuid::new_v4();
        let t = tok(r);
        assert!(t.authorizes(r));
        assert!(!t.is_consumed());
    }

    #[test]
    fn rejects_other_request() {
        let t = tok(Uuid::new_v4());
        assert!(!t.authorizes(Uuid::new_v4()));
    }

    #[test]
    fn rejects_after_consume() {
        let r = Uuid::new_v4();
        let mut t = tok(r);
        t.consume();
        assert!(t.is_consumed());
        assert!(
            !t.authorizes(r),
            "single-use: a consumed token never re-authorizes"
        );
    }

    #[test]
    fn serde_roundtrip_preserves_consumed() {
        let r = Uuid::new_v4();
        let mut t = tok(r);
        t.consume();
        let json = serde_json::to_string(&t).unwrap();
        let deser: ExecutionToken = serde_json::from_str(&json).unwrap();
        assert!(deser.is_consumed());
        assert!(!deser.authorizes(r));
    }

    #[test]
    fn legacy_token_without_consumed_field_defaults_false() {
        let r = Uuid::new_v4();
        let json = serde_json::json!({
            "request_id": r,
            "granted_at": "2026-05-31T00:00:00Z",
            "granted_by": "System",
        });
        let t: ExecutionToken = serde_json::from_value(json).unwrap();
        assert!(!t.is_consumed(), "legacy token must default consumed=false");
        assert!(t.authorizes(r));
    }
}
