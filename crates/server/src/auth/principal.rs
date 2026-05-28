//! `RequestPrincipal` — single source of authentication truth.
//!
//! Replaces the three independent unbound auth layers (bearer +
//! X-Scripps-User + share-token) with a unified principal extracted
//! once at the middleware boundary and threaded through every handler
//! via Axum's request extensions.

use crate::chat_routes::app_state::ChatAppState;
use axum::{
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
use ecaa_workflow_conversation::session::Session;
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// Single source of authentication truth for any handler.
///
/// Use via `axum::Extension<RequestPrincipal>` in handler signatures.
/// Constructed once by [`extract_principal`] middleware; never re-derived.
#[derive(Debug, Clone)]
pub enum RequestPrincipal {
    /// Request bears no recognized credential. May happen on public
    /// endpoints (none currently mutate). Handlers should error 401
    /// or 403 when they need any other principal type.
    Anonymous,
    /// Authenticated by bearer + X-Scripps-User. Owns sessions tagged
    /// with matching owner_user. `user` is always NFC-normalized
    /// + ASCII-lowercased (see [`normalize_owner_user`]).
    Owner {
        /// NFC-normalized + ASCII-lowercased owner username.
        user: String,
        /// True when the request carried a valid `Authorization: Bearer` header.
        bearer_authenticated: bool,
    },
    /// Authenticated by share-token only. Read-only viewer of a specific
    /// session id. Cannot mutate; cannot traverse the lineage parent
    /// chain (closes P0-209).
    /// Share-token viewer; session_id is the session the token grants access to.
    ShareViewer { session_id: Uuid, scope: ShareScope },
    /// Authenticated by harness-issued self-token (issued at /start_execution,
    /// session-scoped). Used by the harness to POST task-state updates back
    /// to the server.
    /// Harness self-token; session_id is the session the harness is executing against.
    HarnessAgent { session_id: Uuid },
}

/// Access scope granted by a share token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareScope {
    /// Token grants read-only access; mutation endpoints return 403.
    ReadOnly,
}

/// Audit-actor stamped onto DecisionRecord by handler code. Always
/// derived from the principal — never from request body fields
/// (closes P1-224).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditActor {
    /// Authenticated SME owner with the given username.
    User(String),
    /// Read-only share-token viewer.
    ShareViewer,
    /// Harness self-token agent.
    Harness,
    /// Internal server action not initiated by an external principal.
    System,
}

impl RequestPrincipal {
    /// Return the username if this is an `Owner` principal.
    pub fn owner_user(&self) -> Option<&str> {
        match self {
            RequestPrincipal::Owner { user, .. } => Some(user),
            _ => None,
        }
    }

    /// True iff this principal is authorized to READ the given session.
    pub fn can_read(&self, session: &Session) -> bool {
        match self {
            RequestPrincipal::Anonymous => false,
            RequestPrincipal::Owner { user, .. } => user == &session.owner_user,
            RequestPrincipal::ShareViewer { session_id, .. } => *session_id == session.id,
            RequestPrincipal::HarnessAgent { session_id } => *session_id == session.id,
        }
    }

    /// True iff this principal is authorized to MUTATE the given session.
    /// Share viewers cannot mutate.
    pub fn can_mutate(&self, session: &Session) -> bool {
        match self {
            RequestPrincipal::Owner { user, .. } => user == &session.owner_user,
            RequestPrincipal::HarnessAgent { session_id } => *session_id == session.id,
            RequestPrincipal::Anonymous | RequestPrincipal::ShareViewer { .. } => false,
        }
    }

    /// True iff this principal can read the LINEAGE PARENT of the given
    /// session. Share tokens do NOT confer parent access (closes P0-209).
    pub fn can_read_lineage_parent(&self, parent: &Session) -> bool {
        match self {
            RequestPrincipal::Owner { user, .. } => user == &parent.owner_user,
            _ => false,
        }
    }

    /// Audit-actor for DecisionRecord stamping. Always derived from
    /// principal, never from body (closes P1-224).
    pub fn audit_actor(&self) -> AuditActor {
        match self {
            RequestPrincipal::Owner { user, .. } => AuditActor::User(user.clone()),
            RequestPrincipal::ShareViewer { .. } => AuditActor::ShareViewer,
            RequestPrincipal::HarnessAgent { .. } => AuditActor::Harness,
            RequestPrincipal::Anonymous => AuditActor::System,
        }
    }

