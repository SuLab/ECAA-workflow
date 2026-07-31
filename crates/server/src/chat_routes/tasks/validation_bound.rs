//! SME validation-bound endpoint.
//!
//! `POST /session/:id/task/:task_id/validation-bound` adds, replaces, or removes
//! one SME-authored assertion that merges into the emitted
//! `policies/validation-contract.json` and is enforced post-hoc by the harness
//! `run_assertion`. Deterministic REST (no new LLM `Tool`). Mirrors the amend
//! guards (`ensure_not_imported`, If-Match, Idempotency-Key, rate limit,
//! path-jail); does NOT invalidate the DAG (bounds are post-hoc checks) but DOES
//! clear the confirmation so a re-confirm re-emits the amended contract.

use super::super::*;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

/// Request body for `POST .../task/:task_id/validation-bound`.
///
/// `bound = Some(_)` adds or replaces (by id, within its `stage_class`);
/// `bound = None` removes the bound identified by `(stage_class, bound_id)`.
#[derive(Debug, Deserialize)]
pub(crate) struct SetValidationBoundRequest {
    /// Stage class the bound applies to (required for removal + as a fallback
    /// when `bound` is present it is taken from the bound itself).
    #[serde(default)]
    pub stage_class: Option<String>,
    /// The bound to add/replace. `null` (or absent) means "remove by id".
    #[serde(default)]
    pub bound: Option<ecaa_workflow_core::validation_bound::SmeValidationBound>,
    /// The bound id to remove (used only when `bound` is `None`).
    #[serde(default)]
    pub bound_id: Option<String>,
    /// Optional SME rationale (currently informational; not persisted on the
    /// bound itself).
    #[serde(default)]
    pub rationale: Option<String>,
}

fn task_id_is_safe(task_id: &str) -> bool {
    safe_segment_join(std::path::Path::new("/ecaa-jail"), task_id).is_ok()
}

/// POST an SME validation bound (add / replace / remove).
#[tracing::instrument(skip(app, headers, req), fields(session_id = %session_id, task_id = %task_id))]
pub(crate) async fn post_validation_bound(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    headers: HeaderMap,
    BoundedJson(req): BoundedJson<SetValidationBoundRequest>,
) -> Response {
    if !task_id_is_safe(&task_id) {
        return (StatusCode::BAD_REQUEST, "invalid task_id").into_response();
    }
    if let Some(session) = app.conversation.get_session(session_id).await {
        if let Err(resp) = crate::chat_routes::package_import::ensure_not_imported(&session) {
            return resp.into_response();
        }
        if let IfMatchOutcome::Mismatch { server, client } =
            check_if_match(&headers, &session, "set_validation_bound")
        {
            return precondition_failed_response(&server, &client);
        }
    } else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }
    if let Err(status) = LlmRateBuckets::check(
        &app.llm_buckets.sme_edit,
        session_id,
        app.llm_rate_limits.sme_edit,
    ) {
        return (status, "rate limit exceeded: SME edits capped").into_response();
    }
    let ticket = app
        .idempotency
        .lookup(session_id, "set_validation_bound", &headers);
    if let Some(replay) = ticket.cached_response() {
        return replay;
    }
    let response = post_validation_bound_inner(app.clone(), session_id, req).await;
    ticket.store(&app.idempotency, response).await
}

async fn post_validation_bound_inner(
    app: ChatAppState,
    session_id: Uuid,
    req: SetValidationBoundRequest,
) -> Response {
    // Resolve the (stage_class, bound_id) the decision + removal key over both
    // the add (bound present) and remove (bound absent) shapes.
    let stage_class = req
        .bound
        .as_ref()
        .map(|b| b.stage_class.clone())
        .or_else(|| req.stage_class.clone());
    let bound_id = req
        .bound
        .as_ref()
        .map(|b| b.id.clone())
        .or_else(|| req.bound_id.clone());
    let (Some(stage_class), Some(bound_id)) = (stage_class, bound_id) else {
        return (
            StatusCode::BAD_REQUEST,
            "validation-bound request must carry a `bound` (add/replace) or a \
             `stage_class` + `bound_id` (remove)",
        )
            .into_response();
    };

    match app
        .conversation
        .set_validation_bound_from_rest(session_id, stage_class, req.bound, bound_id, req.rationale)
        .await
    {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => {
            let msg = format!("{e}");
            let cleaned = msg
                .strip_prefix("internal error: ")
                .unwrap_or(&msg)
                .to_string();
            (StatusCode::BAD_REQUEST, cleaned).into_response()
        }
    }
}

