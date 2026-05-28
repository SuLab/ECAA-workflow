//! Execution-status read endpoints. `get_execution`
//! aggregates the live `ExecutionHandle` state into the
//! `ExecutionStatusResponse` shape (with the four-way status enum:
//! running / pausing / paused / stopping / exited). `get_dag` returns
//! the session's built `WORKFLOW.json`-equivalent DAG; lives here
//! because the Jobs tab in the StateInspector consumes both endpoints
//! together.

use super::super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use uuid::Uuid;

// Note: `get_execution` returns `200 OK` with body `null` when the
// session has no in-memory `ExecutionHandle` — "execution not started
// yet" is a valid pollable state, not a missing resource. Returning 404
// here made the browser console log a "Failed to load resource" for
// every 3s `useConversation` poll while the SME was still composing the
// intake; the UI client (`jsonFetchOrNull`) collapses both 404 and 200
// null to `null` so no functional change. The 404 path is reserved for
// the unrelated "session doesn't exist" case (handled by the
// `/api/chat/session/:id` extractor before this handler runs).

/// `GET /api/chat/session/:id/execution` — return the current harness execution status.
pub async fn get_execution(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let handle = app.executions.get(&session_id).map(|e| e.value().clone());
    match handle {
        Some(h) => {
            let exit_code = h.exit_status_get();
            let paused_at = *super::lock_recover(&h.paused_at);
            let stop_requested_at = *super::lock_recover(&h.stop_requested_at);
            let stop_requested = h.stop_requested.load(std::sync::atomic::Ordering::Acquire);
            let pause_requested = h.pause_requested.load(std::sync::atomic::Ordering::Acquire);
            let paused_acked = tokio::fs::metadata(h.package_dir.join("runtime/.harness-paused"))
                .await
                .is_ok();
            // `pause_requested && !paused_acked` is the in-flight
            // window — the user clicked Pause but the harness is mid
            // agent-dispatch and hasn't reached the top of its
            // iteration loop yet (where the sentinel is observed +
            // ack'd). Returning "running" during this window left the
            // UI with no Resume button (Resume only renders on
            // status="paused"), so a user who paused mid-iteration
            // had no way to back out short of force-kill. Surface
            // "pausing" instead so the UI can render Resume + Stop +
            // Force-kill exactly like the fully-paused state.
            let status = if exit_code.is_some() {
                "exited"
            } else if stop_requested {
                "stopping"
            } else if pause_requested && paused_acked {
                "paused"
            } else if pause_requested {
                "pausing"
            } else {
                "running"
            };
            Json(ExecutionStatusResponse {
                pid: h.pid,
                pgid: h.pgid,
                started_at: h.started_at,
                package_dir: h.package_dir,
                agent_command: h.agent_command,
                status: status.into(),
                exit_code,
                paused_at,
                stop_requested_at,
            })
            .into_response()
        }
        None => Json(serde_json::Value::Null).into_response(),
    }
}

/// `GET /api/chat/session/:id/dag` — return the session's current `WorkflowDag` as JSON.
pub async fn get_dag(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    match app.conversation.get_session(session_id).await {
        Some(session) => match session.current_dag() {
            Some(dag) => Json(dag).into_response(),
            // Pre-emit conversation is a valid pollable state, not a missing
            // resource — return 200 + null so the UI's pre-emit /dag poll
            // doesn't spam "Failed to load resource" in the browser console.
            None => Json(serde_json::Value::Null).into_response(),
        },
        None => (StatusCode::NOT_FOUND, "session not found").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::test_support::{make_router, seed_session_with_completed_task};
    use super::super::super::ExecutionHandle;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::Utc;
    use tempfile::TempDir;
    use tower::util::ServiceExt;
    use uuid::Uuid;

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

    #[tokio::test]
    async fn get_execution_returns_null_when_nothing_started() {
        // Regression: previously returned 404 + "no execution for
        // session". The UI client (`jsonFetchOrNull`) collapses 404
        // to `null`, but the browser logs every 404 as "Failed to load
        // resource" — dozens of these accumulated in the console while
        // SMEs were composing intake (the 3s `useConversation` poll
        // fires the moment a session exists but no execution has been
        // started). "Execution not started yet" is a valid pollable
        // state, not a missing resource — return 200 + null instead.
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/execution", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = crate::chat_routes::test_support::body_json(resp.into_body()).await;
        assert!(
            body.is_null(),
            "expected JSON null when no execution started, got {body}",
        );
    }

    #[tokio::test]
    async fn get_execution_reports_pausing_when_pause_unacked() {
        // Regression: pause_requested without the.harness-paused ack
        // file used to report status="running", which left the UI with
        // no Resume button (the SME couldn't back out of a pause that
        // the harness hadn't observed yet because it was mid-iteration).
        // Now the in-flight window reports "pausing" so the UI can
        // render Resume + Stop + Force-kill.
        let (router, app) = make_router(vec![]).await;
        let (id, pkg) = seed_running_execution(&app, 999_995).await;
        // Simulate /pause having flipped the atomic + written the
        // sentinel, but the harness hasn't ack'd yet.
        {
            let entry = app.executions.get(&id).unwrap();
            let h = entry.value();
            h.pause_requested
                .store(true, std::sync::atomic::Ordering::Release);
            *super::super::lock_recover(&h.paused_at) = Some(Utc::now());
        }
        std::fs::write(pkg.path().join("runtime/.harness-pause"), b"requested").unwrap();
        // Note: NO.harness-paused ack file written.

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/execution", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = crate::chat_routes::test_support::body_json(resp.into_body()).await;
        assert_eq!(
            body["status"].as_str().unwrap(),
            "pausing",
            "pause_requested without ack must report `pausing`, not `running`",
        );
        assert!(body["paused_at"].is_string());
    }

    #[tokio::test]
    async fn get_execution_reports_paused_only_after_ack() {
        // Companion: once the harness writes.harness-paused ack,
        // status flips from "pausing" to "paused".
        let (router, app) = make_router(vec![]).await;
        let (id, pkg) = seed_running_execution(&app, 999_994).await;
        {
            let entry = app.executions.get(&id).unwrap();
            let h = entry.value();
            h.pause_requested
                .store(true, std::sync::atomic::Ordering::Release);
            *super::super::lock_recover(&h.paused_at) = Some(Utc::now());
        }
        std::fs::write(pkg.path().join("runtime/.harness-pause"), b"requested").unwrap();
        std::fs::write(pkg.path().join("runtime/.harness-paused"), b"ack").unwrap();

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/execution", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let body = crate::chat_routes::test_support::body_json(resp.into_body()).await;
        assert_eq!(body["status"].as_str().unwrap(), "paused");
    }
}