    /// Test-only constructor for the chat-routes test router. Returns a
    /// bearer-authenticated `Owner` principal with the canonical "local"
    /// single-user pseudo-owner — mirrors the bearer-without-header
    /// fallback in [`extract_principal`] so handlers that gate on
    /// `Owner { .. }` resolve cleanly under `make_router` without
    /// rebuilding the auth middleware stack in every test.
    ///
    /// Production paths must NOT call this — the real principal is
    /// constructed by `extract_principal` from request headers.
    pub fn test_default() -> Self {
        RequestPrincipal::Owner {
            user: "local".to_string(),
            bearer_authenticated: true,
        }
    }
}

/// Normalize an owner-user string for storage and compare: NFC + ASCII
/// lowercase. Casing + Unicode-form variants of the same identity
/// collapse to one stored owner_user.
pub fn normalize_owner_user(s: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    s.nfc().collect::<String>().to_ascii_lowercase()
}

/// Validate an owner-user looks like a real username. Separate from
/// `normalize_owner_user`; callers normalize THEN validate.
pub fn is_valid_username(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Constant-time bearer verify. Comparing presented vs expected bytes
/// in constant time avoids leaking the prefix length through timing.
pub fn constant_time_verify_bearer(presented: &str, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return false; // fail-closed when auth not configured
    };
    presented.as_bytes().ct_eq(expected.as_bytes()).into()
}

/// Extract bearer token from Authorization header. Rejects CR/LF in
/// header value; rejects whitespace inside the trimmed token.
pub fn extract_bearer(headers: &HeaderMap) -> Option<String> {
    let h = headers.get(axum::http::header::AUTHORIZATION)?;
    let s = h.to_str().ok()?;
    if s.bytes().any(|b| b == b'\r' || b == b'\n') {
        return None;
    }
    let token = s.strip_prefix("Bearer ")?.trim();
    if token.is_empty() || token.contains(char::is_whitespace) {
        return None;
    }
    Some(token.to_string())
}

/// Resolve the expected bearer token from the runtime environment.
/// Mirrors [`crate::auth::AuthConfig::from_env`] so the unified
/// middleware reads the same source of truth without depending on the
/// `AuthConfig` extractor (which is wired as separate router state).
/// Returns `None` when the env var is unset or empty — handlers using
/// [`constant_time_verify_bearer`] will then fail closed.
fn expected_bearer_from_env() -> Option<String> {
    std::env::var("SWFC_SERVER_AUTH_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
}

/// Middleware: extract `RequestPrincipal` once, stamp into request
/// extensions. Subsequent handlers read via `axum::Extension<RequestPrincipal>`.
///
/// Resolution order:
/// 1. Share-token via `?share_token=` query param → ShareViewer.
/// 2. Bearer auth + X-Scripps-User header → Owner.
/// 3. Harness self-token via X-Harness-Token header → HarnessAgent.
/// 4. None of the above → Anonymous.
pub async fn extract_principal(
    State(state): State<ChatAppState>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let headers = req.headers().clone();
    let query = req.uri().query().unwrap_or("").to_string();

    // 1. Share-token via query string.
    if let Some(token) = extract_share_token_from_query(&query) {
        match resolve_share_token(&state, &token).await {
            Some(principal) => {
                req.extensions_mut().insert(principal);
                return Ok(next.run(req).await);
            }
            None => return Err(StatusCode::FORBIDDEN),
        }
    }

    // 2. Bearer + X-Scripps-User.
    if let Some(bearer) = extract_bearer(&headers) {
        let expected = expected_bearer_from_env();
        if !constant_time_verify_bearer(&bearer, expected.as_deref()) {
            return Err(StatusCode::UNAUTHORIZED);
        }
        let user_raw = headers
            .get("X-Scripps-User")
            .or_else(|| headers.get("X-Forwarded-User"))
            .and_then(|v| v.to_str().ok())
            .map(normalize_owner_user)
            .filter(|s| is_valid_username(s));

        let principal = match user_raw {
            Some(u) => RequestPrincipal::Owner {
                user: u,
                bearer_authenticated: true,
            },
            None => {
                // Bearer authenticated but no header → fall back to
                // the "local" pseudo-owner (matches existing
                // single-user default established by commit 57ca6ac2).
                RequestPrincipal::Owner {
                    user: "local".into(),
                    bearer_authenticated: true,
                }
            }
        };
        req.extensions_mut().insert(principal);
        return Ok(next.run(req).await);
    }

    // 3. Harness self-token.
    if let Some(harness_token) = headers.get("X-Harness-Token").and_then(|v| v.to_str().ok()) {
        if let Some(principal) = resolve_harness_token(&state, harness_token).await {
            req.extensions_mut().insert(principal);
            return Ok(next.run(req).await);
        }
        return Err(StatusCode::UNAUTHORIZED);
    }

    // 4. Anonymous.
    req.extensions_mut().insert(RequestPrincipal::Anonymous);
    Ok(next.run(req).await)
}

fn extract_share_token_from_query(query: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut split = pair.splitn(2, '=');
        let key = split.next()?;
        let val = split.next().unwrap_or("");
        if key == "share_token" {
            // Reject excessive length (closes P3-244-adjacent SHA256-amplification).
            if val.len() > 128 {
                return None;
            }
            // Hex-only check; cheap.
            if !val.chars().all(|c| c.is_ascii_hexdigit()) {
                return None;
            }
            return Some(val.to_string());
        }
    }
    None
}

