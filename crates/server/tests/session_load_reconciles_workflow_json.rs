//! Integration test: on session load (cache miss after server restart),
//! `Session::task_states` is reconciled against the on-disk
//! `WORKFLOW.json` of the emitted package.
//!
//! Bug reproducer (paraphrased from the report): during a brief server
//! restart, the harness keeps writing task transitions to its local
//! `WORKFLOW.json` and POSTs to `/api/chat/session/<id>/task/<tid>/state`
//! return 404 (server momentarily unaware of the session). After the
//! restart, the persisted session JSON file has stale `task_states`
//! (last successfully POST'd snapshot), while the on-disk
//! `WORKFLOW.json` reflects the newer harness writes. Without
//! reconciliation, every `current_dag()` consumer (dashboard, summary,
//! execution status, task results, impact) and the `/state` endpoint
//! itself overlay the stale snapshot.
//!
//! This test creates that exact disk shape (session with 2 completed in
//! `task_states`, WORKFLOW.json with 5 completed at
//! `emitted_package_path`), boots a fresh `ChatAppState` (process
//! restart), then asserts `GET /state` returns 5 completed.

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use scripps_workflow_conversation::{LlmBackend, MockLlmBackend, Session, SessionStore};
use scripps_workflow_core::dag::TaskState;
use scripps_workflow_server::chat_routes;
use std::path::Path;
use std::sync::Arc;
use tower::util::ServiceExt;

fn config_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

/// Build a `ChatAppState` over an existing on-disk session directory
/// (simulates the "process restart that re-opens the same store" path).
async fn make_router_over(sessions_dir: &Path) -> (axum::Router, chat_routes::ChatAppState) {
    let store = SessionStore::open(sessions_dir).await.unwrap();
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(vec![]));
    let app = chat_routes::ChatAppState::with_backend(backend, store, config_dir());
    let router = chat_routes::router(app.clone()).layer(axum::Extension(
        scripps_workflow_server::auth::RequestPrincipal::test_default(),
    ));
    (router, app)
}

async fn body_json(body: Body) -> serde_json::Value {
    let bytes = to_bytes(body, 1_000_000).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn server_restart_reconciles_stale_task_states_against_workflow_json() {
    // Layout:
    //   <sessions_dir>/<session_id>.json      session with stale task_states
    //   <package_dir>/WORKFLOW.json           harness-truth: 5 completed
    let sessions_dir = tempfile::tempdir().unwrap();
    let package_dir = tempfile::tempdir().unwrap();

    // Hand-craft a session whose `task_states` map records ONLY the
    // first 2 completions (everything the server saw before its brief
    // outage). `emitted_package_path` points at the package directory
    // where the harness keeps writing to `WORKFLOW.json`.
    let mut session = Session::new(false);
    session.emitted_package_path = Some(package_dir.path().to_path_buf());
    session.task_states.insert(
        "task_1".to_string(),
        TaskState::Completed {
            result: serde_json::json!({"ok": true}),
        },
    );
    session.task_states.insert(
        "task_2".to_string(),
        TaskState::Completed {
            result: serde_json::json!({"ok": true}),
        },
    );
    let session_id = session.id;
    let session_path = sessions_dir
        .path()
        .join(format!("{}.json", session_id.as_hyphenated()));
    let bytes = serde_json::to_vec_pretty(&session).unwrap();
    tokio::fs::write(&session_path, &bytes).await.unwrap();

    // WORKFLOW.json reflects the harness's view AFTER the outage —
    // 5 completed tasks (task_1..task_5), one blocked task.
    let workflow_json = serde_json::json!({
        "tasks": {
            "task_1": {"state": {"status": "completed", "result": {}}},
            "task_2": {"state": {"status": "completed", "result": {}}},
            "task_3": {"state": {"status": "completed", "result": {}}},
            "task_4": {"state": {"status": "completed", "result": {}}},
            "task_5": {"state": {"status": "completed", "result": {}}},
            "task_blocker": {
                "state": {
                    "status": "blocked",
                    "record": {"reason": "needs input", "attempts": []}
                }
            }
        }
    });
    tokio::fs::write(
        package_dir.path().join("WORKFLOW.json"),
        serde_json::to_vec(&workflow_json).unwrap(),
    )
    .await
    .unwrap();

    // "Boot the server" — open a fresh ChatAppState over the on-disk
    // session directory.
    let (router, _app) = make_router_over(sessions_dir.path()).await;

    // First GET /state forces `ensure_loaded` → reconciliation path.
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/chat/session/{}/state", session_id))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;

    // Without the fix this would be 2 (stale `session.task_states`).
    // With the fix the `/state` aggregate reflects WORKFLOW.json reality.
    assert_eq!(
        body["progress"]["completed"].as_u64(),
        Some(5),
        "GET /state must reflect on-disk WORKFLOW.json after server restart: {body}"
    );
    assert_eq!(
        body["progress"]["blocked"].as_u64(),
        Some(1),
        "blocked count must reflect WORKFLOW.json reality: {body}"
    );
    assert_eq!(
        body["blocked_tasks"].as_array().unwrap(),
        &vec![serde_json::json!("task_blocker")],
        "blocked_tasks list must come from WORKFLOW.json: {body}"
    );
}

#[tokio::test]
async fn server_restart_skips_reconciliation_for_session_without_package() {
    // Session in greeting/intake state has no `emitted_package_path`,
    // so reconciliation is a no-op. This is the regression guard for
    // "don't break sessions that haven't emitted yet" — the load path
    // must not pay any disk-read cost for sessions without packages.
    let sessions_dir = tempfile::tempdir().unwrap();
    let session = Session::new(false);
    assert!(session.emitted_package_path.is_none());
    let session_id = session.id;
    let session_path = sessions_dir
        .path()
        .join(format!("{}.json", session_id.as_hyphenated()));
    let bytes = serde_json::to_vec_pretty(&session).unwrap();
    tokio::fs::write(&session_path, &bytes).await.unwrap();

    let (router, _app) = make_router_over(sessions_dir.path()).await;
    let req = Request::builder()
        .method("GET")
        .uri(format!("/api/chat/session/{}/state", session_id))
        .body(Body::empty())
        .unwrap();
    let resp = router.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp.into_body()).await;
    // No package = nothing reconciled. The pre-existing fallback path
    // returns a zero progress summary.
    assert_eq!(body["progress"]["completed"].as_u64(), Some(0));
}
