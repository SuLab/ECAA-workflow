//! Disposition apply paths.
//!
//! Owns the POST handlers that dispatch the SME's "apply this
//! disposition" click into the canonical `apply_actions` engine
//! (in `mod.rs`): full-disposition apply (`/dispositions/apply/*path`)
//! and single-action apply (`/dispositions/apply-one/*path`). Wire-shape
//! request/response types live alongside their handlers.

use super::*;
use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, serde::Deserialize, Default)]
pub(super) struct ApplyRequest {
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub dry_run: Option<bool>,
}

#[derive(Debug, Serialize)]
struct DryRunResponse {
    session_id: Uuid,
    path: String,
    action_count: usize,
    status: DispositionStatus,
}

#[derive(Debug, serde::Deserialize, Default)]
pub(super) struct ApplyActionRequest {
    /// Zero-based index into the disposition's `actions[]` array. The
    /// axum wildcard-routing constraint (§5.2 note in `mod.rs::router`)
    /// means this comes from the body rather than the URL path.
    pub action_index: usize,
    /// Optional SME justification attached to the `DispositionApplied`
    /// record. Threaded through when present but not enforced — the
    /// card already surfaces the agent's own rationale in the preview.
    #[serde(default)]
    pub rationale: Option<String>,
}

/// `POST /api/chat/session/:id/dispositions/apply/*path` — apply every
/// action in the disposition. Returns 207 Multi-Status when a partial
/// failure occurred so the UI can render per-action status.
pub(super) async fn post_apply(
    State(app): State<ChatAppState>,
    AxumPath((session_id, raw_path)): AxumPath<(Uuid, String)>,
    body: Option<BoundedJson<ApplyRequest>>,
) -> impl IntoResponse {
    let req = body.map(|BoundedJson(r)| r).unwrap_or_default();
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return (StatusCode::BAD_REQUEST, "session has no emitted package").into_response();
    };
    let rel_path = decode_path(&raw_path);
    // 404 up-front when the file doesn't exist on disk. Without this
    // the downstream `apply_actions` returns an ApplyOutcome with an
    // empty action list and 200 OK, which fools the UI into thinking
    // the apply ran.
    if queue_get_or_load(&app.dispositions, session_id, &pkg, &rel_path)
        .await
        .is_none()
    {
        return (StatusCode::NOT_FOUND, "disposition not found").into_response();
    }
    // dry_run returns the preview body without touching disk / state.
    if req.dry_run.unwrap_or(false) {
        let d = queue_get_or_load(&app.dispositions, session_id, &pkg, &rel_path)
            .await
            .expect("disposition presence just verified");
        return Json(DryRunResponse {
            session_id,
            path: rel_path.to_string_lossy().to_string(),
            action_count: d.actions.len(),
            status: d.status,
        })
        .into_response();
    }
    let outcome = apply_actions(&app, session_id, &pkg, &rel_path, false).await;
    // Record the SME's rationale on the overall apply as a generic
    // trailing entry (optional). The individual action records carry
    // the mechanical per-action status.
    if let Some(r) = req.rationale {
        if !r.trim().is_empty() {
            let rel = rel_path.to_string_lossy().to_string();
            let _ = app
                .conversation
                .store_handle()
                .update(session_id, move |s| {
                    s.record_decision(
                        DecisionType::DispositionApplied {
                            path: rel.clone(),
                            action_index: usize::MAX,
                            action_kind: "apply_all".into(),
                            target_stage: "-".into(),
                            outcome: "ok".into(),
                            error_reason: None,
                            auto: false,
                        },
                        DecisionActor::Sme,
                        Some(r.clone()),
                    );
                    Ok(())
                })
                .await;
        }
    }
    let status = match outcome.status {
        DispositionStatus::Applied => StatusCode::OK,
        DispositionStatus::Partial => StatusCode::MULTI_STATUS,
        _ => StatusCode::OK,
    };
    (status, Json(outcome)).into_response()
}

