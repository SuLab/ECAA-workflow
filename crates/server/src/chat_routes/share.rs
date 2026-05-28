//! shared read-only URL endpoints.
//!
//! - `POST /api/chat/session/:id/share-token` — issue a new token.
//! - `DELETE /api/chat/session/:id/share-token/:token` — revoke.
//! - `GET /api/chat/session/:id/share-tokens` — list active tokens.
//!
//! Gated on `SWFC_SHARED_URLS_ENABLED=1`. The read-only middleware
//! (`crate::read_only`) consumes the token from `?share_token=` or
//! the `X-Share-Token` header and rejects any mutation endpoint
//! (POST/PATCH/PUT/DELETE) with 403. Read endpoints let the request
//! through unchanged.

use super::ChatAppState;
use axum::extract::Path;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use chrono::{Duration, Utc};
use rand::rngs::OsRng;
use rand::RngCore;
use scripps_workflow_conversation::ShareToken;
use scripps_workflow_core::hash_utils::sha256_hex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use zeroize::Zeroizing;

/// Hard cap on share-token lifetime. The audit
/// found unbounded-TTL tokens stored in plaintext were a stored-XSS
/// pivot surface; bounding lifetime narrows the window an exfiltrated
/// token is useful.
const SHARE_TOKEN_MAX_TTL_HOURS: i64 = 168; // 7 days

/// Hash the bearer plaintext into the canonical
/// 64-char lowercase hex form persisted alongside the session. Used
/// by both issuance (`post_share_token`) and verification
/// (`read_only::read_only_guard`) so the compare is over equally
/// shaped values.
pub fn hash_share_token(plaintext: &str) -> String {
    sha256_hex(plaintext.as_bytes())
}

/// Resolve a presented share-token plaintext to a
/// [`crate::auth::principal::RequestPrincipal::ShareViewer`].
///
/// Called from `auth::principal::extract_principal`. Scans all sessions
/// for a matching `sha256(plaintext)` token-hash with an unexpired
/// `expires_at`. Returns `None` if no match or if expired.
///
/// `expires_at: None` is treated as ALREADY EXPIRED to match the
/// `read_only_guard` posture (closes the legacy never-expires-token
/// loophole).
///
/// This scan is O(N sessions). A future in-memory token-hash →
/// session-id index built at session-load + share-token-issue time
/// would lift it to O(1).
pub async fn resolve_share_token_principal(
    state: &ChatAppState,
    token: &str,
) -> Option<crate::auth::principal::RequestPrincipal> {
    use subtle::ConstantTimeEq;

    let presented_hash = hash_share_token(token);
    let presented_bytes = presented_hash.as_bytes();
    let now = Utc::now();

    for session in state.conversation.iter_sessions().await {
        for stored in &session.share_tokens {
            let stored_bytes = stored.token_hash.as_bytes();
            if stored_bytes.len() != presented_bytes.len() {
                continue;
            }
            if !bool::from(stored_bytes.ct_eq(presented_bytes)) {
                continue;
            }
            // `None` = expired (fail-closed).
            let live = stored.expires_at.map(|e| e > now).unwrap_or(false);
            if !live {
                tracing::warn!(
                    session_id = %session.id,
                    "share-token matched but is expired; rejecting"
                );
                return None;
            }
            return Some(crate::auth::principal::RequestPrincipal::ShareViewer {
                session_id: session.id,
                scope: crate::auth::principal::ShareScope::ReadOnly,
            });
        }
    }
    None
}

fn feature_enabled() -> bool {
    scripps_workflow_core::env_helpers::env_bool("SWFC_SHARED_URLS_ENABLED")
}

/// Request body for `POST /api/chat/session/:id/share-token`.
#[derive(Debug, Deserialize)]
pub struct CreateTokenRequest {
    /// Token lifetime in hours. REQUIRED and
    /// capped at `SHARE_TOKEN_MAX_TTL_HOURS` (7 days). Values <= 0 or
    /// > 168 are rejected at the handler with 400.
    pub expires_in_hours: i64,
}

