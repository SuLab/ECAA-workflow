//! Integration tests for git commit hooks.
//!
//! Per-package shape: the top-level `/api/git/{commit,push}`
//! routes were removed in favor of session-scoped
//! `/api/git/session/:id/{commit,push}`. These tests pin the
//! `effective_enabled` gate through the new shape — a session-scoped
//! commit/push on a session that has no emitted package returns 404, and
//! the kill-switch (`SWFC_GIT_ENABLED=0`) returns 409 regardless.
//!
//! The unit-test surface in
//! `crates/server/src/git_routes/config.rs::tests::effective_enabled_respects_kill_switch`
//! continues to cover the gate logic at the config layer; these
//! tests pin the same property at the HTTP-handler layer through
//! `tower::ServiceExt::oneshot`.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use ecaa_workflow_conversation::{LlmBackend, MockLlmBackend, SessionStore};
use ecaa_workflow_server::chat_routes::ChatAppState;
use ecaa_workflow_server::git_routes;
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

/// Build a `ChatAppState` with `MockLlmBackend` + a tempdir
/// `SessionStore` + the repo's `config/` directory.
async fn build_test_state() -> ChatAppState {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = SessionStore::open(dir.path())
        .await
        .expect("open session store");
    // Leak the tempdir so it lives as long as the SessionStore the
    // ChatAppState holds. Test-only; matches the pattern used by
    // `make_router` in the in-crate test_support helper.
    std::mem::forget(dir);
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(vec![]));
    let config_dir: PathBuf = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config");
    ChatAppState::with_backend(backend, store, config_dir)
}

/// Session-scoped commit returns 404 when the session has no emitted
/// package — the only state in which `/api/git/session/:id/commit`
/// fires before any package work has been emitted. Pins the resolve-
/// package precondition.
#[tokio::test]
async fn post_session_commit_returns_404_when_no_emitted_package() {
    let app = build_test_state().await;
    let (sid, _) = app
        .conversation
        .start_session(false)
        .await
        .expect("start_session");
    // Force-enable git so the route doesn't short-circuit on the
    // kill-switch — we want the "no package" 404 path.
    let mut cfg = app.git_config().read().clone();
    cfg.enabled = true;
    let _ = app.git_config().update(cfg);

    let router = git_routes::router(app);
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/git/session/{}/commit", sid))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"message":"emit","push":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "session-scoped commit must 404 when session has no emitted package"
    );
}

/// `POST /api/git/session/:id/commit` returns `409 CONFLICT` when git
/// integration is disabled — the kill-switch is evaluated before the
/// session-resolution path.
#[tokio::test]
async fn post_session_commit_returns_conflict_when_disabled() {
    let app = build_test_state().await;
    // Default GitConfig has `enabled: false`; build_test_state's
    // GitConfigStore picks up that default since the test config
    // file doesn't exist on disk.
    let bogus = Uuid::new_v4();
    let router = git_routes::router(app);
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/git/session/{}/commit", bogus))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"message":"emit","push":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "post commit must reject with 409 when git integration is disabled"
    );
    let body = to_bytes(resp.into_body(), 1_000_000).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        json["error"].as_str().unwrap_or("").contains("disabled"),
        "expected 'disabled' in error body, got: {}",
        json
    );
}

/// `POST /api/git/session/:id/push` returns `409 CONFLICT` when git
/// integration is disabled — symmetric with the post_session_commit case.
#[tokio::test]
async fn post_session_push_returns_conflict_when_disabled() {
    let app = build_test_state().await;
    let bogus = Uuid::new_v4();
    let router = git_routes::router(app);
    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/git/session/{}/push", bogus))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "post push must reject with 409 when git integration is disabled"
    );
    let body = to_bytes(resp.into_body(), 1_000_000).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        json["error"].as_str().unwrap_or("").contains("disabled"),
        "expected 'disabled' in error body, got: {}",
        json
    );
}

