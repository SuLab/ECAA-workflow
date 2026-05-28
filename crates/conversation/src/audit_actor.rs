//! Audit actor for high-leverage session mutations.
//!
//! This is the conversation-crate-local copy of the same enum that
//! lives in `crates/server/src/auth/principal.rs::AuditActor`. The two
//! are kept intentionally aligned (same variant names + carried data)
//! and the server crate defines a `From<&server::AuditActor>` impl so
//! handler code that derives an actor from `RequestPrincipal` can stamp
//! it onto a [`crate::session::ConfirmationToken`] without pulling the
//! server crate into the conversation crate's dep arrow.
//!
//! Why a separate type from
//! `scripps_workflow_core::decision_log::DecisionActor`? `DecisionActor`
//! is a coarse classifier (Sme / Llm / Harness) keyed for the
//! `runtime/decisions.jsonl` audit log; `AuditActor` carries the actual
//! identity string (e.g. the SME's owner-user) so a confirmation token
//! is bound to a specific named principal. The two types coexist on
//! purpose: the decision log keeps its coarse shape (UI surface), the
//! token carries the granular identity (security surface).

use serde::{Deserialize, Serialize};

/// Audit-actor for `ConfirmationToken::granted_by`. Mirrors
/// `crates/server/src/auth/principal.rs::AuditActor`; the server crate
/// provides `From<&server::AuditActor>` for this type so the
/// conversation crate stays at the bottom of the dep arrow.
///
/// Variants:
///
/// - `User(owner_user)` — authenticated owner of the session. The
///   string carries the owner-user identifier the server's
///   `RequestPrincipal::Owner` carries.
/// - `ShareViewer` — read-only share token. Never authorized to confirm;
///   reserved for forward-compat / sanity checks.
/// - `Harness` — system-issued harness token. Not a confirm actor today
///   (the harness has no /confirm path); reserved.
/// - `System` — fallback when the server cannot identify a principal
///   (e.g. CLI offline paths). Gives handlers an explicit "I don't
///   know who you are" sentinel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub enum AuditActor {
    /// Authenticated user identified by their user-id string.
    User(String),
    /// Read-only viewer accessing via a share token.
    ShareViewer,
    /// The execution harness (writes task state via progress events).
    Harness,
    /// Fallback when no principal can be identified (e.g. CLI paths).
    System,
}
