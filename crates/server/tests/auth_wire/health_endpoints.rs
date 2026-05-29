//! `/healthz` + `/readyz` integration tests.
//!
//! Verifies that both endpoints respond without an Authorization header,
//! that `/healthz` always returns 200 with the literal body `ok`, and
//! that `/readyz` returns 200 with `ready: true` when the test-state
//! directories are writable.

use axum::body::to_bytes;
use axum::http::{Request, StatusCode};
use ecaa_workflow_conversation::{LlmBackend, MockLlmBackend, SessionStore};
use ecaa_workflow_server::chat_routes::{health_router, ChatAppState};
use std::sync::Arc;
use tower::ServiceExt;

fn config_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

async fn make_health_app() -> axum::Router {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await.unwrap();
    // Keep the tempdir alive for the test lifetime by forgetting its guard —
    // the same pattern used in test_support::make_router.
    std::mem::forget(dir);
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(vec![]));
    let app = ChatAppState::with_backend(backend, store, config_dir());
    health_router(app)
}

#[tokio::test]
async fn healthz_returns_200_unauthenticated() {
    let app = make_health_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                // Deliberately omit Authorization header.
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "/healthz must return 200 without a bearer token"
    );
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    assert_eq!(
        &body[..],
        b"ok",
        "/healthz body must be the literal string `ok`"
    );
}

#[tokio::test]
async fn readyz_returns_200_when_ready() {
    let app = make_health_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                // Deliberately omit Authorization header.
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "/readyz must return 200 when sessions dir + package root are writable"
    );
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).expect("readyz body must be JSON");
    assert_eq!(
        json["ready"],
        serde_json::json!(true),
        "/readyz JSON must have ready: true for a well-formed test state"
    );
    assert_eq!(
        json["failures"],
        serde_json::json!([]),
        "/readyz JSON must have an empty failures array when all checks pass"
    );
}

#[tokio::test]
async fn readyz_body_has_expected_schema() {
    let app = make_health_app().await;
    let response = app
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // Both `ready` (bool) and `failures` (array) must be present.
    assert!(json["ready"].is_boolean(), "ready must be a boolean");
    assert!(json["failures"].is_array(), "failures must be an array");
}
