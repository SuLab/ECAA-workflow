//! `POST /api/chat/session/:id/explain`
//! §3.3 "Explain this for me" endpoint.
//!
//! Ask Haiku 4.5 to rewrite a technical snippet (blocker reason,
//! narrative fragment, stage description) in plain language. Billed
//! through the side-call cost bucket (`side_call_cost_usd`). Cached
//! 60 minutes at module scope in the conversation crate so a repeat
//! click doesn't re-bill.

use super::{ChatAppState, LlmRateBuckets};
use axum::extract::Path;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use ecaa_workflow_conversation::side_calls::explain as explain_side_call;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub(super) struct ExplainRequest {
    pub text: String,
    #[serde(default)]
    pub context: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct ExplainResponse {
    pub explanation: String,
    pub model: String,
    pub cached: bool,
}

pub(super) async fn post_explain(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<ExplainRequest>,
) -> impl IntoResponse {
    // explain endpoint hits Haiku 4.5 via a side-call. 30/min is
    // generous enough for "click every blocker, every figure" but
    // tight enough that a runaway client doesn't drain side_call_cost_usd.
    if let Err(status) = LlmRateBuckets::check(
        &app.llm_buckets.explain,
        session_id,
        app.llm_rate_limits.explain,
    ) {
        return (
            status,
            "rate limit exceeded: /explain capped at 30/min/session",
        )
            .into_response();
    }

    if req.text.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "empty text").into_response();
    }
    if app.conversation.get_session(session_id).await.is_none() {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }

    let backend = app.conversation.llm_for_scoring();
    let metrics = app.conversation.metrics();
    match explain_side_call::explain(
        backend,
        metrics,
        session_id,
        &req.text,
        req.context.as_deref(),
    )
    .await
    {
        Ok(r) => Json(ExplainResponse {
            explanation: r.explanation,
            model: r.model,
            cached: r.cached,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("explain failed: {}", e),
        )
            .into_response(),
    }
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[("POST", "/api/chat/session/:id/explain")];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/explain",
        axum::routing::post(post_explain),
    )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{assistant, body_json, make_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    #[tokio::test]
    async fn post_explain_rejects_empty_text() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/explain", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"text":"   "}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_explain_404_for_unknown_session() {
        let (router, _) = make_router(vec![]).await;
        let bogus = Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/explain", bogus))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"text":"hello"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_explain_returns_explanation_via_mock_backend() {
        // Side-call routes through `app.conversation.llm_for_scoring()`,
        // which in test mode returns the same MockLlmBackend the main
        // conversation uses. One scripted assistant response is enough
        // because explain only sends one TurnRequest.
        let (router, app) = make_router(vec![assistant(
            "In plain English: this means the analysis cleared the gate.",
        )])
        .await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        // Use a unique input so the side-call's process-wide cache
        // doesn't collide with other tests.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/explain", id))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"text":"unique-explain-test-input-alpha-1"}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body["explanation"]
            .as_str()
            .unwrap()
            .contains("In plain English"));
        assert!(body["cached"].as_bool().is_some());
    }
}
