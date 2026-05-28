//! Pause + resume sentinel handlers. Both flip an
//! atomic on the in-memory `ExecutionHandle` and write/remove a
//! sentinel file under `runtime/` that the harness observes at the
//! top of its iteration loop. Idempotent. The status report from
//! `get_execution` distinguishes "pausing" (request issued but not
//! yet ack'd by `runtime/.harness-paused`) from "paused" (ack file
//! present).

use super::super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use uuid::Uuid;

/// `POST /api/chat/session/:id/execution/pause`
///
/// Sets `pause_requested = true` and writes the
/// `runtime/.harness-pause` sentinel. The harness self-suspends at
/// the top of its next iteration. Returns 409 if no execution alive,
/// or 200 with the updated status otherwise. Idempotent.
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub async fn post_pause_execution(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let handle = app.executions.get(&session_id).map(|e| e.value().clone());
    let Some(h) = handle else {
        return (StatusCode::NOT_FOUND, "no execution for session").into_response();
    };
    if h.exit_status_get().is_some() {
        return (
            StatusCode::CONFLICT,
            "execution already exited; pause has no effect",
        )
            .into_response();
    }
    h.pause_requested
        .store(true, std::sync::atomic::Ordering::Release);
    *super::lock_recover(&h.paused_at) = Some(chrono::Utc::now());
    let sentinel = h.package_dir.join("runtime/.harness-pause");
    if let Err(e) = tokio::fs::write(&sentinel, b"requested\n").await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("writing pause sentinel: {e}"),
        )
            .into_response();
    }
    Json(serde_json::json!({"status":"pausing","sentinel": sentinel.to_string_lossy()}))
        .into_response()
}

/// `POST /api/chat/session/:id/execution/resume`
///
/// Clears `pause_requested` + the sentinel files. Harness picks up
/// where it left off on its next iteration check. Idempotent.
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub async fn post_resume_execution(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let handle = app.executions.get(&session_id).map(|e| e.value().clone());
    let Some(h) = handle else {
        return (StatusCode::NOT_FOUND, "no execution for session").into_response();
    };
    h.pause_requested
        .store(false, std::sync::atomic::Ordering::Release);
    *super::lock_recover(&h.paused_at) = None;
    let _ = tokio::fs::remove_file(h.package_dir.join("runtime/.harness-pause")).await;
    let _ = tokio::fs::remove_file(h.package_dir.join("runtime/.harness-paused")).await;
    Json(serde_json::json!({"status":"resuming"})).into_response()
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::{make_router, seed_session_with_completed_task};
    use super::super::super::ExecutionHandle;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::{Duration, Utc};
    use tempfile::TempDir;
    use tower::util::ServiceExt;
    use uuid::Uuid;

    /// Build + insert a running-state handle into app.executions and
    /// return (sessionId, package_dir tempdir kept alive). exit_status
    /// is None so the handlers see "still running".
    async fn seed_running_execution(
        app: &crate::chat_routes::ChatAppState,
        pgid: u32,
    ) -> (Uuid, TempDir) {
        let pkg = TempDir::new().unwrap();
        std::fs::create_dir_all(pkg.path().join("runtime")).unwrap();
        let id =
            seed_session_with_completed_task(app, "t_demo", Some(pkg.path().to_path_buf())).await;

        // `for_running` constructor.
        let handle = ExecutionHandle::for_running(
            pgid,
            pgid,
            pkg.path().to_path_buf(),
            "scripts/agent-claude.sh".into(),
        );
        app.executions.insert(id, handle);
        (id, pkg)
    }

    async fn seed_exited_execution(app: &crate::chat_routes::ChatAppState) -> (Uuid, TempDir) {
        let pkg = TempDir::new().unwrap();
        std::fs::create_dir_all(pkg.path().join("runtime")).unwrap();
        let id =
            seed_session_with_completed_task(app, "t_demo", Some(pkg.path().to_path_buf())).await;

        // `for_exited` constructor. Overrides
        // `started_at` so the "60s ago" relative timing the tests
        // depend on stays intact.
        let mut handle = ExecutionHandle::for_exited(12345, 12345, 0);
        handle.started_at = Utc::now() - Duration::seconds(60);
        handle.package_dir = pkg.path().to_path_buf();
        handle.agent_command = "scripts/agent-claude.sh".into();
        app.executions.insert(id, handle);
        (id, pkg)
    }

    // pause ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn pause_404_when_no_execution() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/execution/pause", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn pause_writes_sentinel_and_flips_flag() {
        let (router, app) = make_router(vec![]).await;
        // pid 999_999 is almost certainly not a live pgid; that's
        // irrelevant to pause (no syscall). The handler only writes
        // a sentinel file and flips the atomic.
        let (id, pkg) = seed_running_execution(&app, 999_999).await;

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/execution/pause", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Sentinel file written.
        assert!(pkg.path().join("runtime/.harness-pause").exists());
        // pause_requested flipped, paused_at populated.
        let entry = app.executions.get(&id).unwrap();
        let h = entry.value();
        assert!(h.pause_requested.load(std::sync::atomic::Ordering::Acquire));
        assert!(super::super::lock_recover(&h.paused_at).is_some());
    }

    #[tokio::test]
    async fn pause_409_when_already_exited() {
        let (router, app) = make_router(vec![]).await;
        let (id, _pkg) = seed_exited_execution(&app).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/execution/pause", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // resume ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn resume_404_when_no_execution() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/execution/resume", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn resume_clears_pause_flag_and_sentinel() {
        let (router, app) = make_router(vec![]).await;
        let (id, pkg) = seed_running_execution(&app, 999_998).await;
        // Pre-flip into "paused" so resume has something to clear.
        {
            let entry = app.executions.get(&id).unwrap();
            let h = entry.value();
            h.pause_requested
                .store(true, std::sync::atomic::Ordering::Release);
            *super::super::lock_recover(&h.paused_at) = Some(Utc::now());
        }
        std::fs::write(pkg.path().join("runtime/.harness-pause"), b"x").unwrap();
        std::fs::write(pkg.path().join("runtime/.harness-paused"), b"x").unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/execution/resume", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        assert!(!pkg.path().join("runtime/.harness-pause").exists());
        assert!(!pkg.path().join("runtime/.harness-paused").exists());
        let entry = app.executions.get(&id).unwrap();
        let h = entry.value();
        assert!(!h.pause_requested.load(std::sync::atomic::Ordering::Acquire));
        assert!(super::super::lock_recover(&h.paused_at).is_none());
    }
}
