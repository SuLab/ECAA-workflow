//! POST `/api/chat/session/:id/stall-signal` — direct relay endpoint
//! from the harness stall-relay thread.
//!
//! Unlike the existing `/progress` path (which is drained by the main
//! harness loop), this endpoint is reached via a dedicated background
//! thread in the harness that bypasses the main loop entirely. This
//! closes the visibility gap when AWS SSM hangs: the main loop is
//! blocked inside `run_iteration()`, but the relay thread is still
//! running and can POST here.
//!
//! On receipt the handler broadcasts a `SsePayload::StallSignalDirect`
//! event so the UI can surface the stall even when no synthetic
//! assistant turn has been produced by the batcher.
//!
//! `session_id` is validated via the path-jail helper to reject
//! traversal segments before they reach the session store.

use super::*;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

/// Wire body from `crates/harness/src/stall_relay.rs::StallSignalBody`.
/// Mirrors the four stall variants as flat JSON so the harness crate
/// need not depend on the server crate's types.
#[derive(Debug, Deserialize)]
pub(super) struct DirectStallSignalRequest {
    pub task_id: String,
    /// "cpu_starvation" | "memory_pressure" | "gpu_idle_during_training"
    /// | "runtime_over_expected"
    pub kind: String,
    pub measurements: serde_json::Value,
    /// "retry" | "resize" | "abort"
    pub suggested_action: String,
}

pub(super) async fn post_stall_signal(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<DirectStallSignalRequest>,
) -> impl IntoResponse {
    // Path-jail: validate task_id before it touches the in-memory
    // session (defence-in-depth for any downstream filesystem join).
    if let Err(e) = safe_segment_join(std::path::Path::new(""), &req.task_id) {
        return (StatusCode::BAD_REQUEST, format!("invalid task_id: {}", e)).into_response();
    }

    tracing::debug!(
        ?session_id,
        task_id = %req.task_id,
        kind = %req.kind,
        "post_stall_signal (direct relay)"
    );

    // Broadcast `stall_signal_direct` SSE event so subscribers see the
    // stall even when the main harness loop is blocked.
    app.broadcast(
        session_id,
        SsePayload::StallSignalDirect {
            task_id: req.task_id.clone(),
            kind: req.kind.clone(),
            measurements: req.measurements.clone(),
            suggested_action: req.suggested_action.clone(),
        },
    )
    .await;

    StatusCode::NO_CONTENT.into_response()
}

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/stall-signal",
        axum::routing::post(post_stall_signal),
    )
}

pub(super) const ROUTES: &[(&str, &str)] = &[("POST", "/api/chat/session/:id/stall-signal")];

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::make_router;
    use crate::chat_routes::{EnvelopedEvent, SsePayload};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    #[tokio::test]
    async fn post_stall_signal_broadcasts_sse_event() {
        let (router, app) = make_router(vec![]).await;
        let session_id = Uuid::new_v4();
        let mut rx = app.broadcaster(session_id).await.subscribe();

        let body = serde_json::json!({
            "task_id": "alignment",
            "kind": "memory_pressure",
            "measurements": { "pct": 93.5, "window_mins": 5 },
            "suggested_action": "resize"
        });

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/stall-signal", session_id))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "endpoint must return 204 on success"
        );

        let envelope: EnvelopedEvent = rx.recv().await.unwrap();
        match envelope.payload {
            SsePayload::StallSignalDirect {
                task_id,
                kind,
                suggested_action,
                ..
            } => {
                assert_eq!(task_id, "alignment");
                assert_eq!(kind, "memory_pressure");
                assert_eq!(suggested_action, "resize");
            }
            other => panic!("expected StallSignalDirect SSE event, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn post_stall_signal_rejects_path_traversal_in_task_id() {
        let (router, _app) = make_router(vec![]).await;
        let session_id = Uuid::new_v4();

        // Embed a path-traversal attempt in the task_id body field.
        let body = serde_json::json!({
            "task_id": "../../../etc/passwd",
            "kind": "cpu_starvation",
            "measurements": {},
            "suggested_action": "retry"
        });
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/stall-signal", session_id))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "path traversal in task_id must be rejected with 400"
        );
    }
}
