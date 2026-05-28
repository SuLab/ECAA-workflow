//! `GET /api/chat/session/:id/verifier-decisions` — v4 P2 / F18.
//!
//! Returns the typed verifier decision substrate for a session, read
//! from `runtime/verifier-decisions.jsonl` inside the emitted package.
//! A 200 with empty array is returned when no emit has happened yet or
//! when the session is a v1/v2/v3 emit that didn't run the
//! compatibility engine.

use super::ChatAppState;
use axum::extract::{Path, Query};
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use ecaa_workflow_core::decision_substrate::VerifierDecision;
use serde::Deserialize;
use uuid::Uuid;

/// Query parameters for `GET /verifier-decisions`. The substrate for
/// large composer runs can exceed 100k rows; without a default cap the
/// browser tab freezes rendering the table. Callers that need the full
/// log pass `limit=0` to opt out of the cap.
#[derive(Debug, Deserialize, Default)]
pub(super) struct VerifierDecisionsQuery {
    /// Maximum rows to return. `0` = no cap. Default is 5000.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Number of rows to skip before returning (for paging). Default 0.
    #[serde(default)]
    pub offset: Option<usize>,
}

const DEFAULT_LIMIT: usize = 5000;

/// Read the substrate sidecar for the session's most recent emit.
/// 404 when the session is unknown; 200 with `[]` when no sidecar
/// file has been written yet. Returns a JSON array (wire-compatible
/// with the pre-pagination shape); pagination is via `?limit=&offset=`
/// query params. Default cap is `DEFAULT_LIMIT`; `limit=0` opts out.
pub(super) async fn get_verifier_decisions(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    Query(q): Query<VerifierDecisionsQuery>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let empty = || Json(Vec::<VerifierDecision>::new()).into_response();
    let Some(pkg_root) = session.emitted_package_path.as_ref() else {
        return empty();
    };
    let runtime_dir = pkg_root.join("runtime");
    match ecaa_workflow_conversation::emit::read_verifier_decisions(&runtime_dir) {
        Ok(decisions) => {
            let offset = q.offset.unwrap_or(0).min(decisions.len());
            let raw_limit = q.limit.unwrap_or(DEFAULT_LIMIT);
            // limit=0 → no cap; sane upper bound to avoid pathological JSON
            // payloads on misconfigured clients.
            let limit = if raw_limit == 0 {
                decisions.len().saturating_sub(offset)
            } else {
                raw_limit.min(decisions.len().saturating_sub(offset))
            };
            let slice: Vec<_> = decisions.iter().skip(offset).take(limit).cloned().collect();
            Json(slice).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("verifier-decisions read failed: {}", e),
        )
            .into_response(),
    }
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder.
pub(super) const ROUTES: &[(&str, &str)] = &[("GET", "/api/chat/session/:id/verifier-decisions")];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/verifier-decisions",
        axum::routing::get(get_verifier_decisions),
    )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{body_json, make_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    #[tokio::test]
    async fn returns_404_for_unknown_session() {
        let (router, _) = make_router(vec![]).await;
        let bogus = Uuid::new_v4();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/verifier-decisions", bogus))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn returns_empty_array_for_session_without_emit() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/verifier-decisions", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body.is_array());
        assert_eq!(body.as_array().unwrap().len(), 0);
    }
}
