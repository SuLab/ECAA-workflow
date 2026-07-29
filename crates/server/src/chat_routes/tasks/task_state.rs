//! POST `/api/chat/session/:id/task/:task_id/state` — the
//! authoritative HTTP surface for harness-binary task-state writes.
//! Routes the harness into the `task_states` source-of-truth (RC-18)
//! so writes are not clobbered by the conversation tool-loop merge.
//!
//! The handler routes through `Session::set_task_state` which:
//! - enforces monotonicity (no Completed/Failed → non-terminal),
//! - invalidates the derived DAG cache on every successful mutation
//!   so the next `current_dag()` read overlays the harness write.
//!
//! `task_id` is validated via the Phase-16 path-jail helper to reject
//! `..`, separators, and other shell-meaningful components that could
//! escape a downstream filesystem join.

use super::super::*;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use ecaa_workflow_core::dag::TaskState;
use serde::Deserialize;
use std::path::Path as StdPath;
use std::sync::Arc;
use uuid::Uuid;

/// Wire body for the state-write endpoint. The `state` field uses the
/// Existing `TaskState` serde shape (`{ "status": "running",... }`)
/// so the harness can ship one `TaskState` value without a separate
/// wire-type translation layer.
#[derive(Debug, Deserialize)]
pub(crate) struct SetTaskStateRequest {
    pub state: TaskState,
}

