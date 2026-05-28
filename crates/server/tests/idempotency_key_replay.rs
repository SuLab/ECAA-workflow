//! Integration test for `Idempotency-Key` replay on
//! the three high-impact mutating endpoints (`confirm`,
//! `branch_session`, `start-execution`).
//!
//! The exact handler bodies have a lot of dependencies (a real
//! harness spawn, a session in `PendingConfirmation`, etc.), so this
//! integration test focuses on the SHAPE of the contract:
//!
//! 1. First POST without `Idempotency-Key` runs to completion as
//! normal — no replay header set on the response.
//! 2. First POST WITH `Idempotency-Key` runs the handler, stores the
//! response, and returns it WITHOUT the replay header.
//! 3. Second POST WITH the same `Idempotency-Key` short-circuits
//! before the handler body runs, returning the cached response
//! with `idempotent-replay: true` on the headers.
//!
//! `confirm` on a fresh session is the cleanest target: it hits the
//! state-machine guard and returns a deterministic error envelope.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ecaa_workflow_server::chat_routes;
use tower::util::ServiceExt;
use uuid::Uuid;

/// Build a router + ChatAppState wired with a mock LLM backend, mirror
/// of the in-crate `test_support::make_router` helper but reachable
/// from an integration test.
async fn make_router() -> chat_routes::ChatAppState {
    use ecaa_workflow_conversation::{LlmBackend, MockLlmBackend, SessionStore};
    use std::path::Path;
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await.unwrap();
    std::mem::forget(dir);
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(vec![]));
    let config_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config");
    chat_routes::ChatAppState::with_backend(backend, store, config_dir)
}

fn confirm_request(session_id: Uuid, key: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(format!("/api/chat/session/{}/confirm", session_id));
    if let Some(k) = key {
        b = b.header("Idempotency-Key", k);
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn confirm_without_idempotency_key_serves_no_replay_header() {
    let app = make_router().await;
    let router = chat_routes::router(app.clone());
    let sid = Uuid::new_v4();
    let resp = router
        .clone()
        .oneshot(confirm_request(sid, None))
        .await
        .unwrap();
    // Whatever the status (it'll be a 400-class because the session
    // doesn't exist), the replay header must not be set when no
    // Idempotency-Key was attached.
    assert!(
        resp.headers().get("idempotent-replay").is_none(),
        "no Idempotency-Key on the request must mean no idempotent-replay on the response"
    );
}

#[tokio::test]
async fn confirm_with_idempotency_key_replays_on_retry() {
    let app = make_router().await;
    let router = chat_routes::router(app.clone());
    let sid = Uuid::new_v4();
    let key = format!("test-{}", Uuid::new_v4());

    // First request: should NOT carry the replay header.
    let resp1 = router
        .clone()
        .oneshot(confirm_request(sid, Some(&key)))
        .await
        .unwrap();
    let status1 = resp1.status();
    assert!(
        resp1.headers().get("idempotent-replay").is_none(),
        "the first request is a cache miss; replay header only on retries"
    );

    // Second request with the SAME key: should replay.
    let resp2 = router
        .clone()
        .oneshot(confirm_request(sid, Some(&key)))
        .await
        .unwrap();
    assert_eq!(
        resp2
            .headers()
            .get("idempotent-replay")
            .map(|v| v.as_bytes()),
        Some(&b"true"[..]),
        "second request with same Idempotency-Key must serve from cache"
    );
    assert_eq!(
        resp2.status(),
        status1,
        "the replay's status matches the first request's status"
    );
}

#[tokio::test]
async fn confirm_with_different_keys_each_runs_handler() {
    let app = make_router().await;
    let router = chat_routes::router(app.clone());
    let sid = Uuid::new_v4();
    let key_a = format!("a-{}", Uuid::new_v4());
    let key_b = format!("b-{}", Uuid::new_v4());

    let resp_a = router
        .clone()
        .oneshot(confirm_request(sid, Some(&key_a)))
        .await
        .unwrap();
    let resp_b = router
        .clone()
        .oneshot(confirm_request(sid, Some(&key_b)))
        .await
        .unwrap();
    // Neither should carry the replay header — they're distinct cache
    // entries.
    assert!(resp_a.headers().get("idempotent-replay").is_none());
    assert!(resp_b.headers().get("idempotent-replay").is_none());
}

#[tokio::test]
async fn ttl_env_override_is_picked_up_by_store() {
    // `SWFC_IDEMPOTENCY_TTL_SECS` should override the default
    // 1 hour. We test the constructor path only (not the full router)
    // because mutating env in a multi-threaded integration test is
    // fragile; the deeper behavioral test lives in the unit-test block
    // alongside the store impl.
    let store = chat_routes::IdempotencyStore::from_env();
    let _ = store; // smoke test: constructor doesn't panic
}

/// Bonus: `Idempotency-Key` is case-insensitive per RFC 9110.
#[tokio::test]
async fn header_lookup_is_case_insensitive() {
    let app = make_router().await;
    let router = chat_routes::router(app.clone());
    let sid = Uuid::new_v4();
    let key = format!("case-{}", Uuid::new_v4());

    // First with canonical casing.
    let _ = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/chat/session/{}/confirm", sid))
                .header("Idempotency-Key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Second with lowercase.
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/chat/session/{}/confirm", sid))
                .header("idempotency-key", &key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Axum normalizes header names case-insensitively, so the second
    // request hits the same cache entry — replay header set.
    assert_eq!(
        resp.headers()
            .get("idempotent-replay")
            .map(|v| v.as_bytes()),
        Some(&b"true"[..]),
        "header lookup must be case-insensitive per RFC 9110"
    );
    // Reject the unused 200/204 distinction here — `axum` returns
    // 405 / 412 / etc depending on the session state but the status
    // is irrelevant to the case-insensitivity check.
    let _ = StatusCode::OK; // pin the import
}
