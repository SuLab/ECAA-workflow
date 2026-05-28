//! Owner-user authZ middleware for per-session routes.
//!
//! The server has no built-in user authentication; that responsibility
//! belongs to an upstream reverse proxy that performs SSO / OIDC / etc.
//! Once the proxy authenticates a request it MUST inject the resulting
//! username via the `X-Scripps-User` request header before forwarding
//! traffic to us. The chat surface persists that header onto the
//! session as `Session.owner_user` at session-create time
//! (`chat_routes::sessions::owner_user_from_headers`).
//!
//! This middleware is the **enforcement** half. It runs on every
//! `/api/chat/session/:id/*` and `/api/git/*` request that carries a
//! session id in the URL path, compares the upstream header value
//! against the persisted `owner_user`, and rejects mismatches with
//! `403 Forbidden`.
//!
//! ## Bypass / single-user-dev posture
//!
//! 1. `SWFC_OWNER_AUTHZ_DISABLE=1` — global kill-switch for the layer.
//!    Used by the integration test harness and by single-user dev where
//!    the bearer-token layer is already the only auth boundary.
//! 2. `Session.owner_user == "local"` — the default value derived from
//!    the server-process `$USER` env when no `X-Scripps-User` header
//!    populated the session. This is the single-user-dev sentinel; in
//!    that mode the session has no specific owner and any caller is
//!    allowed through. The moment a session is created with a real
//!    upstream user header (multi-tenant deploy), the sentinel goes
//!    away and the strict-compare path applies.
//!
//! ## Failure modes
//!
//! - Header absent but the session has a non-`local` owner → 403.
//! - Header present but mismatched against `owner_user` → 403.
//! - Header present and matches → handler runs.
//! - Session not found → handler runs (the handler is responsible for
//!   404'ing; we don't want this layer to leak session-existence info
//!   when the caller wouldn't have been authorized to read it anyway).
//!   This matches the existing `read_only_guard` posture.

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;

use crate::chat_routes::ChatAppState;

/// Header injected by the upstream auth proxy after it has authenticated
/// the requesting user.
pub const OWNER_USER_HEADER: &str = "X-Scripps-User";

/// Single-user-dev sentinel. When `Session.owner_user` equals this
/// value the middleware short-circuits and lets the request through
/// regardless of the header value. See `crates/conversation/src/session/
/// state.rs::default_owner_user` — this is the default when no
/// `X-Scripps-User` header populates the session at create time.
pub(super) const LOCAL_OWNER_SENTINEL: &str = "local";

/// `SWFC_OWNER_AUTHZ_DISABLE=1` env-var bypass. Documented in CLAUDE.md;
/// used by integration tests that mount the chat router without a
/// fronting proxy. Resolved fresh on each request so the integration
/// tests can flip the bypass mid-suite without restarting the server.
pub fn owner_authz_disabled() -> bool {
    matches!(
        std::env::var("SWFC_OWNER_AUTHZ_DISABLE").as_deref(),
        Ok("1")
    )
}

/// Per-request errors surfaced by the middleware. Kept as a discriminated
/// type so tests + the `unauthorized()` helper can distinguish the
/// failure mode without re-parsing the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerAuthzError {
    /// Session has a non-`local` `owner_user` and the request did not
    /// carry an `X-Scripps-User` header.
    HeaderMissing,
    /// Header value did not match `Session.owner_user`.
    OwnerMismatch,
}

impl OwnerAuthzError {
    fn body(self) -> &'static str {
        match self {
            OwnerAuthzError::HeaderMissing => {
                r#"{"error":"forbidden","reason":"missing X-Scripps-User"}"#
            }
            OwnerAuthzError::OwnerMismatch => {
                r#"{"error":"forbidden","reason":"owner_user mismatch"}"#
            }
        }
    }
}

fn forbidden(err: OwnerAuthzError) -> Response {
    let mut resp = Response::new(Body::from(err.body()));
    *resp.status_mut() = StatusCode::FORBIDDEN;
    resp.headers_mut()
        .insert("content-type", "application/json".parse().unwrap());
    resp
}

