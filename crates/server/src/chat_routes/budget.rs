//! `POST /api/chat/session/:id/budget`
//! §3.4 — set / clear the session-level soft budget cap.
//!
//! Records a `DecisionType::BudgetChanged` entry so the audit log
//! captures every cap adjustment alongside the prior value. The cap
//! itself is advisory (the warn / exceeded chip drives UI behavior);
//! the harness auto-relaunch predicate refuses to spawn when the
//! projected cost would exceed the cap *and* `SWFC_BUDGET_HARD_STOP=1`.

use super::{client_ip_from, ChatAppState};
use axum::extract::{ConnectInfo, Path};
use axum::http::HeaderMap;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use chrono::Utc;
use ecaa_workflow_core::decision_log::{DecisionActor, DecisionType};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub(super) struct BudgetRequest {
    /// New cap in USD. Null / absent clears the cap.
    #[serde(default)]
    pub usd: Option<f64>,
    /// Who is making the change (SME username). Optional; defaults to
    /// an empty string on the audit record.
    #[serde(default)]
    pub author: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct BudgetResponse {
    pub budget_usd: Option<f64>,
    pub budget_set_by: Option<String>,
    pub budget_set_at: Option<String>,
}

pub(super) async fn post_budget(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    headers: HeaderMap,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    Json(req): Json<BudgetRequest>,
) -> impl IntoResponse {
    if let Some(v) = req.usd {
        if !v.is_finite() || v < 0.0 {
            return (
                StatusCode::BAD_REQUEST,
                "budget must be a positive finite number",
            )
                .into_response();
        }
    }
    let new_cap = req.usd;
    let author = req.author.unwrap_or_default();
    // Capture the originating IP from the
    // request envelope so the BudgetChanged decision in
    // `runtime/decisions.jsonl` carries an audit trail of which
    // network endpoint flipped the cap.
    let source_ip = client_ip_from(&headers, connect_info.as_ref());
    let store = app.conversation.store_handle();
    let result = store
        .update(session_id, move |s| {
            let prior = s.budget_usd;
            s.budget_usd = new_cap;
            s.budget_set_by = if author.is_empty() {
                None
            } else {
                Some(author.clone())
            };
            s.budget_set_at = Some(Utc::now());
            s.record_decision_with_ip(
                DecisionType::BudgetChanged {
                    prior_usd: prior,
                    new_usd: new_cap,
                },
                DecisionActor::Sme,
                None,
                source_ip.clone(),
            );
            Ok(())
        })
        .await;
    match result {
        Ok(_) => {
            let sess = app.conversation.get_session(session_id).await;
            Json(BudgetResponse {
                budget_usd: sess.as_ref().and_then(|s| s.budget_usd),
                budget_set_by: sess.as_ref().and_then(|s| s.budget_set_by.clone()),
                budget_set_at: sess
                    .as_ref()
                    .and_then(|s| s.budget_set_at)
                    .map(|dt| dt.to_rfc3339()),
            })
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to update budget: {}", e),
        )
            .into_response(),
    }
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[("POST", "/api/chat/session/:id/budget")];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/budget",
        axum::routing::post(post_budget),
    )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{body_json, make_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn post_budget_rejects_negative_value() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/budget", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"usd": -10.0}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_budget_persists_value_and_records_decision() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/budget", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"usd": 50.0, "author": "alan"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["budget_usd"], 50.0);
        assert_eq!(body["budget_set_by"], "alan");
        assert!(body["budget_set_at"].as_str().is_some());

        // Session should carry the cap, and the decision log should
        // hold a `BudgetChanged` entry.
        let session = app.conversation.get_session(id).await.unwrap();
        assert_eq!(session.budget_usd, Some(50.0));
        assert_eq!(session.budget_set_by.as_deref(), Some("alan"));
        let has_budget_decision = session.decisions.iter().any(|d| {
            matches!(
                d.decision,
                ecaa_workflow_core::decision_log::DecisionType::BudgetChanged { .. }
            )
        });
        assert!(
            has_budget_decision,
            "BudgetChanged decision must be recorded"
        );
    }

    #[tokio::test]
    async fn post_budget_clears_with_null() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        // First, set a budget.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/budget", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"usd": 100.0}"#))
            .unwrap();
        let _ = router.clone().oneshot(req).await.unwrap();
        // Then clear it.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/budget", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"usd": null}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let session = app.conversation.get_session(id).await.unwrap();
        assert!(session.budget_usd.is_none());
    }

    /// When an `X-Forwarded-For` header is
    /// supplied, the recorded BudgetChanged decision must carry the
    /// header's first value as `source_ip` so the audit trail in
    /// `runtime/decisions.jsonl` survives review.
    #[tokio::test]
    async fn post_budget_records_source_ip_from_xff_header() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/budget", id))
            .header("content-type", "application/json")
            .header("x-forwarded-for", "203.0.113.42, 10.0.0.1")
            .body(Body::from(r#"{"usd": 75.0}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let session = app.conversation.get_session(id).await.unwrap();
        let budget_decision = session
            .decisions
            .iter()
            .find(|d| {
                matches!(
                    d.decision,
                    ecaa_workflow_core::decision_log::DecisionType::BudgetChanged { .. }
                )
            })
            .expect("BudgetChanged decision must be recorded");
        assert_eq!(
            budget_decision.source_ip.as_deref(),
            Some("203.0.113.42"),
            "source_ip must reflect the first X-Forwarded-For value"
        );
    }

    #[tokio::test]
    async fn post_budget_accepts_zero() {
        // 0.0 is finite and non-negative — must be accepted as an
        // explicit "I want a hard cap of $0" intent. Edge case worth
        // pinning so a future `> 0` rewrite of the validation doesn't
        // accidentally exclude it.
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/budget", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"usd": 0.0}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let session = app.conversation.get_session(id).await.unwrap();
        assert_eq!(session.budget_usd, Some(0.0));
    }
}
