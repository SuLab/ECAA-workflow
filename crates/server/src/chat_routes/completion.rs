//! `GET /api/chat/metrics/completion` — fleet completion-rate KPI.
//!
//! The per-session metrics layer can report a single session's terminal
//! disposition but cannot express "of all sessions started, what
//! fraction reached an emitted package." This endpoint walks the
//! SessionStore, maps each `SessionState` to a `SessionDisposition`, and
//! returns the aggregate via the pure
//! `ecaa_workflow_conversation::metrics::compute_completion_stats`
//! (unit-tested in the conversation crate).

use super::ChatAppState;
use axum::{extract::State, response::IntoResponse, Json};
use ecaa_workflow_conversation::metrics::{compute_completion_stats, SessionDisposition};
use ecaa_workflow_conversation::session::SessionState;

#[tracing::instrument(skip(app))]
pub(super) async fn get_completion_stats(State(app): State<ChatAppState>) -> impl IntoResponse {
    let dispositions: Vec<SessionDisposition> = app
        .conversation
        .iter_sessions()
        .await
        .iter()
        .map(|s| match &s.state {
            SessionState::Emitted => SessionDisposition::Emitted,
            SessionState::Blocked { .. } => SessionDisposition::Blocked,
            _ => SessionDisposition::InProgress,
        })
        .collect();
    Json(compute_completion_stats(&dispositions))
}

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/metrics/completion",
        axum::routing::get(get_completion_stats),
    )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::make_router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn completion_endpoint_returns_stats_for_empty_fleet() {
        let (router, _app) = make_router(vec![]).await;
        let req = Request::builder()
            .method("GET")
            .uri("/api/chat/metrics/completion")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "completion endpoint must 200"
        );
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        // Shape contract: the five aggregate fields are present and the
        // rate is a float in [0, 1].
        assert!(v["sessions"].is_u64(), "sessions must be a number: {v}");
        assert!(v["emitted"].is_u64(), "emitted must be a number: {v}");
        assert!(v["blocked"].is_u64(), "blocked must be a number: {v}");
        assert!(
            v["in_progress"].is_u64(),
            "in_progress must be a number: {v}"
        );
        let rate = v["completion_rate"].as_f64().unwrap();
        assert!(
            (0.0..=1.0).contains(&rate),
            "completion_rate must be in [0,1], got {rate}"
        );
    }
}