/// Returned at issuance time. `token` is the plaintext bearer the
/// caller must save — the server never persists it. On subsequent
/// requests, only the `sha256(plaintext)` hex digest in
/// `Session::share_tokens[].token_hash` is compared.
#[derive(Debug, Clone, Serialize)]
pub struct TokenDescriptor {
    /// Plaintext token. Returned exactly once on creation; not
    /// recoverable from `list_share_tokens` (which returns only
    /// metadata).
    pub token: String,
    /// ISO 8601 expiry timestamp; `None` tokens are treated as expired.
    pub expires_at: Option<String>,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// Always `"read_only"` in this version.
    pub scope: &'static str,
}

/// Metadata-only view for `GET /share-tokens` — never echoes the
/// hash to the client (a hash leak would still let an attacker
/// brute-force the preimage if the random source ever weakened).
#[derive(Debug, Clone, Serialize)]
pub struct TokenMetadata {
    /// Truncated hash prefix for UI display ("token …a1b2c3d4").
    pub token_prefix: String,
    /// ISO 8601 expiry timestamp.
    pub expires_at: Option<String>,
    /// ISO 8601 creation timestamp.
    pub created_at: String,
    /// Always `"read_only"` in this version.
    pub scope: &'static str,
}

fn to_metadata(t: &ShareToken) -> TokenMetadata {
    TokenMetadata {
        token_prefix: t.token_hash.chars().take(8).collect(),
        expires_at: t.expires_at.map(|dt| dt.to_rfc3339()),
        created_at: t.created_at.to_rfc3339(),
        scope: "read_only",
    }
}

/// Generate a true 256-bit random bearer token
/// from `OsRng`. Returned as 64-char lowercase hex (32 bytes × 2).
/// The previous implementation concatenated two UUID v4s which leaked
/// 4 bits per UUID to the version/variant nibbles (~244 effective
/// bits); this implementation gets the full 256.
///
/// R3-S4 — return the plaintext wrapped in `Zeroizing<String>` so the
/// heap allocation backing the hex string is wiped on drop. Callers
/// hold the wrapper for the lifetime they need the plaintext (hashing
/// it, copying into the response), and the local-variable destructor
/// scrubs the bytes the moment the wrapper goes out of scope.
fn generate_token() -> Zeroizing<String> {
    // Wrap the random byte buffer too — `Zeroizing<[u8; 32]>` clears
    // the array slot when this local is dropped, so even if hex::encode
    // is inlined and reads from the stack we don't leave the entropy
    // sitting in a stale frame.
    let mut bytes: Zeroizing<[u8; 32]> = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(bytes.as_mut_slice());
    Zeroizing::new(hex::encode(bytes.as_slice()))
}