/// The git-config `effective_enabled` gate is the load-bearing
/// invariant — both handler tests above depend on it. This test
/// exercises the gate from outside the crate to mirror the
/// existing in-crate unit test
/// (`config.rs::tests::effective_enabled_respects_kill_switch`).
#[test]
fn git_config_is_reachable_from_lib_boundary() {
    use ecaa_workflow_server::git_routes::GitConfig;
    let cfg = GitConfig {
        enabled: false,
        ..GitConfig::default()
    };
    assert!(
        !cfg.effective_enabled(),
        "default-disabled GitConfig must report effective_enabled=false at the lib boundary"
    );
    let cfg = GitConfig {
        enabled: true,
        ..GitConfig::default()
    };
    assert!(
        cfg.effective_enabled(),
        "enabled GitConfig must report effective_enabled=true at the lib boundary"
    );
}

/// Integration coverage of the read-only side: the `GET /api/git/config`
/// route returns the persisted GitConfig regardless of the
/// `effective_enabled` gate (the SME needs to see the current
/// configuration to flip the kill-switch). When git is disabled, the
/// response still carries the config — the gate only blocks mutation
/// endpoints (commit / push).
#[tokio::test]
async fn get_git_config_returns_persisted_value_even_when_disabled() {
    let app = build_test_state().await;
    let router = git_routes::router(app);
    let resp = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/git/config")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "get_git_config must succeed regardless of effective_enabled — \
         the SME needs to see the config to flip the kill-switch"
    );
    let body = to_bytes(resp.into_body(), 1_000_000).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    // The persisted default has `enabled: false`. Test the field
    // exists; specific values are pinned by the in-crate config
    // unit tests, not duplicated here.
    assert!(
        json.get("enabled").is_some(),
        "git config response must carry 'enabled' field, got: {}",
        json
    );
    // Per-package shape: `repo_path` has been dropped from the config.
    assert!(
        json.get("repo_path").is_none(),
        "GitConfig must NOT carry repo_path post-2026-05-12; got: {}",
        json
    );
}

/// Integration coverage for the emit-hook fire path: when a session
/// has an emitted package, the `/api/git/session/:id/status` route must
/// resolve the package_dir and report the per-package git state. After
/// a `hook_commit` lands a commit, the route must reflect it.
#[tokio::test]
async fn session_status_reflects_per_package_git_state() {
    let app = build_test_state().await;
    let (sid, _) = app
        .conversation
        .start_session(false)
        .await
        .expect("start_session");

    // Seed the session with a fake emitted_package_path so the route
    // can resolve a package dir.
    let pkg = tempfile::tempdir().expect("pkg tempdir");
    std::fs::write(pkg.path().join("ro-crate-metadata.json"), b"{}\n").unwrap();
    app.conversation
        .store_handle()
        .update(sid, |s| {
            s.emitted_package_path = Some(pkg.path().to_path_buf());
            Ok(())
        })
        .await
        .unwrap();

    // Pre-condition: no.git exists.
    assert!(!pkg.path().join(".git").exists());
    let cfg = app.git_config().read().clone();
    // Fire the hook directly — `enabled` defaults to false, so force
    // it on for the test.
    let mut cfg_enabled = cfg.clone();
    cfg_enabled.enabled = true;
    git_routes::service::hook_commit(
        &cfg_enabled,
        pkg.path(),
        "emit",
        "integration test",
        &sid.to_string(),
    );
    assert!(
        pkg.path().join(".git").exists(),
        "hook_commit must auto-init the per-package .git"
    );

    // The status route must surface the new state. enabled=false on
    // the persisted config is fine — status is read-only and doesn't
    // gate on the kill-switch.
    let router = git_routes::router(app);
    let resp = router
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/git/session/{}/status", sid))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = to_bytes(resp.into_body(), 1_000_000).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["initialized"].as_bool(),
        Some(true),
        "status must report initialized=true after hook_commit; got: {}",
        json
    );
    assert_eq!(
        json["commit_count"].as_u64(),
        Some(1),
        "status must report commit_count=1 after one commit; got: {}",
        json
    );
    assert_eq!(
        json["repo_path"].as_str(),
        Some(pkg.path().to_string_lossy().as_ref()),
        "status must echo the session's package path; got: {}",
        json
    );
    drop(pkg);
}
