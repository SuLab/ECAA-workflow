//! SME applied-parameter endpoints.
//!
//! - `GET  /session/:id/task/:task_id/parameters` serves the task's atom
//!   `ParameterSpec[]` (the editable schema) plus the SME's current overrides
//!   and current method, so the UI can render a structured editor.
//! - `POST /session/:id/task/:task_id/parameters` binds concrete SME values to
//!   those parameters, validated against the spec, invalidates the task's
//!   forward slice, and routes the session to `ReadyToEmit` (re-emit fires on
//!   the SME's next `/confirm`, per the Phase-0 rule — no git-commit/relaunch
//!   here).
//!
//! Both are deterministic REST (no new LLM `Tool`). The POST mirrors the
//! amend/branch guards: `ensure_not_imported`, If-Match ETag, Idempotency-Key,
//! rate limit, and path-jail on the `task_id` segment.

use super::super::*;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Response for `GET .../task/:task_id/parameters`.
///
/// `Deserialize` is derived alongside `Serialize` so tests can round-trip the
/// response body; the wire contract is serialize-only in production.
#[derive(Debug, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub(crate) struct TaskParametersResponse {
    /// The task the parameters belong to.
    pub task_id: String,
    /// The atom id that backs this task, if the DAG recorded one.
    pub source_atom_id: Option<String>,
    /// The atom's declared, typed parameter schema (empty when the atom
    /// declares no `parameters:` block or can't be resolved).
    pub parameters: Vec<ecaa_workflow_core::atom::ParameterSpec>,
    /// The SME's currently-set overrides for this task (`name -> value`).
    #[ts(type = "Record<string, unknown>")]
    pub current_overrides: std::collections::BTreeMap<String, serde_json::Value>,
    /// The SME-recorded method for this stage, if any.
    pub current_method: Option<String>,
    /// The SME-authored validation bounds currently attached to this task's
    /// stage class (resolved the harness way: `spec.stage_class`, falling back
    /// to `task_id`). Lets the drawer list + remove the bounds that apply here.
    pub current_validation_bounds: Vec<ecaa_workflow_core::validation_bound::SmeValidationBound>,
}

/// Request body for `POST .../task/:task_id/parameters`.
#[derive(Debug, Deserialize)]
pub(crate) struct SetParametersRequest {
    /// Concrete `name -> value` overrides to bind to the task's atom parameters.
    #[serde(default)]
    pub overrides: std::collections::BTreeMap<String, serde_json::Value>,
    /// Optional SME rationale recorded on the resulting amendment.
    #[serde(default)]
    pub rationale: Option<String>,
}

/// Reject a `task_id` segment that could escape a filesystem path. The value is
/// not itself spliced into a path here, but validating it keeps the RC-17
/// contract uniform across every task-scoped endpoint.
fn task_id_is_safe(task_id: &str) -> bool {
    safe_segment_join(std::path::Path::new("/ecaa-jail"), task_id).is_ok()
}

/// GET the editable parameter schema + current values for a task.
#[tracing::instrument(skip(app), fields(session_id = %session_id, task_id = %task_id))]
pub(crate) async fn get_parameters(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
) -> Response {
    if !task_id_is_safe(&task_id) {
        return (StatusCode::BAD_REQUEST, "invalid task_id").into_response();
    }
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(dag) = session.current_dag() else {
        return (StatusCode::NOT_FOUND, "no DAG built yet").into_response();
    };
    let Some(task) = dag.tasks.get(task_id.as_str()) else {
        return (StatusCode::NOT_FOUND, "task not found in DAG").into_response();
    };
    let source_atom_id = task.source_atom_id.clone();
    let config_dir = app.config.config_dir.clone();
    let parameters = session.atom_parameter_specs(&task_id, &config_dir);
    let current_overrides = session.current_parameter_overrides(&task_id);
    let current_method = session
        .intake_methods
        .0
        .get(&task_id)
        .map(|r| r.method.clone())
        .filter(|m| !m.is_empty());
    let current_validation_bounds = session.current_validation_bounds_for_task(&task_id);
    Json(TaskParametersResponse {
        task_id,
        source_atom_id,
        parameters,
        current_overrides,
        current_method,
        current_validation_bounds,
    })
    .into_response()
}