pub async fn post_share_token(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<CreateTokenRequest>,
) -> impl IntoResponse {
    if !feature_enabled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "shared URLs disabled (set SWFC_SHARED_URLS_ENABLED=1 to enable)",
        )
            .into_response();
    }
    if app.conversation.get_session(session_id).await.is_none() {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }
    // Mandatory positive TTL capped at 7 days.
    if req.expires_in_hours <= 0 || req.expires_in_hours > SHARE_TOKEN_MAX_TTL_HOURS {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "expires_in_hours must be in 1..={SHARE_TOKEN_MAX_TTL_HOURS}; got {}",
                req.expires_in_hours
            ),
        )
            .into_response();
    }
    let Some(expires_at) = Utc::now().checked_add_signed(Duration::hours(req.expires_in_hours))
    else {
        return (
            StatusCode::BAD_REQUEST,
            "expires_in_hours overflows the calendar clock",
        )
            .into_response();
    };
    // Generate fresh plaintext, persist only the
    // SHA-256 digest, return the plaintext to the caller exactly once.
    //
    // R3-S4 — `plaintext` is `Zeroizing<String>`: the heap buffer behind
    // the hex string is wiped on drop, narrowing the window the bytes
    // sit in process memory. The response struct still owns a `String`
    // copy that lives until axum serializes the body; that residual is
    // unavoidable without rewriting the serialization path, but the
    // generator-side allocation is now scrubbed deterministically.
    let plaintext = generate_token();
    let token_hash = hash_share_token(&plaintext);
    let created_at = Utc::now();
    let stored = ShareToken {
        token_hash: token_hash.clone(),
        expires_at: Some(expires_at),
        created_at,
    };
    let stored_clone = stored.clone();
    let store = app.conversation.store_handle();
    match store
        .update(session_id, move |s| {
            s.share_tokens.push(stored_clone.clone());
            Ok(())
        })
        .await
    {
        Ok(_) => Json(TokenDescriptor {
            // Clone into the response struct so the local `Zeroizing<String>`
            // can scrub itself on the function-scope drop. Without the
            // clone we'd `into_inner()` the Zeroizing, which would
            // surrender the wipe guarantee for no benefit.
            token: (*plaintext).clone(),
            expires_at: stored.expires_at.map(|dt| dt.to_rfc3339()),
            created_at: stored.created_at.to_rfc3339(),
            scope: "read_only",
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to persist share token: {}", e),
        )
            .into_response(),
    }
}

/// `DELETE /api/chat/session/:id/share-token/:token` — revoke a share token.
pub async fn delete_share_token(
    State(app): State<ChatAppState>,
    Path((session_id, token)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    if !feature_enabled() {
        return (StatusCode::SERVICE_UNAVAILABLE, "shared URLs disabled").into_response();
    }
    if app.conversation.get_session(session_id).await.is_none() {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }
    // Revocation by hash. The `token` URL
    // parameter may be EITHER the plaintext bearer (when the operator
    // is revoking a token they still hold) OR the stored hash (when
    // revoking from the UI, which never sees plaintext after creation).
    // Match against both: hash the input first, then compare against
    // the stored hash. Direct hash equality covers the UI-from-list
    // case where the input was already the hash.
    let input_hash = hash_share_token(&token);
    let token_for_closure = token.clone();
    let store = app.conversation.store_handle();
    match store
        .update(session_id, move |s| {
            s.share_tokens
                .retain(|t| t.token_hash != input_hash && t.token_hash != token_for_closure);
            Ok(())
        })
        .await
    {
        Ok(_) => (StatusCode::NO_CONTENT, ()).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to revoke token: {}", e),
        )
            .into_response(),
    }
}

/// When `?cursor=` or `?limit=` is present this
/// endpoint returns the paginated `{ data, next_cursor, has_more }`
/// envelope. When neither query parameter is present the legacy
/// shape (a bare `Vec<TokenMetadata>`) is returned so existing UI
/// consumers keep working unchanged.
pub async fn list_share_tokens(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    if !feature_enabled() {
        return (StatusCode::SERVICE_UNAVAILABLE, "shared URLs disabled").into_response();
    }
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let now = Utc::now();
    // `expires_at: None` is treated as expired
    // by the read_only guard; mirror here so the UI list never shows a
    // token it can't actually use. Also returns only metadata — the
    // hash and plaintext are never echoed.
    let list: Vec<TokenMetadata> = session
        .share_tokens
        .iter()
        .filter(|t| t.expires_at.map(|e| e > now).unwrap_or(false))
        .map(to_metadata)
        .collect();
    let wants_pagination = query.contains_key("cursor") || query.contains_key("limit");
    if !wants_pagination {
        return Json(list).into_response();
    }
    let params = super::PaginationParams::from_query(&query);
    let page = super::PaginatedPage::from_slice(&list, params);
    Json(page).into_response()
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub const ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/chat/session/:id/share-token"),
    ("DELETE", "/api/chat/session/:id/share-token/:token"),
    ("GET", "/api/chat/session/:id/share-tokens"),
];

