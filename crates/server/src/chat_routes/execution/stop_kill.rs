//! Cooperative stop + hard kill handlers. `post_stop_execution`
//! is the polite path: writes `runtime/.harness-stop`, harness
//! self-finalizes on its next iteration. `post_kill_execution` is the
//! atomic SIGTERM-pgroup path used when the harness is hung and won't
//! observe the sentinel.

use super::super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use uuid::Uuid;

/// `POST /api/chat/session/:id/execution/stop`
///
/// Requests cooperative shutdown of the running harness by writing a
/// `runtime/.harness-stop` sentinel. The harness checks this sentinel at
/// the top of each iteration loop; a currently-running agent invocation is
/// not interrupted. The harness exits after the in-flight agent returns and
/// the next iteration-boundary check observes the sentinel.
///
/// For immediate cancellation of a hung agent, use `/execution/kill`, which
/// SIGTERMs the entire harness process group.
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub async fn post_stop_execution(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let handle = app.executions.get(&session_id).map(|e| e.value().clone());
    let Some(h) = handle else {
        return (StatusCode::NOT_FOUND, "no execution for session").into_response();
    };
    if h.exit_status_get().is_some() {
        return (StatusCode::CONFLICT, "execution already exited").into_response();
    }
    h.stop_requested
        .store(true, std::sync::atomic::Ordering::Release);
    *super::lock_recover(&h.stop_requested_at) = Some(chrono::Utc::now());
    let sentinel = h.package_dir.join("runtime/.harness-stop");
    // Best-effort control sentinel: if the server crashes immediately
    // after this write, the SME can re-request stop on restart.
    if let Err(e) = tokio::fs::write(&sentinel, b"requested\n").await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("writing stop sentinel: {e}"),
        )
            .into_response();
    }
    Json(serde_json::json!({
        "status": "stop_requested",
        "semantics": "after_current_iteration",
        "kill_endpoint": format!("/api/chat/session/{session_id}/execution/kill"),
        "sentinel": sentinel.to_string_lossy(),
    }))
    .into_response()
}

/// `POST /api/chat/session/:id/execution/kill`
///
/// Hard kill — SIGTERM the entire harness pgid, taking down the
/// harness + agent-claude.sh + npm exec + claude subtree atomically.
/// Skips the cooperative-stop dance; leaves WORKFLOW.json in whatever
/// state the harness last wrote. Idempotent — sending kill on an
/// already-exited execution is a no-op.
///
/// Returns:
/// - 200 with `{killed: true, pgid}` on success
/// - 404 if no execution
/// - 409 if execution already exited (kill is a no-op)
/// - 500 on syscall error
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub async fn post_kill_execution(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let handle = app.executions.get(&session_id).map(|e| e.value().clone());
    let Some(h) = handle else {
        return (StatusCode::NOT_FOUND, "no execution for session").into_response();
    };
    if h.exit_status_get().is_some() {
        return (StatusCode::CONFLICT, "execution already exited").into_response();
    }
    let pgid = h.pgid;
    // Mark stop_requested so any racing /execution probe sees a
    // sensible state during the brief window between kill and reap.
    h.stop_requested
        .store(true, std::sync::atomic::Ordering::Release);
    *super::lock_recover(&h.stop_requested_at) = Some(chrono::Utc::now());

    // Negative pid → kill the entire process group.
    #[cfg(unix)]
    {
        let pgid_i32 = pgid as i32;
        // libc::kill is `unsafe` because the kernel signal API is
        // FFI; the unsafety surface is the bare syscall, not our
        // logic. Workspace lint is `unsafe_code = "forbid"` (S5.32);
        // this is the bounded waiver. We checked `pgid != 0` so the
        // negative-pid form refers to our own process group, not "all
        // processes the caller can signal".
        #[allow(unsafe_code)]
        let r = unsafe { libc::kill(-pgid_i32, libc::SIGTERM) };
        if r != 0 {
            let errno = std::io::Error::last_os_error();
            // ESRCH (no such process) is fine — already reaped.
            if errno.raw_os_error() != Some(libc::ESRCH) {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("kill -TERM -{pgid}: {errno}"),
                )
                    .into_response();
            }
        }
    }
    Json(serde_json::json!({"killed": true, "pgid": pgid})).into_response()
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::{
        body_json, make_router, seed_session_with_completed_task,
    };
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

        // `for_exited` constructor with overrides
        // for `started_at` (60s ago) + per-package path so the test
        // assertions stay intact.
        let mut handle = ExecutionHandle::for_exited(12345, 12345, 0);
        handle.started_at = Utc::now() - Duration::seconds(60);
        handle.package_dir = pkg.path().to_path_buf();
        handle.agent_command = "scripts/agent-claude.sh".into();
        app.executions.insert(id, handle);
        (id, pkg)
    }

    // stop ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn stop_404_when_no_execution() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/execution/stop", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn stop_writes_sentinel_and_flips_flag() {
        let (router, app) = make_router(vec![]).await;
        let (id, pkg) = seed_running_execution(&app, 999_997).await;

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/execution/stop", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["status"], "stop_requested");
        assert_eq!(body["semantics"], "after_current_iteration");
        assert_eq!(
            body["kill_endpoint"],
            format!("/api/chat/session/{}/execution/kill", id)
        );
        assert!(
            body["sentinel"]
                .as_str()
                .is_some_and(|p| p.ends_with("runtime/.harness-stop")),
            "stop response should expose the sentinel path"
        );

        assert!(pkg.path().join("runtime/.harness-stop").exists());
        let entry = app.executions.get(&id).unwrap();
        let h = entry.value();
        assert!(h.stop_requested.load(std::sync::atomic::Ordering::Acquire));
        assert!(super::super::lock_recover(&h.stop_requested_at).is_some());
    }

    #[tokio::test]
    async fn stop_409_when_already_exited() {
        let (router, app) = make_router(vec![]).await;
        let (id, _pkg) = seed_exited_execution(&app).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/execution/stop", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    // kill ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn kill_404_when_no_execution() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/execution/kill", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn kill_409_when_already_exited() {
        let (router, app) = make_router(vec![]).await;
        let (id, _pkg) = seed_exited_execution(&app).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/execution/kill", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn kill_treats_esrch_as_success() {
        // An out-of-range pgid yields ESRCH on kill(-pgid, SIGTERM),
        // which the handler folds into a 200 (already-reaped path).
        let (router, app) = make_router(vec![]).await;
        let (id, _pkg) = seed_running_execution(&app, 999_996).await;

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/execution/kill", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // stop_requested gets flipped on the kill path so a racing
        // /execution probe sees a sensible state.
        let entry = app.executions.get(&id).unwrap();
        let h = entry.value();
        assert!(h.stop_requested.load(std::sync::atomic::Ordering::Acquire));
    }
}
