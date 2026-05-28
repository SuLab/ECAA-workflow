//! Disposition review (reject) path.
//!
//! Owns the SME-rejection POST handler. No mutations run on reject —
//! it just flips the on-disk status to `rejected`, records a
//! `DispositionRejected` decision, and refreshes the in-memory queue
//! so the UI's review card reflects the new state.

use super::*;
use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use uuid::Uuid;

#[derive(Debug, serde::Deserialize, Default)]
pub(super) struct RejectRequest {
    #[serde(default)]
    pub rationale: Option<String>,
}

/// `POST /api/chat/session/:id/dispositions/reject/*path` — mark the
/// disposition as rejected. No mutations run. Writes a
/// `DispositionRejected` decision record.
pub(super) async fn post_reject(
    State(app): State<ChatAppState>,
    AxumPath((session_id, raw_path)): AxumPath<(Uuid, String)>,
    body: Option<BoundedJson<RejectRequest>>,
) -> impl IntoResponse {
    let req = body.map(|BoundedJson(r)| r).unwrap_or_default();
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
    if matches!(
        d.status,
        DispositionStatus::Applied | DispositionStatus::Rejected
    ) {
        // Idempotent — nothing to do.
        return (
            StatusCode::OK,
            Json(serde_json::json!({ "status": d.status })),
        )
            .into_response();
    }
    d.status = DispositionStatus::Rejected;
    d.status_updated_at = Some(ecaa_workflow_core::time_helpers::now_rfc3339());
    persist_disposition(&pkg, &rel_path, &d);
    queue_upsert(&app.dispositions, session_id, rel_path.clone(), d.clone()).await;
    record_rejected_decision(&app, session_id, &rel_path, req.rationale.clone()).await;
    (
        StatusCode::OK,
        Json(serde_json::json!({ "status": "rejected" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn ivd_v0_disposition() -> serde_json::Value {
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
    async fn reject_endpoint_marks_disposition_rejected_and_logs_decision() {
        let pkg = tempfile::tempdir().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(
            &app,
            "batch_correction",
            Some(pkg.path().to_path_buf()),
        )
        .await;
        write_disposition(pkg.path(), "results_review", ivd_v0_disposition());

        // Hydrate the queue first.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/dispositions/scan", id))
            .body(Body::empty())
            .unwrap();
        let _ = router.clone().oneshot(req).await.unwrap();

        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/dispositions/reject/runtime/outputs/results_review/sme_disposition.json",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"rationale":"not ready to re-run the full slice"}"#,
            ))
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Disk status is now rejected.
        let disk: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                pkg.path()
                    .join("runtime/outputs/results_review/sme_disposition.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(disk["status"], "rejected");
        assert!(disk["status_updated_at"].is_string());

        // In-memory session has a DispositionRejected record.
        let session = app.conversation.get_session(id).await.unwrap();
        assert!(session
            .decisions
            .iter()
            .any(|d| matches!(
                &d.decision,
                DecisionType::DispositionRejected { rationale, .. } if rationale.as_deref() == Some("not ready to re-run the full slice")
            )));
    }
}
