//! V3 lifecycle adjudication queue endpoints (design §7).
//!
//! Two routes:
//!
//! - `GET /api/chat/session/:id/adjudication` — list queue entries.
//! - `POST /api/chat/session/:id/adjudication/:entry_id/resolve` — resolve.
//!
//! Resolving an entry transitions the queue entry to
//! `AdjudicationStatus::Resolved`, records a
//! `DecisionType::LifecycleTransition` entry with the resolution
//! payload, and clears the entry from the active queue so the next
//! `rebuild_dag` does not re-fire the same `BlockerKind::AdjudicationRequired`.

use super::{BoundedJson, ChatAppState};
use axum::extract::Path;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use ecaa_workflow_core::decision_log::{DecisionActor, DecisionType};
use ecaa_workflow_core::lifecycle_adversarial::{AdjudicationQueueEntry, AdjudicationStatus};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub(super) struct ResolveBody {
    /// Who is resolving the entry (SME username or operator id).
    pub decided_by: String,
    /// Free-text decision narrative.
    pub decision: String,
}

/// `GET /api/chat/session/:id/adjudication` — return every queue
/// entry on the session in insertion order.
pub(super) async fn list_queue(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let session = match app.conversation.get_session(session_id).await {
        Some(s) => s,
        None => return (StatusCode::NOT_FOUND, "session not found").into_response(),
    };
    Json::<Vec<AdjudicationQueueEntry>>(session.adjudication_queue.clone()).into_response()
}

/// `POST /api/chat/session/:id/adjudication/:entry_id/resolve` —
/// resolve a queue entry. Records a `LifecycleTransition`
/// decision-log row reflecting the resolution choice and marks the
/// entry `Resolved`.
pub(super) async fn resolve(
    State(app): State<ChatAppState>,
    Path((session_id, entry_id)): Path<(Uuid, String)>,
    BoundedJson(body): BoundedJson<ResolveBody>,
) -> impl IntoResponse {
    if body.decided_by.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "decided_by must be a non-empty string",
        )
            .into_response();
    }
    if body.decision.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "decision must be a non-empty string",
        )
            .into_response();
    }
    let store = app.conversation.store_handle();
    let decided_by = body.decided_by;
    let decision = body.decision;
    let entry_id_clone = entry_id.clone();
    let result = store
        .update(session_id, move |s| {
            let pos = s
                .adjudication_queue
                .iter()
                .position(|e| e.id == entry_id_clone);
            match pos {
                Some(idx) => {
                    let entry = &mut s.adjudication_queue[idx];
                    if entry.status.is_terminal() {
                        return Ok(()); // idempotent — already resolved.
                    }
                    let kind = entry.transition.kind().to_string();
                    entry.status = AdjudicationStatus::Resolved {
                        decided_by: decided_by.clone(),
                        decision: decision.clone(),
                        decided_at: ecaa_workflow_core::time_helpers::now_rfc3339(),
                    };
                    let payload = serde_json::to_string(&entry.transition)
                        .unwrap_or_else(|_| String::from("{}"));
                    s.record_decision(
                        DecisionType::LifecycleTransition {
                            transition_kind: format!("{kind}:resolved"),
                            payload,
                        },
                        DecisionActor::Sme,
                        Some(format!("{}: {}", decided_by, decision)),
                    );
                    Ok(())
                }
                None => Err(anyhow::anyhow!(
                    "adjudication entry {} not found",
                    entry_id_clone
                )),
            }
        })
        .await;
    match result {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    }
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/chat/session/:id/adjudication"),
    (
        "POST",
        "/api/chat/session/:id/adjudication/:entry_id/resolve",
    ),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/adjudication",
            axum::routing::get(list_queue),
        )
        .route(
            "/api/chat/session/:id/adjudication/:entry_id/resolve",
            axum::routing::post(resolve),
        )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{body_json, make_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn list_queue_returns_empty_for_new_session() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/adjudication", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body.is_array());
        assert_eq!(body.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn resolve_rejects_empty_decided_by() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/adjudication/x/resolve", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"decided_by": "", "decision": "approve"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
