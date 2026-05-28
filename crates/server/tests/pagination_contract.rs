//! Integration tests for cursor pagination on the
//! growing-collection list endpoints.
//!
//! Verifies the wire contract on each endpoint:
//! - `GET /api/chat/session/:id/decisions`
//! - `GET /api/chat/session/:id/transcript`
//! - `GET /api/chat/session/:id/share-tokens`
//! - `GET /api/chat/session/:id/harness-events`
//!
//! Each test:
//! 1. Sets up a session with N rows in the underlying collection.
//! 2. Walks the endpoint with `?cursor=&limit=K` until `has_more=false`.
//! 3. Asserts every row is returned exactly once and in stable order.
//! 4. Asserts the legacy unpaginated shape (where preserved) still
//! works for clients that haven't migrated.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use ecaa_workflow_conversation::{
    HarnessEvent, LlmBackend, MockLlmBackend, SessionStore, ShareToken, Turn, TurnRole,
};
use ecaa_workflow_server::chat_routes;
use std::path::Path;
use std::sync::Arc;
use tower::util::ServiceExt;
use uuid::Uuid;

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
/// `RequestPrincipal` extension layered on top. Mirrors the same fix
/// applied to `chat_routes::test_support::make_router` so handlers
/// that extract `Extension<RequestPrincipal>` (C1 hardening) resolve
/// cleanly under integration tests that don't run the production
/// `auth::extract_principal` middleware.
fn make_router(app: chat_routes::ChatAppState) -> axum::Router {
    use ecaa_workflow_server::auth::RequestPrincipal;
    chat_routes::router(app).layer(axum::Extension(RequestPrincipal::test_default()))
}

async fn body_json(body: Body) -> serde_json::Value {
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

async fn create_session(router: &axum::Router) -> Uuid {
    let req = Request::builder()
        .method("POST")
        .uri("/api/chat/session")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"careful_mode": false}"#))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    Uuid::parse_str(body["session_id"].as_str().unwrap()).unwrap()
}

// ── harness-events: paginated walk ─────────────────────────────────

#[tokio::test]
async fn harness_events_paginated_walk_returns_every_event_exactly_once() {
    let app = make_app().await;
    let router = make_router(app.clone());
    let id = create_session(&router).await;

    // Seed 250 harness events with distinct task_id markers.
    app.conversation
        .store_handle()
        .update(id, |s| {
            for i in 0..250usize {
                s.harness_events.push(HarnessEvent {
                    kind: "task_started".into(),
                    task_id: format!("t_{i}"),
                    status: "running".into(),
                    detail: String::new(),
                    remote: None,
                    timestamp: chrono::Utc::now(),
                });
            }
            Ok(())
        })
        .await
        .unwrap();

    // Walk with limit=100 — should require 3 pages (100, 100, 50).
    let mut seen: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut iterations = 0usize;
    loop {
        iterations += 1;
        let q = match cursor.as_deref() {
            Some(c) => format!("limit=100&cursor={c}"),
            None => "limit=100".to_string(),
        };
        let uri = format!("/api/chat/session/{id}/harness-events?{q}");
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let data = body["data"].as_array().expect("paginated `data` field");
        for row in data {
            seen.push(row["taskId"].as_str().unwrap().to_string());
        }
        let has_more = body["has_more"].as_bool().unwrap();
        cursor = body["next_cursor"].as_str().map(|s| s.to_string());
        if !has_more {
            assert!(cursor.is_none(), "last page must have next_cursor=null");
            break;
        }
        assert!(iterations < 10, "walk must terminate");
    }
    assert_eq!(iterations, 3, "250 / 100 = 3 pages");
    assert_eq!(seen.len(), 250, "all 250 rows must surface across pages");
    // Stable ordering: page-wise concat must match insertion order.
    for (i, task_id) in seen.iter().enumerate() {
        assert_eq!(task_id, &format!("t_{i}"));
    }
}

#[tokio::test]
async fn harness_events_unpaginated_request_still_returns_events_array() {
    let app = make_app().await;
    let router = make_router(app.clone());
    let id = create_session(&router).await;

    app.conversation
        .store_handle()
        .update(id, |s| {
            for i in 0..3usize {
                s.harness_events.push(HarnessEvent {
                    kind: "task_started".into(),
                    task_id: format!("t_{i}"),
                    status: "running".into(),
                    detail: String::new(),
                    remote: None,
                    timestamp: chrono::Utc::now(),
                });
            }
            Ok(())
        })
        .await
        .unwrap();

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/chat/session/{id}/harness-events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp.into_body()).await;
    // Legacy field — kept for backward compatibility.
    assert_eq!(body["events"].as_array().unwrap().len(), 3);
    // New paginated envelope also present (3 < default 100, all in one page).
    assert_eq!(body["data"].as_array().unwrap().len(), 3);
    assert_eq!(body["has_more"], false);
    assert!(body["next_cursor"].is_null());
}

// ── transcript: paginated walk ─────────────────────────────────────

