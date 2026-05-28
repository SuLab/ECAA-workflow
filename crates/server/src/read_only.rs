//! read-only share-token middleware.
//!
//! A request carrying `?share_token=X` (query) or `X-Share-Token: X`
//! (header) that validates against a stored `ShareToken` for the
//! addressed session is treated as a read-only view: any mutation
//! method (POST/PATCH/PUT/DELETE) returns 403. GET / HEAD always
//! pass; this middleware is a thin permission gate, not an auth
//! system.
//!
//! `SWFC_SHARED_URLS_ENABLED=1` turns the whole thing on; without
//! the flag, tokens are never honored and the middleware is a no-op
//! (every request passes through).

use axum::{
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use chrono::Utc;
use subtle::ConstantTimeEq;
use uuid::Uuid;

use crate::chat_routes::ChatAppState;

fn feature_enabled() -> bool {
    scripps_workflow_core::env_helpers::env_bool("SWFC_SHARED_URLS_ENABLED")
}

/// Tokens are 64-char lowercase hex (sha256 of 32 random bytes; see
/// `chat_routes::share::generate_token`). Anything outside that shape
/// can be rejected before we pay the 8KB SHA-256 cost of `hash_share_token`
/// — closes the cheap-CPU-amplification probe surface.
fn is_well_formed_token(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Extract a share token from either the `?share_token=X` query or the
/// `X-Share-Token: X` header. Returns None when neither is present, or
/// when the presented value doesn't match the canonical 64-hex shape.
fn extract_token(req: &Request<Body>) -> Option<String> {
    if let Some(q) = req.uri().query() {
        for pair in q.split('&') {
            if let Some(eq) = pair.find('=') {
                let (k, v) = pair.split_at(eq);
                if k == "share_token" {
                    let v = v.trim_start_matches('=');
                    if is_well_formed_token(v) {
                        // Token characters are [a-f0-9] so no
                        // percent-decoding is needed.
                        return Some(v.to_string());
                    }
                }
            }
        }
    }
    let header = req
        .headers()
        .get("x-share-token")
        .and_then(|v| v.to_str().ok())?;
    if is_well_formed_token(header) {
        Some(header.to_string())
    } else {
        None
    }
}

/// Pull the session id out of the path. Covers both
/// `/api/chat/session/:id/...` (the chat surface) and
/// `/api/git/session/:id/...` (the per-package git provenance
/// endpoints) so read-only share tokens cannot leak through to
/// `git commit`/`push` endpoints unguarded.
fn session_id_from_path(path: &str) -> Option<Uuid> {
    const PREFIXES: &[&str] = &["/api/chat/session/", "/api/git/session/"];
    for prefix in PREFIXES {
        if let Some(rest) = path.strip_prefix(prefix) {
            let id = rest.split('/').next().unwrap_or("");
            if let Ok(u) = Uuid::parse_str(id) {
                return Some(u);
            }
        }
    }
    None
}

fn is_mutation(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// Axum middleware that enforces read-only access when a share-token is present.
/// Rejects mutation requests (POST/PUT/PATCH/DELETE) with 403.
pub async fn read_only_guard(
    State(app): State<ChatAppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if !feature_enabled() {
        return next.run(req).await;
    }
    let Some(token) = extract_token(&req) else {
        return next.run(req).await;
    };
    let Some(session_id) = session_id_from_path(req.uri().path()) else {
        // Token on a non-session path — let it through; the endpoint
        // either doesn't care or will 404 on its own.
        return next.run(req).await;
    };
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let now = Utc::now();
    // Hash the presented plaintext and compare
    // constant-time against the persisted SHA-256 digest. The previous
    // implementation compared plaintext-to-plaintext with `==`, which
    // (a) leaked the bearer through the session JSON and (b) used a
    // short-circuiting compare that's timing-side-channel exposed.
    //
    // `expires_at: None` is treated as ALREADY
    // EXPIRED so legacy session entries (pre-Phase-11 plaintext tokens
    // with never-expires policy) fail closed instead of being honored.
    let presented_hash = crate::chat_routes::share::hash_share_token(&token);
    let presented_bytes = presented_hash.as_bytes();
    let valid = session.share_tokens.iter().any(|t| {
        let stored = t.token_hash.as_bytes();
        // ConstantTimeEq requires equal-length inputs.
        stored.len() == presented_bytes.len()
            && bool::from(stored.ct_eq(presented_bytes))
            && t.expires_at.map(|e| e > now).unwrap_or(false)
    });
    if !valid {
        return (StatusCode::FORBIDDEN, "invalid or expired share token").into_response();
    }
    if is_mutation(req.method()) {
        return (
            StatusCode::FORBIDDEN,
            "read-only share token cannot perform mutations",
        )
            .into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup/teardown; the bounded waiver is
    // scoped to this `mod tests` block.
    #![allow(unsafe_code)]
    use super::*;
    use crate::chat_routes::ChatAppState;
    use axum::{
        body::Body,
        http::Request,
        routing::{get, post},
        Router,
    };
    use chrono::Duration;
    use scripps_workflow_conversation::{LlmBackend, MockLlmBackend, SessionStore, ShareToken};
    use std::path::PathBuf;
    use std::sync::Arc;
    use tower::ServiceExt;

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

    fn config_dir() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config")
    }

    async fn build_router() -> (Router, ChatAppState) {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        std::mem::forget(dir);
        let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(vec![]));
        let app = ChatAppState::with_backend(backend, store, config_dir());

        let inner: Router = Router::new()
            .route("/api/chat/session/:id/state", get(|| async { "ok" }))
            .route(
                "/api/chat/session/:id/mutate",
                post(|| async { "ok" })
                    .put(|| async { "ok" })
                    .patch(|| async { "ok" })
                    .delete(|| async { "ok" }),
            )
            .route("/health", get(|| async { "ok" }));

        // Layer a default `RequestPrincipal` extension so any inner
        // handler that extracts `Extension<RequestPrincipal>` (C1
        // hardening) resolves cleanly under this hand-rolled test
        // router. Mirrors the same fix applied to
        // `chat_routes::test_support::make_router`. Tests in this
        // module use stub handlers that don't read the principal, but
        // we layer it anyway so the helper stays consistent with the
        // canonical test wiring and future stubs can swap in real
        // handlers without surprise 500s.
        let router = inner
            .layer(axum::middleware::from_fn_with_state(
                app.clone(),
                read_only_guard,
            ))
            .layer(axum::Extension(
                crate::auth::RequestPrincipal::test_default(),
            ));
        (router, app)
    }

    async fn seed_session_with_token(
        app: &ChatAppState,
        expires_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> (Uuid, String) {
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        // Store sha256(plaintext); the test returns plaintext
        // to the caller so request paths exercise the hash-and-compare.
        let plaintext: String = (0..64).map(|_| 'a').collect();
        let token_hash = crate::chat_routes::share::hash_share_token(&plaintext);
        // `expires_at == None` is now treated as
        // already-expired by the middleware. Tests that pass `None`
        // expected the legacy never-expires policy; default such tests
        // to a 1h-future expiry so the happy-path tests keep passing.
        // The "expired-token" test explicitly passes `Some(past)`.
        let effective_expiry = expires_at.or_else(|| Some(Utc::now() + Duration::hours(1)));
        let store = app.conversation.store_handle();
        store
            .update(id, move |s| {
                s.share_tokens.push(ShareToken {
                    token_hash: token_hash.clone(),
                    expires_at: effective_expiry,
                    created_at: Utc::now(),
                });
                Ok(())
            })
            .await
            .unwrap();
        (id, plaintext)
    }

    /// Exercise the multi-prefix matcher.
    #[test]
    fn session_id_from_path_matches_git_prefix() {
        let u = Uuid::new_v4();
        let chat = format!("/api/chat/session/{u}/state");
        let git = format!("/api/git/session/{u}/commit");
        assert_eq!(session_id_from_path(&chat), Some(u));
        assert_eq!(session_id_from_path(&git), Some(u));
        assert_eq!(session_id_from_path("/api/other/path"), None);
        assert_eq!(session_id_from_path("/api/chat/session/not-a-uuid/x"), None);
    }

    /// Build a request with no token. Caller picks method + path.
    fn req(method: &str, uri: String) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    fn req_with_header(method: &str, uri: String, token: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("X-Share-Token", token)
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn feature_off_passes_through_mutation() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::disable();
        let (router, app) = build_router().await;
        let (id, token) = seed_session_with_token(&app, None).await;
        // POST + valid token, but feature flag off → middleware no-op.
        let resp = router
            .oneshot(req_with_header(
                "POST",
                format!("/api/chat/session/{}/mutate", id),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn no_token_passes_through_mutation_when_feature_on() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = build_router().await;
        let (id, _token) = seed_session_with_token(&app, None).await;
        // No token at all — request flows through unguarded.
        let resp = router
            .oneshot(req("POST", format!("/api/chat/session/{}/mutate", id)))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn non_session_path_passes_through() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, _app) = build_router().await;
        // Token presented on a path the middleware can't extract a
        // session id from — the gate lets it through; the endpoint
        // (or a 404 router) decides the response.
        let resp = router
            .oneshot(req_with_header(
                "GET",
                "/health".to_string(),
                "anything-here",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn valid_token_get_passes() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = build_router().await;
        let (id, token) = seed_session_with_token(&app, None).await;
        let resp = router
            .oneshot(req_with_header(
                "GET",
                format!("/api/chat/session/{}/state", id),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn valid_token_post_returns_403() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = build_router().await;
        let (id, token) = seed_session_with_token(&app, None).await;
        let resp = router
            .oneshot(req_with_header(
                "POST",
                format!("/api/chat/session/{}/mutate", id),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn valid_token_put_returns_403() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = build_router().await;
        let (id, token) = seed_session_with_token(&app, None).await;
        let resp = router
            .oneshot(req_with_header(
                "PUT",
                format!("/api/chat/session/{}/mutate", id),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn valid_token_patch_returns_403() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = build_router().await;
        let (id, token) = seed_session_with_token(&app, None).await;
        let resp = router
            .oneshot(req_with_header(
                "PATCH",
                format!("/api/chat/session/{}/mutate", id),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn valid_token_delete_returns_403() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = build_router().await;
        let (id, token) = seed_session_with_token(&app, None).await;
        let resp = router
            .oneshot(req_with_header(
                "DELETE",
                format!("/api/chat/session/{}/mutate", id),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn invalid_token_returns_403() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = build_router().await;
        let (id, _real_token) = seed_session_with_token(&app, None).await;
        // Present a wrong but well-formed token (64-char lowercase
        // hex) so the middleware's `is_well_formed_token` shape check
        // passes and the hash-comparison path runs. A malformed token
        // is treated as no-token (defense-in-depth) and would
        // pass through to the inner handler.
        let resp = router
            .oneshot(req_with_header(
                "GET",
                format!("/api/chat/session/{}/state", id),
                "0000000000000000000000000000000000000000000000000000000000000000",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn expired_token_returns_403() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = build_router().await;
        // Token expired one hour ago.
        let past = Utc::now() - Duration::hours(1);
        let (id, token) = seed_session_with_token(&app, Some(past)).await;
        let resp = router
            .oneshot(req_with_header(
                "GET",
                format!("/api/chat/session/{}/state", id),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn extract_token_from_query_param() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = build_router().await;
        let (id, token) = seed_session_with_token(&app, None).await;
        // No header — token only in the query string.
        let resp = router
            .oneshot(req(
                "GET",
                format!("/api/chat/session/{}/state?share_token={}", id, token),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn unknown_session_returns_404() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, _app) = build_router().await;
        // Random session id with a token attached — middleware has to
        // load the session to validate, so this surfaces as a 404 from
        // the guard itself (not the route layer).
        let bogus = Uuid::new_v4();
        let resp = router
            .oneshot(req_with_header(
                "GET",
                format!("/api/chat/session/{}/state", bogus),
                // Well-formed (64-char lowercase hex) wrong token so
                // the middleware's shape check passes and reaches the
                // session lookup. Malformed tokens are treated as
                // no-token and would pass through.
                "0000000000000000000000000000000000000000000000000000000000000000",
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn future_expiry_passes_for_get() {
        let _lock = crate::chat_routes::test_support::SHARED_URLS_ENV_LOCK
            .lock()
            .await;
        let _guard = FlagGuard::enable();
        let (router, app) = build_router().await;
        // Token expires one hour from now — still valid.
        let future = Utc::now() + Duration::hours(1);
        let (id, token) = seed_session_with_token(&app, Some(future)).await;
        let resp = router
            .oneshot(req_with_header(
                "GET",
                format!("/api/chat/session/{}/state", id),
                &token,
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