/// `POST /api/chat/session/:id/dispositions/apply-one/*path`
/// — apply a single action by its zero-based index, passed in the body.
pub(super) async fn post_apply_one(
    State(app): State<ChatAppState>,
    AxumPath((session_id, raw_path)): AxumPath<(Uuid, String)>,
    BoundedJson(req): BoundedJson<ApplyActionRequest>,
) -> impl IntoResponse {
    let action_index = req.action_index;
    let sme_rationale = req.rationale.clone();
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return (StatusCode::BAD_REQUEST, "session has no emitted package").into_response();
    };
    let rel_path = decode_path(&raw_path);
    let Some(mut d) = queue_get_or_load(&app.dispositions, session_id, &pkg, &rel_path).await
    else {
        return (StatusCode::NOT_FOUND, "disposition not found").into_response();
    };
    if action_index >= d.actions.len() {
        return (StatusCode::BAD_REQUEST, "action_index out of range").into_response();
    }
    let action = d.actions[action_index].clone();
    let (kind_label, target_label, result) = match &action {
        Action::AmendMethod {
            target_stage,
            new_method,
            rationale,
            ..
        } => {
            let r = app
                .conversation
                .amend_stage_method_from_rest(
                    session_id,
                    target_stage.clone(),
                    new_method.clone(),
                    rationale.clone(),
                )
                .await
                .map(|res| res.invalidated_tasks);
            (
                "amend_method".to_string(),
                target_stage.clone(),
                r.map_err(|e| format!("{}", e)),
            )
        }
        Action::Rerun {
            target_stage,
            reason,
        } => {
            let r = app
                .conversation
                .rerun_task_from_rest(session_id, target_stage.clone(), reason.clone())
                .await;
            (
                "rerun".to_string(),
                target_stage.clone(),
                r.map_err(|e| format!("{}", e)),
            )
        }
        Action::InvalidateSlice { from_stage, .. } => (
            "invalidate_slice".to_string(),
            from_stage.clone(),
            Ok(Vec::new()),
        ),
        Action::PreservePin { target_stage, .. } => (
            "preserve_pin".to_string(),
            target_stage.clone(),
            Ok(Vec::new()),
        ),
    };
    match result {
        Ok(invalidated) => {
            record_applied_decision(
                &app,
                session_id,
                &rel_path,
                action_index,
                &kind_label,
                &target_label,
                "ok",
                None,
                false,
            )
            .await;
            // Attach the SME rationale as a secondary trailing record
            // so the audit trail captures "I applied action N because X"
            // without needing to invent a new variant.
            if let Some(r) = sme_rationale.as_deref() {
                if !r.trim().is_empty() {
                    let rel = rel_path.to_string_lossy().to_string();
                    let label = kind_label.clone();
                    let target = target_label.clone();
                    let note = r.to_string();
                    let _ = app
                        .conversation
                        .store_handle()
                        .update(session_id, move |s| {
                            s.record_decision(
                                DecisionType::DispositionApplied {
                                    path: rel.clone(),
                                    action_index,
                                    action_kind: label.clone(),
                                    target_stage: target.clone(),
                                    outcome: "ok".into(),
                                    error_reason: None,
                                    auto: false,
                                },
                                DecisionActor::Sme,
                                Some(note.clone()),
                            );
                            Ok(())
                        })
                        .await;
                }
            }
            d.status = if matches!(d.status, DispositionStatus::Pending) {
                DispositionStatus::Partial
            } else {
                d.status
            };
            d.status_updated_at = Some(ecaa_workflow_core::time_helpers::now_rfc3339());
            persist_disposition(&pkg, &rel_path, &d);
            queue_upsert(&app.dispositions, session_id, rel_path.clone(), d.clone()).await;
            crate::chat_routes::execution::maybe_auto_relaunch_harness(
                &app,
                session_id,
                "disposition_apply_one",
            )
            .await;
            Json(serde_json::json!({
                "applied": 1,
                "action_index": action_index,
                "invalidated_tasks": invalidated,
                "status": d.status,
            }))
            .into_response()
        }
        Err(msg) => {
            record_applied_decision(
                &app,
                session_id,
                &rel_path,
                action_index,
                &kind_label,
                &target_label,
                "err",
                Some(msg.clone()),
                false,
            )
            .await;
            (StatusCode::BAD_REQUEST, msg).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_routes::dispositions::scan::scan_session_dispositions;
    use crate::chat_routes::test_support::{make_router, seed_session_with_completed_task};
    use axum::body::Body;
    use axum::http::Request;
    use tower::util::ServiceExt;

    fn write_disposition(pkg: &std::path::Path, task_id: &str, body: serde_json::Value) -> PathBuf {
        let rel = PathBuf::from("runtime/outputs")
            .join(task_id)
            .join("sme_disposition.json");
        let full = pkg.join(&rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(&full, serde_json::to_vec_pretty(&body).unwrap()).unwrap();
        rel
    }

    fn sample_v0_disposition() -> serde_json::Value {
        serde_json::json!({
            "task_id": "results_review",
            "authoritative_interpretation": "batch residual",
            "downstream_invalidation_request": {
                "stages_to_invalidate_in_order": [
                    "batch_correction", "integration", "results_review"
                ],
                "method_amendment": {
                    "stage": "batch_correction",
                    "chosen_method": "cca_integratelayers"
                }
            }
        })
    }

    #[tokio::test]
    async fn apply_endpoint_404_when_disposition_missing() {
        let pkg = tempfile::tempdir().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(
            &app,
            "batch_correction",
            Some(pkg.path().to_path_buf()),
        )
        .await;
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/dispositions/apply/runtime/outputs/nonexistent/sme_disposition.json",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn apply_dry_run_does_not_mutate() {
        let pkg = tempfile::tempdir().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(
            &app,
            "batch_correction",
            Some(pkg.path().to_path_buf()),
        )
        .await;
        write_disposition(pkg.path(), "results_review", sample_v0_disposition());

        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/dispositions/apply/runtime/outputs/results_review/sme_disposition.json",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"dry_run": true}"#))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = crate::chat_routes::test_support::body_json(resp.into_body()).await;
        assert!(body["action_count"].as_u64().unwrap() > 0);
        // Disk file was NEVER rewritten by dry-run. The agent's
        // original write had no `status` field, so deserializing back
        // yields Null — confirming no mutation touched the file.
        let disk: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                pkg.path()
                    .join("runtime/outputs/results_review/sme_disposition.json"),
            )
            .unwrap(),
        )
        .unwrap();
        // Status is either absent (original v0 file) or still "pending".
        let status = disk.get("status").and_then(|v| v.as_str());
        assert!(
            matches!(status, None | Some("pending")),
            "dry-run must not mutate disk status, got {:?}",
            status
        );
    }

    #[tokio::test]
    async fn apply_actions_records_advisory_invalidate_slice_as_ok() {
        // The InvalidateSlice action is advisory — it records an `ok`
        // DispositionApplied record without calling any service
        // mutation. Exercises the apply loop without requiring a real
        // taxonomy-backed DAG (the full amend path needs one; that's
        // covered end-to-end by the fixture-based test in §7.7).
        let pkg = tempfile::tempdir().unwrap();
        let (_router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(
            &app,
            "batch_correction",
            Some(pkg.path().to_path_buf()),
        )
        .await;
        // A disposition with only an advisory invalidate_slice so the
        // loop doesn't call amend/rerun.
        let body = serde_json::json!({
            "task_id": "results_review",
            "actions": [
                {
                    "kind": "invalidate_slice",
                    "from_stage": "batch_correction",
                    "stages_explicit": ["integration", "clustering"]
                }
            ]
        });
        write_disposition(pkg.path(), "results_review", body);
        let rel_path = PathBuf::from("runtime/outputs/results_review/sme_disposition.json");
        let _ = scan_session_dispositions(&app, id).await;
        let outcome = apply_actions(&app, id, pkg.path(), &rel_path, false).await;
        assert_eq!(outcome.status, DispositionStatus::Applied);
        assert_eq!(outcome.applied, 1);
        assert_eq!(outcome.failed, 0);
        let session = app.conversation.get_session(id).await.unwrap();
        assert!(session.decisions.iter().any(|d| matches!(
            &d.decision,
            DecisionType::DispositionApplied {
                action_kind,
                outcome,
                ..
            } if action_kind == "invalidate_slice" && outcome == "ok"
        )));
        // Disk status flipped to applied.
        let disk: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                pkg.path()
                    .join("runtime/outputs/results_review/sme_disposition.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(disk["status"], "applied");
    }

    #[tokio::test]
    async fn apply_actions_reports_partial_on_unknown_stage() {
        // An amend against a stage that doesn't exist in the DAG
        // should fail and produce a Partial outcome with a stderr-safe
        // error_reason on the decision record.
        let pkg = tempfile::tempdir().unwrap();
        let (_router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(
            &app,
            "batch_correction",
            Some(pkg.path().to_path_buf()),
        )
        .await;
        app.conversation
            .store_handle()
            .update(id, |s| {
                use ecaa_workflow_conversation::SessionState;
                s.state = SessionState::Emitted;
                Ok(())
            })
            .await
            .unwrap();
        // A disposition that targets a nonexistent stage.
        let body = serde_json::json!({
            "task_id": "results_review",
            "actions": [
                {
                    "kind": "amend_method",
                    "target_stage": "nonexistent_stage_xyz",
                    "new_method": "whatever"
                }
            ]
        });
        write_disposition(pkg.path(), "results_review", body);
        let rel_path = PathBuf::from("runtime/outputs/results_review/sme_disposition.json");
        let _ = scan_session_dispositions(&app, id).await;

        let outcome = apply_actions(&app, id, pkg.path(), &rel_path, false).await;
        assert_eq!(outcome.status, DispositionStatus::Partial);
        assert_eq!(outcome.failed, 1);
        assert!(!outcome.errors.is_empty());
        let session = app.conversation.get_session(id).await.unwrap();
        assert!(session.decisions.iter().any(|d| matches!(
            &d.decision,
            DecisionType::DispositionApplied { outcome, error_reason, .. }
                if outcome == "err" && error_reason.is_some()
        )));
    }

}