#[tokio::test]
async fn transcript_paginated_walk_returns_every_turn_exactly_once() {
    let app = make_app().await;
    let router = make_router(app.clone());
    let id = create_session(&router).await;

    // Seed 130 user turns directly via the store (skips the LLM round-trip).
    app.conversation
        .store_handle()
        .update(id, |s| {
            // `conversation` is `Arc<Vec<Turn>>` for cheap tool-loop
            // cloning; `Arc::make_mut` copies-on-write only when there's
            // more than one holder.
            let v = std::sync::Arc::make_mut(&mut s.conversation);
            for i in 0..130usize {
                let t = Turn {
                    turn_id: Uuid::new_v4(),
                    role: TurnRole::User,
                    content: format!("msg_{i}"),
                    intent: None,
                    tool_calls: Vec::new(),
                    quick_replies: Vec::new(),
                    confirmation_card: None,
                    timestamp: chrono::Utc::now(),
                };
                v.push(t);
            }
            Ok(())
        })
        .await
        .unwrap();

    // Pagination opt-in via ?limit= or ?cursor=.
    let mut total: usize = 0;
    let mut cursor: Option<String> = None;
    let mut iters = 0;
    loop {
        iters += 1;
        let q = match cursor.as_deref() {
            Some(c) => format!("limit=50&cursor={c}"),
            None => "limit=50".to_string(),
        };
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/chat/session/{id}/transcript?{q}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let data = body["data"].as_array().expect("paginated data");
        total += data.len();
        let has_more = body["has_more"].as_bool().unwrap();
        cursor = body["next_cursor"].as_str().map(|s| s.to_string());
        if !has_more {
            assert!(cursor.is_none());
            break;
        }
        assert!(iters < 20, "must terminate");
    }
    // Original greeting (1) + 130 seeded = at least 131. The greeting
    // may or may not exist depending on the mock; either way `total` is
    // at least 130 and matches `session.conversation.len()`.
    let session = app.conversation.get_session(id).await.unwrap();
    assert_eq!(total, session.conversation.len());
    assert!(total >= 130);
}

#[tokio::test]
async fn transcript_unpaginated_legacy_shape_still_returns_array() {
    let app = make_app().await;
    let router = make_router(app.clone());
    let id = create_session(&router).await;

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/chat/session/{id}/transcript"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp.into_body()).await;
    // Legacy shape: top-level is an array, not an envelope.
    assert!(body.is_array(), "no pagination params → bare array");
}

// ── decisions: paginated walk ──────────────────────────────────────

#[tokio::test]
async fn decisions_paginated_envelope_is_present_alongside_legacy() {
    let app = make_app().await;
    let router = make_router(app.clone());
    let id = create_session(&router).await;

    // Fresh session has zero decisions — the envelope is still present.
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/chat/session/{id}/decisions?limit=10"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp.into_body()).await;
    assert!(body["data"].as_array().unwrap().is_empty());
    assert_eq!(body["has_more"], false);
    assert!(body["next_cursor"].is_null());
    // Legacy field still mirrored.
    assert!(body["decisions"].as_array().unwrap().is_empty());
}

// ── share-tokens: paginated walk ───────────────────────────────────

#[tokio::test]
async fn share_tokens_paginated_walk_returns_every_token() {
    // The share-tokens routes are feature-gated. Enable in
    // a single-threaded scope. SAFETY: integration tests in this file
    // do not race on this env var.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("SWFC_SHARED_URLS_ENABLED", "1");
    }

    let app = make_app().await;
    let router = make_router(app.clone());
    let id = create_session(&router).await;

    // Seed 120 share tokens via store update.
    let now = chrono::Utc::now();
    let expires = now + chrono::Duration::hours(1);
    app.conversation
        .store_handle()
        .update(id, |s| {
            for i in 0..120u32 {
                s.share_tokens.push(ShareToken {
                    token_hash: format!("{:064x}", i),
                    expires_at: Some(expires),
                    created_at: now,
                });
            }
            Ok(())
        })
        .await
        .unwrap();

    let mut total: usize = 0;
    let mut cursor: Option<String> = None;
    let mut iters = 0;
    loop {
        iters += 1;
        let q = match cursor.as_deref() {
            Some(c) => format!("limit=50&cursor={c}"),
            None => "limit=50".to_string(),
        };
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/chat/session/{id}/share-tokens?{q}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = body_json(resp.into_body()).await;
        let data = body["data"].as_array().expect("paginated data");
        total += data.len();
        let has_more = body["has_more"].as_bool().unwrap();
        cursor = body["next_cursor"].as_str().map(|s| s.to_string());
        if !has_more {
            assert!(cursor.is_none());
            break;
        }
        assert!(iters < 10, "must terminate");
    }
    assert_eq!(total, 120);
}

#[tokio::test]
async fn share_tokens_unpaginated_returns_legacy_array() {
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("SWFC_SHARED_URLS_ENABLED", "1");
    }

    let app = make_app().await;
    let router = make_router(app.clone());
    let id = create_session(&router).await;

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/chat/session/{id}/share-tokens"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = body_json(resp.into_body()).await;
    // Legacy shape: top-level is a bare array (possibly empty).
    assert!(body.is_array(), "no pagination params → bare array");
}

// ── cursor robustness ──────────────────────────────────────────────

#[tokio::test]
async fn malformed_cursor_treated_as_start_not_400() {
    let app = make_app().await;
    let router = make_router(app.clone());
    let id = create_session(&router).await;

    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!(
                    "/api/chat/session/{id}/harness-events?cursor=not-valid-hex&limit=10"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "malformed cursor must not be a 400 — it falls back to offset=0",
    );
}

#[tokio::test]
async fn limit_above_max_silently_clamped() {
    let app = make_app().await;
    let router = make_router(app.clone());
    let id = create_session(&router).await;

    // limit=99999 → should clamp to MAX_LIMIT (1000) silently.
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/chat/session/{id}/harness-events?limit=99999"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    // No rows seeded — page is empty but the envelope still has the
    // right shape.
    assert!(body["data"].as_array().unwrap().is_empty());
}