/// POST SME parameter overrides for a task.
#[tracing::instrument(skip(app, headers, req), fields(session_id = %session_id, task_id = %task_id))]
pub(crate) async fn post_parameters(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    headers: HeaderMap,
    BoundedJson(req): BoundedJson<SetParametersRequest>,
) -> Response {
    if !task_id_is_safe(&task_id) {
        return (StatusCode::BAD_REQUEST, "invalid task_id").into_response();
    }
    // Imported (read-only) packages cannot be edited; If-Match guards against a
    // stale view of the session before we mutate it.
    if let Some(session) = app.conversation.get_session(session_id).await {
        if let Err(resp) = crate::chat_routes::package_import::ensure_not_imported(&session) {
            return resp.into_response();
        }
        if let IfMatchOutcome::Mismatch { server, client } =
            check_if_match(&headers, &session, "set_task_parameters")
        {
            return precondition_failed_response(&server, &client);
        }
    } else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }
    // Cheap, non-LLM mutation — capped by the shared SME-edit bucket.
    if let Err(status) = LlmRateBuckets::check(
        &app.llm_buckets.sme_edit,
        session_id,
        app.llm_rate_limits.sme_edit,
    ) {
        return (status, "rate limit exceeded: SME edits capped").into_response();
    }
    // Idempotency-Key replay so a retried POST doesn't double-record decisions.
    let ticket = app
        .idempotency
        .lookup(session_id, "set_task_parameters", &headers);
    if let Some(replay) = ticket.cached_response() {
        return replay;
    }
    let response = post_parameters_inner(app.clone(), session_id, task_id, req).await;
    ticket.store(&app.idempotency, response).await
}

