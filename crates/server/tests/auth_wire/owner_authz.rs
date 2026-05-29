// Tests intentionally hold a std::sync::Mutex across `.await` to serialize
// env-var manipulation; the workspace lint is denied at toolchain level.
#![allow(clippy::await_holding_lock)]
//! Integration tests for the owner-user authZ middleware
//! (`crate::auth::verify_owner_middleware`). See `crates/server/src/auth/
//! verify_owner.rs` for the full module documentation.
//!
//! Test matrix:
//! a. matching `X-Scripps-User` header → request passes (200).
//! b. mismatched header → 403 Forbidden.
//! c. no header AND non-`local` `owner_user` → 403.
//! d. no header AND `local` (sentinel) `owner_user` → request passes.
//! e. `ECAA_OWNER_AUTHZ_DISABLE=1` bypass → all requests pass.
//! f. non-session routes (no `/session/<uuid>/` in the path) → bypass
//! (the layer is route-blind by design and short-circuits when
//! there's no session id to compare against).

use axum::{
    body::Body,
    http::{Request, StatusCode},
    middleware::from_fn_with_state,
    Router,
};
use ecaa_workflow_conversation::{
    anthropic::{StopReason, TurnResponse, Usage},
    LlmBackend, MockLlmBackend, SessionStore,
};
use ecaa_workflow_server::auth::verify_owner_middleware;
use ecaa_workflow_server::chat_routes::{router as chat_router, ChatAppState};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;

/// Test fixture: spin up an in-memory ChatAppState + chat router + the
/// owner-authz layer. Mirrors the prod wiring in `lib::run` but without
/// the bearer-token / governor / CORS / trace layers — those are tested
/// independently elsewhere.
async fn make_app() -> (Router, ChatAppState) {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).await.unwrap();
    std::mem::forget(dir);
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(vec![assistant("hi")]));
    let config_dir = config_dir();
    let app = ChatAppState::with_backend(backend, store, config_dir);
    let router =
        chat_router(app.clone()).layer(from_fn_with_state(app.clone(), verify_owner_middleware));
    (router, app)
}

fn assistant(text: &str) -> TurnResponse {
    TurnResponse {
        assistant_content: text.into(),
        tool_uses: vec![],
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        request_metadata: Default::default(),
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

/// Helper: seed a session and override its `owner_user` to the given
/// value. The default `Session::new` resolves owner from `$USER`; we
/// override so the test matrix can hit both the local-sentinel and
/// strict-compare branches deterministically.
async fn seed_session_with_owner(app: &ChatAppState, owner: &str) -> uuid::Uuid {
    let (id, _) = app.conversation.start_session(false).await.unwrap();
    let store = app.conversation.store_handle();
    let owner = owner.to_string();
    store
        .update(id, move |s| {
            s.owner_user = owner.clone();
            Ok(())
        })
        .await
        .unwrap();
    id
}

/// Process-wide env-var lock so tests that mutate `ECAA_OWNER_AUTHZ_DISABLE`
/// don't race the other tests in this binary. `cargo test` runs tests
/// within a binary in parallel; without this lock the bypass-flag test
/// would observe the cleared state mid-call from another test.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

async fn get_state(app_router: &Router, id: uuid::Uuid, header: Option<&str>) -> StatusCode {
    let mut req = Request::builder().uri(format!("/api/chat/session/{}/state", id));
    if let Some(user) = header {
        req = req.header("X-Scripps-User", user);
    }
    let resp = app_router
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    resp.status()
}

#[tokio::test]
async fn matching_user_passes() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::remove_var("ECAA_OWNER_AUTHZ_DISABLE");
    let (router, app) = make_app().await;
    let id = seed_session_with_owner(&app, "alice").await;
    assert_eq!(get_state(&router, id, Some("alice")).await, StatusCode::OK);
}

#[tokio::test]
async fn mismatched_user_returns_403() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::remove_var("ECAA_OWNER_AUTHZ_DISABLE");
    let (router, app) = make_app().await;
    let id = seed_session_with_owner(&app, "alice").await;
    assert_eq!(
        get_state(&router, id, Some("eve")).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn no_header_non_local_owner_returns_403() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::remove_var("ECAA_OWNER_AUTHZ_DISABLE");
    let (router, app) = make_app().await;
    let id = seed_session_with_owner(&app, "alice").await;
    assert_eq!(get_state(&router, id, None).await, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn no_header_local_owner_passes() {
    // The single-user-dev sentinel — `Session.owner_user == "local"` —
    // is the default when no proxy header populated the session at
    // create time. In that mode the middleware short-circuits and
    // any caller is allowed through.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::remove_var("ECAA_OWNER_AUTHZ_DISABLE");
    let (router, app) = make_app().await;
    let id = seed_session_with_owner(&app, "local").await;
    assert_eq!(get_state(&router, id, None).await, StatusCode::OK);
}

#[tokio::test]
async fn bypass_flag_lets_mismatched_user_through() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var("ECAA_OWNER_AUTHZ_DISABLE", "1");
    let (router, app) = make_app().await;
    let id = seed_session_with_owner(&app, "alice").await;
    let status = get_state(&router, id, Some("eve")).await;
    std::env::remove_var("ECAA_OWNER_AUTHZ_DISABLE");
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn unknown_session_falls_through_to_handler() {
    // The middleware doesn't 404 on its own — it falls through so the
    // handler can emit the canonical 404, which avoids leaking
    // session-existence info to unauthorized callers. The handler
    // emits StatusCode::NOT_FOUND for an unknown id.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::remove_var("ECAA_OWNER_AUTHZ_DISABLE");
    let (router, _app) = make_app().await;
    let id = uuid::Uuid::new_v4();
    assert_eq!(
        get_state(&router, id, Some("alice")).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn non_session_route_bypasses_owner_check() {
    // `GET /api/chat/sessions/recent` carries no session id; the layer
    // short-circuits when `session_id_from_path` returns `None`.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::remove_var("ECAA_OWNER_AUTHZ_DISABLE");
    let (router, _app) = make_app().await;
    let resp = router
        .oneshot(
            Request::builder()
                .uri("/api/chat/sessions/recent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