/// Extract the session id (UUID) from the URL path. Handles every
/// per-session URL shape the chat + git surfaces emit:
///
/// - `/api/chat/session/<uuid>/...`
/// - `/api/git/session/<uuid>/...`
///
/// Returns `None` when the path doesn't include a session segment;
/// non-session routes (e.g. `POST /api/chat/session` for create) bypass
/// the lookup entirely.
fn session_id_from_path(path: &str) -> Option<uuid::Uuid> {
    // Tokens-of-interest: the segment AFTER literal "session". Walk
    // segments rather than regex because (a) the regex crate is already
    // a transitive dep but this is hot path and (b) URL-decode is a
    // no-op for UUIDs.
    let mut segments = path.split('/');
    while let Some(seg) = segments.next() {
        if seg == "session" {
            if let Some(id_seg) = segments.next() {
                return uuid::Uuid::parse_str(id_seg).ok();
            }
        }
    }
    None
}

/// Owner-user authZ layer. Mounted via
/// `axum::middleware::from_fn_with_state(app, verify_owner_middleware)`
/// against the per-session sub-router so non-session routes
/// (`POST /api/chat/session`, `GET /api/chat/sessions/recent`,
/// version-info endpoints, etc.) are not gated.
pub async fn verify_owner_middleware(
    State(app): State<ChatAppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // 1. Global bypass — single-user dev + test harness.
    if owner_authz_disabled() {
        return next.run(req).await;
    }

    // 2. Locate the session id in the URL. Non-session routes get a
    // free pass; the chat surface's session-create / list endpoints
    // don't need owner-check.
    let Some(session_id) = session_id_from_path(req.uri().path()) else {
        return next.run(req).await;
    };

    // 3. Load the session. Unknown sessions fall through to the handler
    // so the handler can emit the canonical 404 — we never want the
    // middleware itself to disclose session existence.
    let Some(session) = app.conversation.get_session(session_id).await else {
        return next.run(req).await;
    };

    // 4. Single-user-dev sentinel: the session has no specific owner.
    // Any caller is authorized.
    if session.owner_user == LOCAL_OWNER_SENTINEL {
        return next.run(req).await;
    }

    // 5. Strict-compare path. Header MUST be present and must equal the
    // persisted `owner_user`. Mismatch / missing → 403.
    //
    // Every forbid path emits a structured warn on
    // `target = "swfc::principal_forbidden"` so operators can alert on
    // unusual rates; `path` is captured so dashboards can break down
    // attacks by endpoint. Conceptual metrics counter:
    // `swfc_principal_forbidden_total`.
    let path = req.uri().path().to_string();
    let presented = req
        .headers()
        .get(OWNER_USER_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    let Some(presented) = presented else {
        tracing::warn!(
            target: "swfc::principal_forbidden",
            session_id = %session_id,
            owner_user = %session.owner_user,
            reason = "header_missing",
            path = %path,
            "owner authz: missing {} header for non-local session",
            OWNER_USER_HEADER,
        );
        return forbidden(OwnerAuthzError::HeaderMissing);
    };
    if presented != session.owner_user {
        tracing::warn!(
            target: "swfc::principal_forbidden",
            session_id = %session_id,
            owner_user = %session.owner_user,
            presented = %presented,
            reason = "owner_mismatch",
            path = %path,
            "owner authz: header mismatch",
        );
        return forbidden(OwnerAuthzError::OwnerMismatch);
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_from_chat_path() {
        let id = uuid::Uuid::new_v4();
        let path = format!("/api/chat/session/{}/state", id);
        assert_eq!(session_id_from_path(&path), Some(id));
    }

    #[test]
    fn session_id_from_git_path() {
        let id = uuid::Uuid::new_v4();
        let path = format!("/api/git/session/{}/branches", id);
        assert_eq!(session_id_from_path(&path), Some(id));
    }

    #[test]
    fn session_id_absent_for_create_route() {
        let path = "/api/chat/session";
        assert_eq!(session_id_from_path(path), None);
    }

    #[test]
    fn session_id_absent_for_non_session_route() {
        let path = "/api/chat/sessions/recent";
        assert_eq!(session_id_from_path(path), None);
    }

    #[test]
    fn malformed_uuid_returns_none() {
        let path = "/api/chat/session/not-a-uuid/state";
        assert_eq!(session_id_from_path(path), None);
    }
}