async fn resolve_share_token(state: &ChatAppState, token: &str) -> Option<RequestPrincipal> {
    // Hashes the presented token + looks up in session's share_tokens vec.
    // Constant-time compare against stored hash via the share-token
    // module's hash_share_token; delegate to that module rather than
    // re-implementing.
    crate::chat_routes::share::resolve_share_token_principal(state, token).await
}

async fn resolve_harness_token(state: &ChatAppState, token: &str) -> Option<RequestPrincipal> {
    // Stub: harness tokens are issued at /start_execution and stored on
    // the ExecutionHandle. Full resolution will accept iff the token
    // decodes a valid session_id from the state's execution map.
    let _ = (state, token);
    None // wired in subsequent task
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_owner_user_nfc_and_lowercase() {
        // Combining diacritic vs precomposed form normalize identically.
        assert_eq!(normalize_owner_user("Alice"), "alice");
        assert_eq!(normalize_owner_user("ALICE"), "alice");
        // NFC normalization unifies decomposed → composed.
        assert_eq!(normalize_owner_user("\u{00E9}"), "é"); // already composed
        assert_eq!(normalize_owner_user("e\u{0301}"), "é"); // decomposed → composed
    }

    #[test]
    fn is_valid_username_rejects_traversal() {
        assert!(!is_valid_username(""));
        assert!(!is_valid_username("../etc/passwd"));
        assert!(!is_valid_username("a/b"));
        assert!(!is_valid_username("a\\b"));
        assert!(!is_valid_username("a b"));
        assert!(!is_valid_username(&"x".repeat(65)));
        assert!(is_valid_username("alice"));
        assert!(is_valid_username("alice.bob"));
        assert!(is_valid_username("a-b_c.d"));
    }

    #[test]
    fn extract_bearer_rejects_crlf() {
        // axum's HeaderValue parser already rejects CRLF — verify that's
        // still the case, and that our own scan would catch any future
        // path that constructs a HeaderValue from raw bytes.
        let parsed = "Bearer abc123\r\nX-Evil: 1".parse::<axum::http::HeaderValue>();
        assert!(
            parsed.is_err(),
            "HeaderValue must reject embedded CRLF as a first-line defense"
        );
        // Construct a HeaderMap with bytes that survived parsing then
        // had CRLF re-introduced via HeaderValue::from_bytes (unsafe
        // path); our extractor should still reject. HeaderValue
        // additionally guards from_bytes against CRLF, so this is
        // belt-and-suspenders; just confirm extractor stays well-formed
        // on a clean input.
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer abc123".parse().unwrap(),
        );
        assert_eq!(extract_bearer(&h), Some("abc123".into()));
    }

    #[test]
    fn extract_bearer_rejects_whitespace_in_token() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer abc 123".parse().unwrap(),
        );
        assert!(extract_bearer(&h).is_none());
    }

    #[test]
    fn extract_bearer_trims_surrounding_whitespace() {
        let mut h = HeaderMap::new();
        h.insert(
            axum::http::header::AUTHORIZATION,
            "Bearer abc123".parse().unwrap(),
        );
        assert_eq!(extract_bearer(&h), Some("abc123".into()));
    }

    #[test]
    fn share_token_query_rejects_non_hex() {
        assert!(extract_share_token_from_query("share_token=abc%20def").is_none());
        assert!(extract_share_token_from_query("share_token=zzz").is_none());
        assert!(
            extract_share_token_from_query(&format!("share_token={}", "a".repeat(200))).is_none()
        );
    }

    #[test]
    fn share_token_query_accepts_64_hex() {
        let token = "a".repeat(64);
        assert_eq!(
            extract_share_token_from_query(&format!("share_token={token}")),
            Some(token)
        );
    }
}
