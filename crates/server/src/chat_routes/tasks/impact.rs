//! Amendment, rerun, undo-amendment, impact-preview cluster. Tightly
//! coupled — they all touch session state via `try_transition` and
//! share the auto-relaunch hook.
//!
//! Endpoints (plan §S16.2):
//! - POST /session/:id/task/:task_id/amend-method
//! - POST /session/:id/task/:task_id/undo-amendment
//! - POST /session/:id/task/:task_id/rerun
//! - POST /session/:id/task/:task_id/impact-preview

use super::super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct AmendMethodRequest {
    /// The method (method_id or free prose) the SME wants this stage
    /// to use henceforth. Must be non-empty after trim.
    pub method_prose: String,
    /// Optional rationale; required when the stage is prespecified in
    /// a confirmatory session (checked inside amend_stage_method).
    #[serde(default)]
    pub rationale: Option<String>,
}

/// Body for the Undo Amendment toast. Carries only the prose the
/// session is being reverted to; there is no rationale channel (the
/// round-trip is the rationale).
#[derive(Debug, Deserialize)]
pub struct UndoAmendmentRequest {
    pub reverted_prose: String,
}

#[derive(Debug, Deserialize)]
pub struct RerunRequest {
    /// Free-form justification captured in the RerunTask decision
    /// record; required when the stage is prespecified in a
    /// confirmatory session (same gate as amend).
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ImpactPreviewRequest {
    /// If set, frame the preview as "amend this stage to <proposed>".
    /// When absent the preview assumes a rerun with the existing method
    /// — the forward-slice blast radius is identical either way; only
    /// the cost model differs (rerun reuses the stage's prior
    /// completion cost; amend adds a re-discovery lookup cost).
    #[serde(default)]
    pub proposed_method: Option<String>,
}

/// POST /api/chat/session/:id/task/:task_id/undo-amendment
///
/// replays the amend_stage_method
/// path with the prose the UI captured before the prior amend, and
/// tags the decision log with an `UndoneAmendment` record.
#[tracing::instrument(skip(app, req), fields(session_id = %session_id, task_id = %task_id))]
pub async fn post_undo_amendment(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    BoundedJson(req): BoundedJson<UndoAmendmentRequest>,
) -> impl IntoResponse {
    match app
        .conversation
        .undo_amendment_from_rest(session_id, task_id.clone(), req.reverted_prose)
        .await
    {
        Ok(amend) => {
            super::super::execution::maybe_auto_relaunch_harness(&app, session_id, "undo_amend")
                .await;
            Json(serde_json::json!({
                "task_id": task_id,
                "invalidated_tasks": amend.invalidated_tasks,
            }))
            .into_response()
        }
        Err(e) => {
            let msg = format!("{}", e);
            let cleaned = msg
                .strip_prefix("internal error: ")
                .unwrap_or(&msg)
                .to_string();
            (StatusCode::BAD_REQUEST, cleaned).into_response()
        }
    }
}

/// REST counterpart of the LLM-callable `amend_stage_method` tool.
/// Same guards: session must be Emitted, stage must exist, method
/// must be non-empty, confirmatory+prespecified ⇒ rationale required.
/// Fires the auto-relaunch hook so the harness resumes against the
/// amended plan without a separate POST /start-execution.
#[tracing::instrument(skip(app, req), fields(session_id = %session_id, task_id = %task_id))]
pub async fn post_amend_method(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    BoundedJson(req): BoundedJson<AmendMethodRequest>,
) -> impl IntoResponse {
    match app
        .conversation
        .amend_stage_method_from_rest(session_id, task_id.clone(), req.method_prose, req.rationale)
        .await
    {
        Ok(amend) => {
            // Auto-relaunch if there's now ready work; the hook's own
            // predicate catches the "session still blocked" /
            // "execution already running" cases and logs a skip reason.
            super::super::execution::maybe_auto_relaunch_harness(&app, session_id, "amend_method")
                .await;
            // git commit on amendment. The
            // conversation service has already re-emitted the package
            // by the time `amend_stage_method_from_rest` returns, so
            // the working tree is ready for `git add`. Per-package
            // shape: resolve the session's emitted_package_path so the
            // Hook commits into the right per-package.git directory.
            if let Some(session) = app.conversation.get_session(session_id).await {
                if let Some(pkg) = session.emitted_package_path.clone() {
                    let cfg = app.git_config().read().clone();
                    let sid = session_id.to_string();
                    let stage = task_id.clone();
                    app.git_hook_pool.spawn("amend", move || {
                        crate::git_routes::service::hook_commit(
                            &cfg,
                            &pkg,
                            "amend",
                            &format!("{} amended", stage),
                            &sid,
                        );
                        Ok(())
                    });
                }
            }
            Json(serde_json::json!({
                "task_id": task_id,
                "invalidated_tasks": amend.invalidated_tasks,
                // Undo toast payload. UI uses
                // this to re-submit amend with the prior prose when
                // the SME clicks Undo within the 30s window.
                "prior_method_prose": amend.prior_method_prose,
            }))
            .into_response()
        }
        Err(e) => {
            // Strip the ServiceError::Internal display prefix so the
            // client sees the actual validation reason + hint rather
            // than "internal error: …" on what is really a 400 payload
            // issue (empty method, unknown stage, missing rationale).
            let msg = format!("{}", e);
            let cleaned = msg
                .strip_prefix("internal error: ")
                .unwrap_or(&msg)
                .to_string();
            (StatusCode::BAD_REQUEST, cleaned).into_response()
        }
    }
}

/// REST counterpart of `rerun_task`. Re-queues a completed stage with
/// its existing method — use `amend_method` if the method itself is
/// changing. Fires the auto-relaunch hook.
pub async fn post_rerun(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    body: Option<BoundedJson<RerunRequest>>,
) -> impl IntoResponse {
    let req = body
        .map(|BoundedJson(r)| r)
        .unwrap_or(RerunRequest { reason: None });
    match app
        .conversation
        .rerun_task_from_rest(session_id, task_id.clone(), req.reason)
        .await
    {
        Ok(invalidated) => {
            // artifact cache for the reset task(s) is
            // now stale — drop it before the agent writes fresh output.
            app.artifact_cache.retain(|(sid, _), _| *sid != session_id);
            super::super::execution::maybe_auto_relaunch_harness(&app, session_id, "rerun_task")
                .await;
            Json(serde_json::json!({
                "task_id": task_id,
                "invalidated_tasks": invalidated,
            }))
            .into_response()
        }
        Err(e) => {
            // Strip the ServiceError::Internal display prefix so the
            // client sees the actual validation reason + hint rather
            // than "internal error: …" on what is really a 400 payload
            // issue (empty method, unknown stage, missing rationale).
            let msg = format!("{}", e);
            let cleaned = msg
                .strip_prefix("internal error: ")
                .unwrap_or(&msg)
                .to_string();
            (StatusCode::BAD_REQUEST, cleaned).into_response()
        }
    }
}

/// Pure-function: what would change if the SME amended or rerun this
/// task? Clones the DAG, runs `invalidate_forward_slice`, joins with
/// historical per-task cost from the metrics store.
///
/// Never mutates. Safe to call repeatedly at UI interaction speed (the
/// DAG clone is O(n) over typical ≤100-task workflows).
pub async fn post_impact_preview(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
    body: Option<BoundedJson<ImpactPreviewRequest>>,
) -> impl IntoResponse {
    let req = body.map(|BoundedJson(r)| r);
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(dag) = session.current_dag() else {
        return (StatusCode::NOT_FOUND, "no DAG built yet").into_response();
    };
    if !dag.tasks.contains_key(task_id.as_str()) {
        return (StatusCode::NOT_FOUND, "task not found in DAG").into_response();
    }
    let mut cloned = dag.clone();
    let invalidated = cloned.invalidate_forward_slice(task_id.as_str(), true);

    // Historical cost lookup: map over the per-task agent snapshots
    // from MetricsStore::snapshot. Tasks that never completed before
    // get a conservative range bound based on their stage_class
    // median in this session.
    let metrics = app.conversation.metrics().snapshot(session_id).await;
    let per_task_costs: std::collections::BTreeMap<String, f64> = metrics
        .as_ref()
        .map(|m| {
            m.per_task_agent
                .iter()
                .map(|r| (r.task_id.clone(), r.cost_usd))
                .collect()
        })
        .unwrap_or_default();

    // Per-stage-class median (from this session's history) for tasks
    // with no prior cost. Rough estimate — better than nothing.
    let mut by_stage: std::collections::BTreeMap<String, Vec<f64>> =
        std::collections::BTreeMap::new();
    for (tid, cost) in &per_task_costs {
        if let Some(task) = dag.tasks.get(tid.as_str()) {
            if let Some(spec) = &task.spec {
                if let Some(stage_class) = spec.get("stage_class").and_then(|v| v.as_str()) {
                    by_stage
                        .entry(stage_class.to_string())
                        .or_default()
                        .push(*cost);
                }
            }
        }
    }
    let stage_median_cost: std::collections::BTreeMap<String, f64> = by_stage
        .into_iter()
        .map(|(k, mut v)| {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let mid = v[v.len() / 2];
            (k, mid)
        })
        .collect();

    // Build the per-invalidated-task breakdown.
    let mut est_cost_min = 0.0f64;
    let mut est_cost_max = 0.0f64;
    let mut tasks_out: Vec<serde_json::Value> = Vec::with_capacity(invalidated.len());
    for tid in &invalidated {
        let task = dag.tasks.get(tid);
        let desc = task.map(|t| t.description.clone()).unwrap_or_default();
        let stage_class = task
            .and_then(|t| t.spec.as_ref())
            .and_then(|s| s.get("stage_class"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        // Use the task's own historical cost if it has one; otherwise
        // fall back to the stage-class median.
        let prior = per_task_costs.get(tid.as_str()).copied();
        let median_fallback = stage_class
            .as_ref()
            .and_then(|sc| stage_median_cost.get(sc).copied());
        let (cost_min, cost_max, source) = match (prior, median_fallback) {
            (Some(c), _) => (c * 0.8, c * 1.3, "prior_run"),
            (None, Some(c)) => (c * 0.5, c * 2.0, "stage_median"),
            _ => (0.5, 5.0, "coarse_default"),
        };
        est_cost_min += cost_min;
        est_cost_max += cost_max;

        tasks_out.push(serde_json::json!({
            "task_id": tid,
            "description": desc,
            "stage_class": stage_class,
            "current_status": task.map(|t| match &t.state {
                ecaa_workflow_core::dag::TaskState::Pending => "pending",
                ecaa_workflow_core::dag::TaskState::Ready => "ready",
                ecaa_workflow_core::dag::TaskState::Running { .. } => "running",
                ecaa_workflow_core::dag::TaskState::Completed { .. } => "completed",
                ecaa_workflow_core::dag::TaskState::Blocked { .. } => "blocked",
                ecaa_workflow_core::dag::TaskState::Failed { .. } => "failed",
            }),
            "est_cost_usd_min": cost_min,
            "est_cost_usd_max": cost_max,
            "cost_source": source,
        }));
    }

    Json(serde_json::json!({
        "target_task_id": task_id,
        "proposed_method": req.and_then(|r| r.proposed_method),
        "invalidated_tasks": tasks_out,
        "invalidated_count": invalidated.len(),
        "est_cost_usd_min": est_cost_min,
        "est_cost_usd_max": est_cost_max,
    }))
    .into_response()
}

pub(crate) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/task/:task_id/impact-preview",
            axum::routing::post(post_impact_preview),
        )
        .route(
            "/api/chat/session/:id/task/:task_id/amend-method",
            axum::routing::post(post_amend_method),
        )
        .route(
            "/api/chat/session/:id/task/:task_id/undo-amendment",
            axum::routing::post(post_undo_amendment),
        )
        .route(
            "/api/chat/session/:id/task/:task_id/rerun",
            axum::routing::post(post_rerun),
        )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{
        body_json, make_router, seed_session_with_completed_task,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    // ── impact-preview endpoint ───────────────────────────────────────────

    #[tokio::test]
    async fn impact_preview_404_when_task_missing() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_known", None).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/task/t_bogus/impact-preview",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn impact_preview_returns_forward_slice_and_cost_ranges() {
        use ecaa_workflow_core::dag::{
            Assignee, ResourceClass, Task, TaskId, TaskKind, TaskState, DAG,
        };
        let (router, app) = make_router(vec![]).await;
        // Seed a 3-task chain: A → B → C, all completed.
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        app.conversation
            .store_handle()
            .update(id, |s| {
                let mut tasks = std::collections::BTreeMap::new();
                for (tid, deps) in [
                    ("A", vec![]),
                    ("B", vec!["A".into()]),
                    ("C", vec!["B".into()]),
                ] {
                    tasks.insert(
                        TaskId::from(tid),
                        Task {
                            kind: TaskKind::Computation,
                            state: TaskState::Completed {
                                result: serde_json::json!({}),
                            },
                            depends_on: deps,
                            assignee: Assignee::Agent,
                            description: format!("task {}", tid),
                            spec: None,
                            resolution: None,
                            result_ref: None,
                            resource_class: ResourceClass::CpuHeavy,
                            requires_sme_review: false,

                            required_artifacts: vec![],
                            container: None,
                            source_atom_id: None,
                            safety: Default::default(),
                        },
                    );
                }
                s.dag = Some(DAG {
                    version: "test".into(),
                    schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
                    workflow_id: "wf-test".into(),
                    current_task: None,
                    tasks,
                    reverse_deps: std::collections::BTreeMap::new(),
                    run_id: None,
                });
                Ok(())
            })
            .await
            .unwrap();

        // Amend at B → forward slice is {B, C} (2 tasks).
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/task/B/impact-preview", id))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["target_task_id"], "B");
        assert_eq!(body["invalidated_count"], 2);
        let ids: Vec<String> = body["invalidated_tasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["task_id"].as_str().unwrap().to_string())
            .collect();
        assert!(ids.contains(&"B".into()));
        assert!(ids.contains(&"C".into()));
        assert!(!ids.contains(&"A".into()), "A is upstream, not invalidated");

        // Cost range comes from the coarse_default fallback (no prior
        // metrics recorded in this seed path): each task priced at
        // $0.5–$5.0.
        let min = body["est_cost_usd_min"].as_f64().unwrap();
        let max = body["est_cost_usd_max"].as_f64().unwrap();
        assert!(min > 0.0 && min < max, "min={} max={}", min, max);
    }

    #[tokio::test]
    async fn impact_preview_carries_proposed_method_through() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/task/t_demo/impact-preview",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"proposed_method":"sctransform_v2"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["proposed_method"], "sctransform_v2");
    }

    // ── amend-method + rerun endpoints ────────────────────────────────────

    #[tokio::test]
    async fn amend_method_400_when_session_not_emitted() {
        // Fresh session is in Greeting/Intake, not Emitted; amend must reject.
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/task/t_demo/amend-method", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"method_prose":"sctransform_v2"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn amend_method_400_when_method_prose_empty() {
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/task/t_demo/amend-method", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"method_prose":"  "}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rerun_400_when_no_recorded_method() {
        // Seed session is Greeting, has a completed task but no
        // intake_methods entry — rerun requires a recorded method.
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(&app, "t_demo", None).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/task/t_demo/rerun", id))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