async fn post_parameters_inner(
    app: ChatAppState,
    session_id: Uuid,
    task_id: String,
    req: SetParametersRequest,
) -> Response {
    match app
        .conversation
        .set_task_parameters_from_rest(session_id, task_id.clone(), req.overrides, req.rationale)
        .await
    {
        Ok(invalidated) => {
            // The reset forward slice's cached artifacts are now stale.
            app.artifact_cache.retain(|(sid, _), _| *sid != session_id);
            // No git-commit / auto-relaunch here — a parameter edit leaves the
            // session in `ReadyToEmit` with the pre-edit package still on disk.
            // Re-emit + relaunch fire from `/confirm` (Phase-0 rule).
            Json(serde_json::json!({
                "task_id": task_id,
                "invalidated_tasks": invalidated,
            }))
            .into_response()
        }
        Err(e) => {
            // Strip the ServiceError::Internal display prefix so the client
            // sees the actual validation reason (bad value / unknown param /
            // wrong state) rather than "internal error: …" on a 400 payload.
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
        "/api/chat/session/:id/task/:task_id/parameters",
        axum::routing::get(get_parameters).post(post_parameters),
    )
}

#[cfg(test)]
mod tests {
    use super::TaskParametersResponse;
    use crate::chat_routes::test_support::{
        assistant, augmented_config, body_json, make_router_with_config, tool_use,
    };
    use crate::chat_routes::ChatAppState;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use ecaa_workflow_conversation::{anthropic::TurnResponse, BatchableTool, SessionState, Tool};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    const MIN_LFC_PARAM: &str = "parameters:\n  - name: min_lfc\n    type: number\n    description: \"SME log fold-change floor\"";

    /// Compose a bulk-rnaseq DAG, force `Emitted`, and return the app +
    /// session id + the task backed by `differential_expression`.
    async fn emitted_diffexpr_session(app: &ChatAppState) -> (Uuid, String) {
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        app.conversation
            .send_turn(id, "set it up".into(), None)
            .await
            .unwrap();
        let session = app
            .conversation
            .store_handle()
            .update(id, |s| {
                s.state = SessionState::Emitted;
                s.emitted_package_path = None;
                s.ensure_dag_cached();
                Ok(())
            })
            .await
            .unwrap();
        let dag = session.dag.as_ref().expect("composed dag");
        let task_id = dag
            .tasks
            .iter()
            .find(|(_, t)| t.source_atom_id.as_deref() == Some("differential_expression"))
            .map(|(k, _)| k.to_string())
            .expect("a task backed by the differential_expression atom");
        (id, task_id)
    }

    fn compose_script() -> Vec<TurnResponse> {
        vec![
            tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
                prose: "bulk rna-seq differential expression in human samples".into(),
            })),
            assistant("ok."),
        ]
    }

    #[tokio::test]
    async fn parameters_endpoint_returns_atom_specs_and_current_overrides() {
        let cfg = augmented_config("differential_expression", MIN_LFC_PARAM);
        let (router, app) = make_router_with_config(cfg, compose_script()).await;
        let (id, task_id) = emitted_diffexpr_session(&app).await;

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/api/chat/session/{}/task/{}/parameters",
                        id, task_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let parsed: TaskParametersResponse = serde_json::from_value(body).unwrap();
        assert_eq!(
            parsed.source_atom_id.as_deref(),
            Some("differential_expression")
        );
        assert!(
            parsed.parameters.iter().any(|p| p.name == "min_lfc"),
            "endpoint must surface the atom's declared parameters; got {:?}",
            parsed.parameters
        );
        assert!(parsed.current_overrides.is_empty());
    }

    #[tokio::test]
    async fn parameters_endpoint_returns_current_validation_bounds() {
        let cfg = augmented_config("differential_expression", MIN_LFC_PARAM);
        let (router, app) = make_router_with_config(cfg, compose_script()).await;
        let (id, task_id) = emitted_diffexpr_session(&app).await;

        // Attach an SME validation bound keyed on the task's resolved stage
        // class (the harness way: `spec.stage_class`, else the task id).
        let tid = task_id.clone();
        app.conversation
            .store_handle()
            .update(id, move |s| {
                let stage_class = s.task_stage_class(&tid).unwrap_or_else(|| tid.clone());
                s.sme_validation_bounds.0.push(
                    ecaa_workflow_core::validation_bound::SmeValidationBound {
                        stage_class,
                        assertion_type: "numeric_threshold".into(),
                        target: "results/tables/de.json".into(),
                        check: serde_json::json!({
                            "json_pointer": "/adjusted_p_max", "op": "lte", "value": 0.01
                        }),
                        severity: "required".into(),
                        id: "sme_de_padj".into(),
                        description: "SME: adjusted p must be <= 0.01".into(),
                    },
                );
                Ok(())
            })
            .await
            .unwrap();

        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/api/chat/session/{}/task/{}/parameters",
                        id, task_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let parsed: TaskParametersResponse = serde_json::from_value(body).unwrap();
        assert!(
            parsed
                .current_validation_bounds
                .iter()
                .any(|b| b.id == "sme_de_padj" && b.assertion_type == "numeric_threshold"),
            "endpoint must surface the SME validation bounds for the task's stage; got {:?}",
            parsed.current_validation_bounds
        );
    }

    #[tokio::test]
    async fn get_parameters_unknown_task_is_404() {
        let cfg = augmented_config("differential_expression", MIN_LFC_PARAM);
        let (router, app) = make_router_with_config(cfg, compose_script()).await;
        let (id, _) = emitted_diffexpr_session(&app).await;
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/api/chat/session/{}/task/no_such_task/parameters",
                        id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn set_task_parameters_validates_records_and_reaches_ready_to_emit() {
        let cfg = augmented_config("differential_expression", MIN_LFC_PARAM);
        let (router, app) = make_router_with_config(cfg, compose_script()).await;
        let (id, task_id) = emitted_diffexpr_session(&app).await;

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/chat/session/{}/task/{}/parameters",
                        id, task_id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"overrides":{"min_lfc":1.0},"rationale":"tighten"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "valid override must succeed");

        let s = app.conversation.get_session(id).await.unwrap();
        assert!(
            matches!(s.state, SessionState::PendingConfirmation { .. }),
            "a parameter edit must re-raise the confirmation card (PendingConfirmation), got {:?}",
            s.state
        );
        assert!(
            s.conversation
                .iter()
                .rev()
                .any(|t| t.confirmation_card.is_some()),
            "a parameter edit must raise a confirmation card so /confirm re-emits"
        );
        let ov = s
            .sme_parameter_overrides
            .for_task(&task_id)
            .expect("override recorded for the task");
        assert_eq!(
            ov.get("min_lfc").map(|o| &o.value),
            Some(&serde_json::json!(1.0))
        );
        // The decision log carries the typed SetTaskParameter record.
        assert!(
            s.decisions.iter().any(|d| {
                matches!(&d.decision, ecaa_workflow_core::decision_log::DecisionType::SetTaskParameter { parameter, .. } if parameter == "min_lfc")
            }),
            "a SetTaskParameter decision must be recorded"
        );
    }

    /// FIX 8: an explicit `null` value for a key REMOVES that override (so the
    /// UI can blank a field), and an empty overrides map is a valid "clear all"
    /// request, not a 400.
    #[tokio::test]
    async fn null_value_clears_override_and_empty_map_is_valid_clear() {
        let cfg = augmented_config("differential_expression", MIN_LFC_PARAM);
        let (router, app) = make_router_with_config(cfg, compose_script()).await;
        let (id, task_id) = emitted_diffexpr_session(&app).await;

        // Seed an existing override directly (keep the session Emitted so a
        // second edit is permitted).
        app.conversation
            .store_handle()
            .update(id, |s| {
                s.sme_parameter_overrides.set(
                    &task_id,
                    "min_lfc",
                    serde_json::json!(1.0),
                    ecaa_workflow_core::parameter_override::OverrideSource::Sme,
                );
                Ok(())
            })
            .await
            .unwrap();

        // Null value clears the override.
        let resp = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/chat/session/{}/task/{}/parameters",
                        id, task_id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"overrides":{"min_lfc":null}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "a null value must be accepted (clears the override)"
        );
        let s = app.conversation.get_session(id).await.unwrap();
        assert!(
            s.sme_parameter_overrides.for_task(&task_id).is_none(),
            "null value must have removed the override for the task"
        );

        // An empty overrides map on a task with no overrides is a valid clear.
        app.conversation
            .store_handle()
            .update(id, |s| {
                s.state = SessionState::Emitted;
                Ok(())
            })
            .await
            .unwrap();
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/chat/session/{}/task/{}/parameters",
                        id, task_id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"overrides":{}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "an empty overrides map must be a valid clear, not a 400"
        );
    }

    #[tokio::test]
    async fn set_task_parameters_rejects_unknown_parameter() {
        let cfg = augmented_config("differential_expression", MIN_LFC_PARAM);
        let (router, app) = make_router_with_config(cfg, compose_script()).await;
        let (id, task_id) = emitted_diffexpr_session(&app).await;

        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/api/chat/session/{}/task/{}/parameters",
                        id, task_id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"overrides":{"not_a_param":7}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "an override for an undeclared parameter must be rejected"
        );
    }

    #[tokio::test]
    async fn set_task_parameters_path_jail_rejects_traversal() {
        let cfg = augmented_config("differential_expression", MIN_LFC_PARAM);
        let (router, app) = make_router_with_config(cfg, compose_script()).await;
        let (id, _) = emitted_diffexpr_session(&app).await;
        // Percent-encoded "../" — decoded by axum into a `..` segment.
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{}/task/%2e%2e/parameters", id))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"overrides":{"min_lfc":1.0}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