/// Build the share-token sub-router to be merged into the main chat router.
pub fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/share-token",
            axum::routing::post(post_share_token),
        )
        .route(
            "/api/chat/session/:id/share-token/:token",
            axum::routing::delete(delete_share_token),
        )
        .route(
            "/api/chat/session/:id/share-tokens",
            axum::routing::get(list_share_tokens),
        )
}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup/teardown; the bounded waiver is
    // scoped to this `mod tests` block.
    #![allow(unsafe_code)]
    use super::{generate_token, hash_share_token};
    use crate::chat_routes::test_support::{body_json, make_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use scripps_workflow_conversation::ShareToken;
    use tower::util::ServiceExt;
    use uuid::Uuid;

    /// Set/clear the feature flag for the lifetime of the guard.
    struct FlagGuard {
        prior: Option<String>,
    }
    impl FlagGuard {
        fn enable() -> Self {
            let prior = std::env::var("SWFC_SHARED_URLS_ENABLED").ok();
            unsafe { std::env::set_var("SWFC_SHARED_URLS_ENABLED", "1") };
            Self { prior }
        }
        fn disable() -> Self {
            let prior = std::env::var("SWFC_SHARED_URLS_ENABLED").ok();
            unsafe { std::env::remove_var("SWFC_SHARED_URLS_ENABLED") };
            Self { prior }
        }
    }
    impl Drop for FlagGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => unsafe { std::env::set_var("SWFC_SHARED_URLS_ENABLED", v) },
                None => unsafe { std::env::remove_var("SWFC_SHARED_URLS_ENABLED") },
            }
        }
    }

    fn json_post(uri: String, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn post_share_token_returns_503_when_feature_disabled() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::disable();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        // Body must deserialize so the feature-flag branch
        // Gets reached (was `null` before mandatory-TTL; the
        // Json extractor now rejects null at 422 before the handler).
        let req = json_post(
            format!("/api/chat/session/{}/share-token", id),
            serde_json::json!({"expires_in_hours": 24}),
        );
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn post_share_token_creates_token_and_persists_on_session() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        // Expires_in_hours is now mandatory.
        let req = json_post(
            format!("/api/chat/session/{}/share-token", id),
            serde_json::json!({"expires_in_hours": 24}),
        );
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let token = body["token"]
            .as_str()
            .expect("token in response")
            .to_string();
        assert_eq!(body["scope"], "read_only");
        assert_eq!(token.len(), 64, "token should be 32 bytes hex (64 chars)");

        // Only the hash is persisted; plaintext
        // is never recoverable from the session.
        let session = app.conversation.get_session(id).await.unwrap();
        assert_eq!(session.share_tokens.len(), 1);
        assert_eq!(
            session.share_tokens[0].token_hash,
            hash_share_token(&token),
            "stored value must be sha256(plaintext)"
        );
        assert_ne!(
            session.share_tokens[0].token_hash, token,
            "plaintext must NOT appear in session storage"
        );
        assert!(
            session.share_tokens[0].expires_at.is_some(),
            "expires_at is now mandatory"
        );
    }

    #[tokio::test]
    async fn post_share_token_with_expiry_records_expires_at() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        let now = chrono::Utc::now();
        let req = json_post(
            format!("/api/chat/session/{}/share-token", id),
            serde_json::json!({"expires_in_hours": 24}),
        );
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let expires_at_str = body["expires_at"].as_str().expect("expires_at in response");
        let expires_at = chrono::DateTime::parse_from_rfc3339(expires_at_str).unwrap();
        let delta = (expires_at.with_timezone(&chrono::Utc) - now).num_hours();
        assert!(
            (23..=25).contains(&delta),
            "expires_at should be ~24h ahead, got {delta}h"
        );
    }

    #[tokio::test]
    async fn post_share_token_404_for_unknown_session() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, _app) = make_router(vec![]).await;
        let bogus = Uuid::new_v4();
        // Body must deserialize successfully so the
        // 404-for-unknown-session branch fires (was relying on empty
        // body before; serde would now reject that as 422).
        let req = json_post(
            format!("/api/chat/session/{}/share-token", bogus),
            serde_json::json!({"expires_in_hours": 24}),
        );
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_share_token_rejects_zero_ttl() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = json_post(
            format!("/api/chat/session/{}/share-token", id),
            serde_json::json!({"expires_in_hours": 0}),
        );
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_share_token_rejects_ttl_over_seven_days() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = json_post(
            format!("/api/chat/session/{}/share-token", id),
            serde_json::json!({"expires_in_hours": 169}),
        );
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn hash_share_token_is_lowercase_64_hex() {
        let h = hash_share_token("hello");
        assert_eq!(h.len(), 64);
        assert!(h
            .chars()
            .all(|c: char| c.is_ascii_hexdigit() && (c.is_ascii_digit() || c.is_lowercase())));
    }

    #[test]
    fn generate_token_returns_64_lowercase_hex() {
        let t = generate_token();
        assert_eq!(t.len(), 64);
        assert!(t
            .chars()
            .all(|c: char| c.is_ascii_hexdigit() && (c.is_ascii_digit() || c.is_lowercase())));
    }

    #[tokio::test]
    async fn delete_share_token_removes_specific_token_only() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        // Create two tokens directly via the store (cheaper than two HTTP calls).
        let store = app.conversation.store_handle();
        let token_a = "a".repeat(64);
        let token_b = "b".repeat(64);
        // What's stored is sha256 of the plaintext, so seed
        // the test that way and use plaintext on the DELETE URL.
        let hash_a = hash_share_token(&token_a);
        let hash_b = hash_share_token(&token_b);
        let expected_b_hash = hash_b.clone();
        store
            .update(id, move |s| {
                s.share_tokens.push(ShareToken {
                    token_hash: hash_a,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                    created_at: chrono::Utc::now(),
                });
                s.share_tokens.push(ShareToken {
                    token_hash: hash_b,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                    created_at: chrono::Utc::now(),
                });
                Ok(())
            })
            .await
            .unwrap();

        // Delete only token_a — pass the plaintext; handler hashes it.
        let req = Request::builder()
            .method("DELETE")
            .uri(format!("/api/chat/session/{}/share-token/{}", id, token_a))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // token_b must remain.
        let session = app.conversation.get_session(id).await.unwrap();
        assert_eq!(session.share_tokens.len(), 1);
        assert_eq!(session.share_tokens[0].token_hash, expected_b_hash);
    }

    #[tokio::test]
    async fn list_share_tokens_excludes_expired() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();

        let store = app.conversation.store_handle();
        let live = "1".repeat(64);
        let dead = "2".repeat(64);
        let live_hash = hash_share_token(&live);
        let dead_hash = hash_share_token(&dead);
        store
            .update(id, move |s| {
                s.share_tokens.push(ShareToken {
                    token_hash: live_hash,
                    expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
                    created_at: chrono::Utc::now(),
                });
                s.share_tokens.push(ShareToken {
                    token_hash: dead_hash,
                    expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
                    created_at: chrono::Utc::now(),
                });
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/share-tokens", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let arr = body.as_array().expect("list response is an array");
        assert_eq!(arr.len(), 1, "expired token must be filtered out");
        // List returns metadata only; assert prefix matches
        // the first 8 chars of sha256(live), not the plaintext.
        let expected_prefix: String = hash_share_token(&live).chars().take(8).collect();
        assert_eq!(arr[0]["token_prefix"], expected_prefix);
        assert!(
            arr[0]["token"].is_null(),
            "plaintext must NOT appear in list response"
        );
    }
}