#[tracing::instrument(skip(app, req), fields(session_id = %session_id, task_id = %task_id))]
pub(crate) async fn post_set_task_state(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    Json(req): Json<SetTaskStateRequest>,
) -> impl IntoResponse {
    // Reject `..`, separators, empty, or absolute `task_id` segments
    // before they reach the in-memory session — defense-in-depth so a
    // future caller that splices `task_id` into a filesystem path can
    // assume the segment is already safe.
    //
    // This site validates input only (no filesystem op) so
    // `assert_under_root` would be a no-op against an empty root. The
    // segment-only check is the right shape here. Any downstream
    // filesystem callers must route through `runtime_outputs_for_task`
    // which performs the full assert.
    if let Err(e) = super::super::safe_segment_join(StdPath::new(""), &task_id) {
        return (StatusCode::BAD_REQUEST, format!("invalid task_id: {}", e)).into_response();
    }
    // Imported (read-only) packages must not have their task states mutated —
    // no harness runs against them, so any write here is illegitimate.
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    if let Err(resp) = crate::chat_routes::package_import::ensure_not_imported(&session) {
        return resp;
    }
    // CV-4 server-side artifact guard. A `Completed` write for a task
    // whose declared `required_artifacts` are missing/empty on disk is
    // refused and demoted to a `[missing_artifact]` blocker, mirroring the
    // harness silent-completion guard (`verify_required_artifacts`) so a
    // phantom artifact cannot escape into the deposit via this route.
    // Evaluated against the pre-write snapshot: the declared artifacts and
    // package root are structural and don't change under the write below.
    let demotion: Option<(TaskState, String)> = if matches!(req.state, TaskState::Completed { .. })
    {
        let missing =
            crate::chat_routes::artifact_guard::missing_declared_artifacts(&session, &task_id);
        if missing.is_empty() {
            None
        } else {
            tracing::warn!(
                %session_id,
                task_id = %task_id,
                missing = ?missing,
                "set_task_state: refused Completed — declared required artifacts missing/empty; demoting to [missing_artifact] blocker"
            );
            let reason =
                crate::chat_routes::artifact_guard::missing_artifact_reason(&task_id, &missing);
            let state =
                crate::chat_routes::artifact_guard::demoted_blocked_state(&task_id, &missing);
            Some((state, reason))
        }
    } else {
        None
    };
    let refused_reason: Option<String> = demotion.as_ref().map(|(_, r)| r.clone());
    let task_id_for_closure = task_id.clone();
    // The state actually written: the demoted `Blocked` state on a refused
    // completion, else the requested state verbatim.
    let new_state = demotion
        .map(|(state, _)| state)
        .unwrap_or_else(|| req.state.clone());
    // Capture both the pre-mutation status (so we can decrement the
    // matching bucket in the cached ProgressSummary) and the resulting
    // status (which may equal the existing terminal value on a refused
    // regression). Share via `Arc<Mutex<...>>` since `store.update` takes
    // a `move |s|` closure and returns `Ok(())`.
    let observed: Arc<std::sync::Mutex<Option<(&'static str, &'static str)>>> =
        Arc::new(std::sync::Mutex::new(None));
    let observed_for_closure = observed.clone();
    let req_state_for_closure = req.state.clone();
    let result = app
        .conversation
        .store_handle()
        .update(session_id, move |s| {
            let old = s
                .task_states
                .get(&task_id_for_closure)
                .cloned()
                .unwrap_or(TaskState::Pending);
            let written = s.set_task_state(&task_id_for_closure, new_state);
            if !matches!(
                (&written, &req_state_for_closure),
                (TaskState::Completed { .. }, TaskState::Completed { .. })
                    | (TaskState::Failed { .. }, TaskState::Failed { .. })
            ) && written.is_terminal()
                && !req_state_for_closure.is_terminal()
            {
                tracing::warn!(
                    %session_id,
                    task_id = %task_id_for_closure,
                    "set_task_state: refused terminal->non-terminal regression"
                );
            }
            *observed_for_closure
                .lock()
                .unwrap_or_else(|p| p.into_inner()) =
                Some((classify_status(&old), classify_status(&written)));
            Ok(())
        })
        .await;
    match result {
        Ok(_) => {
            // Incremental cache update. With both the old and new status
            // from the closure's observation, the cached ProgressSummary
            // can be patched in place without forcing the next
            // `GET /state` to walk the sibling-package dir and reparse
            // `WORKFLOW.json`. Leaves `valid = true` so the cache hits
            // on the next reader.
            let observed_pair = observed.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some((old_status, new_status)) = observed_pair {
                if let Some(mut entry) = app.reconciled_progress_cache.get_mut(&session_id) {
                    let cached = &mut entry.value_mut().progress;
                    match old_status {
                        "completed" => cached.completed = cached.completed.saturating_sub(1),
                        "ready" => cached.ready = cached.ready.saturating_sub(1),
                        "blocked" => cached.blocked = cached.blocked.saturating_sub(1),
                        _ => cached.pending = cached.pending.saturating_sub(1),
                    }
                    match new_status {
                        "completed" => cached.completed += 1,
                        "ready" => cached.ready += 1,
                        "blocked" => cached.blocked += 1,
                        _ => cached.pending += 1,
                    }
                    // Track `blocked_tasks` set membership.
                    let blocked_tasks = &mut entry.value_mut().blocked_tasks;
                    if new_status == "blocked" && !blocked_tasks.contains(&task_id) {
                        blocked_tasks.push(task_id.clone());
                        blocked_tasks.sort();
                    } else if old_status == "blocked" && new_status != "blocked" {
                        blocked_tasks.retain(|t| t != &task_id);
                    }
                } else {
                    // No cache entry yet — leave alone; the next reader
                    // will populate via the full reconciliation walk.
                }
            }
            // Per-task success-rate telemetry: record terminal
            // dispositions only — Completed ⇒ success, Failed ⇒ failure.
            // Non-terminal states (and Blocked, which is recoverable, not
            // a failure) are not dispositions. Keyed by task_id so a
            // rerun's later terminal state overwrites the earlier one.
            // Best-effort; runs only on a successful write.
            match &req.state {
                // A demoted completion (refused_reason set) is NOT a
                // terminal success disposition — it landed as `Blocked` —
                // so it must not be recorded as a completed task.
                TaskState::Completed { .. } if refused_reason.is_none() => {
                    app.conversation
                        .metrics()
                        .record_task_terminal(session_id, task_id.clone(), true)
                        .await;
                }
                TaskState::Failed { .. } => {
                    app.conversation
                        .metrics()
                        .record_task_terminal(session_id, task_id.clone(), false)
                        .await;
                }
                _ => {}
            }
            // A refused completion returns 409 with the demotion reason so
            // the caller (harness) sees the completion was not accepted;
            // the `Blocked` demotion has already been written above.
            if let Some(reason) = refused_reason {
                return (StatusCode::CONFLICT, reason).into_response();
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => (StatusCode::NOT_FOUND, format!("session not found: {}", e)).into_response(),
    }
}

/// Bucket name for the ProgressSummary counters. Mirrors the bucket
/// assignment in `ecaa_workflow_core::dag::DAG::progress` so the
/// incremental update stays consistent with the canonical recompute:
/// `Running` rolls into `ready` (counted as active), `Failed` rolls
/// into `blocked` (counted as needing attention).
fn classify_status(s: &TaskState) -> &'static str {
    match s {
        TaskState::Pending => "pending",
        TaskState::Ready => "ready",
        TaskState::Running { .. } => "ready",
        TaskState::Completed { .. } => "completed",
        TaskState::Failed { .. } => "blocked",
        TaskState::Blocked { .. } => "blocked",
    }
}

pub(crate) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/task/:task_id/state",
        axum::routing::post(post_set_task_state),
    )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{
        make_router, seed_session_with_completed_task, seed_session_with_task_requiring_artifacts,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use ecaa_workflow_core::dag::TaskState;
    use tower::util::ServiceExt;

    /// CV-4: a `Completed` write for a task whose declared
    /// `required_artifacts` are absent on disk must be REFUSED (409) and
    /// the task demoted to a `[missing_artifact]` blocker — never recorded
    /// as `Completed`, so a phantom artifact can't escape via this route.
    #[tokio::test]
    async fn post_set_task_state_refuses_completed_when_required_artifacts_missing() {
        let (router, app) = make_router(vec![]).await;
        let pkg = tempfile::tempdir().unwrap();
        // Task output dir exists but the declared artifact is never written.
        std::fs::create_dir_all(pkg.path().join("runtime/outputs/de")).unwrap();
        let id = seed_session_with_task_requiring_artifacts(
            &app,
            "de",
            pkg.path().to_path_buf(),
            &["results/de.tsv"],
        )
        .await;
        let body = serde_json::json!({
            "state": { "status": "completed", "result": { "ok": true } }
        });
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/task/de/state", id))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::CONFLICT,
            "missing required artifact must refuse the completion with 409"
        );
        let session = app.conversation.get_session(id).await.unwrap();
        match session.task_states.get("de") {
            Some(TaskState::Blocked { record }) => {
                assert!(
                    record.reason.starts_with("[missing_artifact]"),
                    "demotion reason must carry the [missing_artifact] marker: {}",
                    record.reason
                );
            }
            other => panic!("expected Blocked demotion, got {:?}", other),
        }
    }

    /// CV-4: a `Completed` write is ACCEPTED (204, recorded `Completed`)
    /// when every declared `required_artifact` exists and is non-empty.
    #[tokio::test]
    async fn post_set_task_state_accepts_completed_when_required_artifacts_present() {
        let (router, app) = make_router(vec![]).await;
        let pkg = tempfile::tempdir().unwrap();
        let artifact = pkg.path().join("runtime/outputs/de/results/de.tsv");
        std::fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        std::fs::write(&artifact, b"gene\tlog2fc\nCRISPLD2\t2.61\n").unwrap();
        let id = seed_session_with_task_requiring_artifacts(
            &app,
            "de",
            pkg.path().to_path_buf(),
            &["results/de.tsv"],
        )
        .await;
        let body = serde_json::json!({
            "state": { "status": "completed", "result": { "ok": true } }
        });
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/task/de/state", id))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "present required artifact must accept the completion"
        );
        let session = app.conversation.get_session(id).await.unwrap();
        assert!(
            matches!(
                session.task_states.get("de"),
                Some(TaskState::Completed { .. })
            ),
            "completion with present artifacts must be recorded Completed: got {:?}",
            session.task_states.get("de")
        );
    }

    #[tokio::test]
    async fn post_set_task_state_writes_through_task_states_authoritative_map() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t1", None).await;
        // Write a non-terminal state for a NEW task id (no existing
        // terminal entry to block) via the new endpoint.
        let body = serde_json::json!({
            "state": { "status": "ready" }
        });
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/task/t2/state", id))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "endpoint should return 204 on success"
        );
        // The write must land in `task_states` (the post-Phase-D
        // authority), not just the legacy `session.dag` cache.
        let session = app.conversation.get_session(id).await.unwrap();
        assert!(
            matches!(session.task_states.get("t2"), Some(TaskState::Ready)),
            "harness write should reach task_states authoritative map: got {:?}",
            session.task_states.get("t2")
        );
    }

    #[tokio::test]
    async fn post_set_task_state_returns_404_for_unknown_session() {
        let (router, _app) = make_router(vec![]).await;
        let unknown = uuid::Uuid::new_v4();
        let body = serde_json::json!({ "state": { "status": "ready" } });
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/task/t1/state", unknown))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NOT_FOUND,
            "unknown session must 404"
        );
    }

    #[tokio::test]
    async fn post_set_task_state_rejects_path_traversal_in_task_id() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t1", None).await;
        let body = serde_json::json!({ "state": { "status": "ready" } });
        // The dot-dot literal is rejected by the path-jail helper; we
        // pick a separator-free form so axum's path extractor accepts
        // the segment before our handler validates it.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/task/../state", id))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        // Axum may either route `..` as a literal segment (handler
        // returns 400) or refuse to extract (404); both prevent the
        // unsafe segment from reaching the session store.
        assert!(
            matches!(
                resp.status(),
                StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND
            ),
            "path traversal must be rejected before write; got {}",
            resp.status(),
        );
    }

    #[tokio::test]
    async fn post_set_task_state_enforces_monotonicity_via_session_setter() {
        // The setter-level guard is exercised end-to-end via the HTTP
        // endpoint: write Completed via POST, then try to regress to
        // Running and verify task_states still shows Completed.
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t1", None).await;
        // First POST: Completed for t9.
        let body1 = serde_json::json!({
            "state": { "status": "completed", "result": { "ok": true } }
        });
        let req1 = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/task/t9/state", id))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body1).unwrap()))
            .unwrap();
        let resp1 = router.clone().oneshot(req1).await.unwrap();
        assert_eq!(resp1.status(), StatusCode::NO_CONTENT);
        // Second POST: try to regress to Running.
        let body2 = serde_json::json!({
            "state": { "status": "running", "started_at": "2026-05-13T00:00:00Z" }
        });
        let req2 = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/task/t9/state", id))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body2).unwrap()))
            .unwrap();
        let resp2 = router.oneshot(req2).await.unwrap();
        // Endpoint still returns 204 because the session is internally
        // consistent — the setter refused the regression silently.
        assert_eq!(resp2.status(), StatusCode::NO_CONTENT);
        let session = app.conversation.get_session(id).await.unwrap();
        assert!(
            matches!(
                session.task_states.get("t9"),
                Some(TaskState::Completed { .. })
            ),
            "monotonicity guard must preserve Completed despite regression POST: got {:?}",
            session.task_states.get("t9")
        );
    }
}
