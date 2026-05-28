//! Integration test for the `/api/v1/chat/` mount.
//!
//! Both `/api/chat/...` (legacy) and `/api/v1/chat/...` (versioned)
//! dispatch the same handler set via the
//! `dispatch_v1_to_canonical` catch-all that rewrites the URI and
//! forwards into a cloned router service.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use scripps_workflow_conversation::{LlmBackend, MockLlmBackend, SessionStore};
use scripps_workflow_server::chat_routes;
use std::path::Path;
use std::sync::Arc;
use tower::util::ServiceExt;

async fn make_app() -> chat_routes::ChatAppState {
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

/// Build the canonical chat router with the test-default
/// `RequestPrincipal` extension layered on top. The v1 forwarder
/// `nest_service` reuses the same canonical router under the hood, so
/// the extension propagates through the rewrite path as well.
fn make_router(app: chat_routes::ChatAppState) -> axum::Router {
    use scripps_workflow_server::auth::RequestPrincipal;
    chat_routes::router(app).layer(axum::Extension(RequestPrincipal::test_default()))
}

async fn body_json(body: Body) -> serde_json::Value {
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn create_session_works_on_both_prefixes() {
    let app = make_app().await;
    let router = make_router(app.clone());

    // Legacy prefix.
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/session")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"careful_mode": false}"#))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert!(
        body["session_id"].is_string(),
        "legacy mount returned a session id"
    );

    // Versioned prefix — same handler, identical response shape.
    let req = Request::builder()
        .method("POST")
        .uri("/api/v1/chat/session")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"careful_mode": false}"#))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    assert!(
        body["session_id"].is_string(),
        "v1 mount returned a session id"
    );
}

#[tokio::test]
async fn v1_prefix_preserves_query_string() {
    // The cursor pagination contract round-trips ?cursor= / ?limit=
    // through the rewriter; this test makes sure the query string is
    // not lost by the path-rewriting layer.
    let app = make_app().await;
    let router = make_router(app.clone());

    // Bootstrap a session via the legacy mount.
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/session")
        .header("content-type", "application/json")
        .body(Body::from(r#"{}"#))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let body = body_json(resp.into_body()).await;
    let sid = body["session_id"].as_str().unwrap().to_string();

    // Now hit the v1 mount WITH a query string.
    let req = Request::builder()
        .method("GET")
        .uri(format!(
            "/api/v1/chat/session/{}/harness-events?limit=5",
            sid
        ))
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    // The pagination envelope must be present — proves the query
    // string survived the rewrite.
    assert!(
        body["data"].as_array().is_some(),
        "data field present after v1 query rewrite"
    );
    assert!(body["has_more"].is_boolean());
}

#[tokio::test]
async fn unprefixed_path_is_404() {
    // Make sure the rewriter only fires on the exact `/api/v1/chat/`
    // prefix — adjacent paths (`/api/v2/...`, `/api/v1/git/...`)
    // must not be rewritten.
    let app = make_app().await;
    let router = make_router(app.clone());
    let req = Request::builder()
        .method("POST")
        .uri("/api/v2/chat/session")
        .header("content-type", "application/json")
        .body(Body::from(r#"{}"#))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    // Falls through to fallback / 404 because /api/v2/chat is not a
    // mounted route and is not rewritten.
    assert_ne!(
        resp.status(),
        StatusCode::OK,
        "/api/v2/chat must not silently dispatch to the chat handlers",
    );
}