pub(crate) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/task/:task_id/validation-bound",
        axum::routing::post(post_validation_bound),
    )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{
        assistant, make_router, seed_session_with_completed_task, tool_use,
    };
    use crate::chat_routes::ChatAppState;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use ecaa_workflow_conversation::{BatchableTool, SessionState, Tool};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    /// Compose a bulk-rnaseq DAG and force `Emitted` so the validation-bound
    /// endpoint's Emitted-state guard is satisfied.
    async fn emitted_session(app: &ChatAppState) -> Uuid {
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        app.conversation
            .send_turn(id, "set it up".into(), None)
            .await
            .unwrap();
        app.conversation
            .store_handle()
            .update(id, |s| {
                s.state = SessionState::Emitted;
                s.ensure_dag_cached();
                Ok(())
            })
            .await
            .unwrap();
        id
    }

    fn add_bound_body() -> String {
        serde_json::json!({
            "bound": {
                "stage_class": "differential_expression",
                "assertion_type": "numeric_threshold",
                "target": "results/tables/de.json",
                "check": { "json_pointer": "/adjusted_p_max", "op": "lte", "value": 0.01 },
                "severity": "required",
                "id": "sme_de_padj",
                "description": "SME: adjusted p must be <= 0.01"
            }
        })
        .to_string()
    }

    #[tokio::test]
    async fn add_validation_bound_records_decision_and_reaches_ready_to_emit() {
        let (_router, app) = make_router(vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "bulk rna-seq differential expression in human samples".into(),
            })),
            assistant("ok."),
        ])
        .await;
        let id = emitted_session(&app).await;
        let router = crate::chat_routes::router(app.clone()).layer(axum::Extension(
            crate::auth::RequestPrincipal::test_default(),
        ));

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/chat/session/{}/task/differential_expression/validation-bound",
                        id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(add_bound_body()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "adding a bound must succeed");

        let s = app.conversation.get_session(id).await.unwrap();
        assert!(
            matches!(s.state, SessionState::PendingConfirmation { .. }),
            "a bound edit must re-raise the confirmation card (PendingConfirmation), got {:?}",
            s.state
        );
        assert!(
            s.conversation
                .iter()
                .rev()
                .any(|t| t.confirmation_card.is_some()),
            "a bound edit must raise a confirmation card so /confirm re-emits"
        );
        assert!(
            s.sme_validation_bounds
                .0
                .iter()
                .any(|b| b.id == "sme_de_padj"),
            "the SME bound must be stored on the session"
        );
        assert!(
            s.decisions.iter().any(|d| matches!(
                &d.decision,
                ecaa_workflow_core::decision_log::DecisionType::SetValidationBound { bound_id, removed, .. }
                    if bound_id == "sme_de_padj" && !*removed
            )),
            "a SetValidationBound decision must be recorded"
        );
    }

    #[tokio::test]
    async fn unsupported_assertion_type_is_rejected() {
        let (_router, app) = make_router(vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "bulk rna-seq differential expression in human samples".into(),
            })),
            assistant("ok."),
        ])
        .await;
        let id = emitted_session(&app).await;
        let router = crate::chat_routes::router(app.clone()).layer(axum::Extension(
            crate::auth::RequestPrincipal::test_default(),
        ));

        let body = serde_json::json!({
            "bound": {
                "stage_class": "differential_expression",
                "assertion_type": "json_key_equals",
                "target": "results/tables/de.json",
                "check": {},
                "severity": "required",
                "id": "bad",
                "description": "unimplemented"
            }
        })
        .to_string();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/chat/session/{}/task/differential_expression/validation-bound",
                        id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "an assertion_type the harness can't run must be rejected"
        );
    }

    /// FIX 3: a bound whose `stage_class` matches no task's stage class in the
    /// DAG is silently inert (the harness never evaluates it). It must be
    /// rejected with 400 instead of merged.
    #[tokio::test]
    async fn unknown_stage_class_is_rejected() {
        let (_router, app) = make_router(vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "bulk rna-seq differential expression in human samples".into(),
            })),
            assistant("ok."),
        ])
        .await;
        let id = emitted_session(&app).await;
        let router = crate::chat_routes::router(app.clone()).layer(axum::Extension(
            crate::auth::RequestPrincipal::test_default(),
        ));

        let body = serde_json::json!({
            "bound": {
                "stage_class": "no_such_stage_class",
                "assertion_type": "numeric_threshold",
                "target": "results/tables/de.json",
                "check": { "json_pointer": "/adjusted_p_max", "op": "lte", "value": 0.01 },
                "severity": "required",
                "id": "sme_bad_stage",
                "description": "bound on a stage that does not exist"
            }
        })
        .to_string();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/chat/session/{}/task/differential_expression/validation-bound",
                        id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "a bound keyed on an unknown stage_class must be rejected"
        );
    }

    /// FIX 4: a bound whose `assertion_type` is supported but whose `check`
    /// payload is missing fields the harness reads (here numeric_threshold with
    /// no json_pointer/op/value) would fail-close to false forever, permanently
    /// re-blocking the stage. It must be rejected at set-time with 400.
    #[tokio::test]
    async fn malformed_check_is_rejected() {
        let (_router, app) = make_router(vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "bulk rna-seq differential expression in human samples".into(),
            })),
            assistant("ok."),
        ])
        .await;
        let id = emitted_session(&app).await;
        let router = crate::chat_routes::router(app.clone()).layer(axum::Extension(
            crate::auth::RequestPrincipal::test_default(),
        ));

        let body = serde_json::json!({
            "bound": {
                "stage_class": "differential_expression",
                "assertion_type": "numeric_threshold",
                "target": "results/tables/de.json",
                "check": {},
                "severity": "required",
                "id": "sme_malformed",
                "description": "numeric_threshold missing json_pointer/op/value"
            }
        })
        .to_string();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/chat/session/{}/task/differential_expression/validation-bound",
                        id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "a supported type with a malformed check must be rejected"
        );
    }

    #[tokio::test]
    async fn validation_bound_rejected_when_not_emitted() {
        // Seeded session is in Greeting (not Emitted) — the endpoint must reject.
        let (_router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "differential_expression", None).await;
        let router = crate::chat_routes::router(app.clone()).layer(axum::Extension(
            crate::auth::RequestPrincipal::test_default(),
        ));
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/chat/session/{}/task/differential_expression/validation-bound",
                        id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(add_bound_body()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
