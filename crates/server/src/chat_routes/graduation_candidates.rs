//! v4 P6 / D4 — LocalExtension graduation-candidate routes (design v4 §4.2).
//!
//! Two routes:
//!
//! - `GET /api/chat/session/:id/graduation/candidates` — list every
//!   `LocalExtension` in the cross-session registry that currently
//!   satisfies the graduation thresholds (usage_count >= min,
//!   unique_sessions >= min, success_rate >= min).
//!
//! - `POST /api/chat/session/:id/graduation/:iri/annotate` — record the
//!   SME's "Annotate for upstream submission" action. Writes a
//!   `DecisionType::UserNote` entry onto the session decision-log so the
//!   audit trail captures who annotated when and with what submission
//!   tracker reference; the actual upstream submission is operator
//!   work outside the chat surface.
//!
//! Framing constraint (v4 §4.2): the routes consult the registry at
//! `<sessions_dir>/_local_extension_registry.jsonl` directly via
//! `CrossSessionAggregator`; the registry is global across sessions so
//! the candidates list is the same regardless of which session id is
//! in the URL (the id scopes the audit-log write, not the read).

use super::ChatAppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    Router,
};
use ecaa_workflow_conversation::session::cross_session_aggregator::{
    CrossSessionAggregator, GraduationCandidateSummary,
};
use ecaa_workflow_core::local_extension_graduation::{GraduationConfig, GraduationThresholds};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Response body for `GET.../graduation/candidates`.
#[derive(Debug, Clone, Serialize)]
pub(super) struct GraduationCandidatesResponse {
    /// Active thresholds. Surfaced so the UI can show "(5/3 sessions
    /// needed, 60% success)" alongside each candidate row.
    pub thresholds: GraduationThresholds,
    /// Eligible candidates, descending by `usage_count`.
    pub candidates: Vec<GraduationCandidateSummary>,
}

/// Request body for `POST.../graduation/:iri/annotate`.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct AnnotateGraduationRequest {
    /// Who is performing the annotation (SME username or operator id).
    pub annotated_by: String,
    /// Upstream submission tracker reference (URL / issue id / PR id).
    /// May be empty if the SME wants to record intent before the
    /// submission has a stable id.
    #[serde(default)]
    pub submission_ref: String,
    /// Free-text rationale captured for the audit log.
    #[serde(default)]
    pub rationale: String,
}

/// Resolve the sessions directory the aggregator looks at. Honors
/// `ECAA_CHAT_SESSIONS_DIR`; falls back to
/// `$HOME/.ecaa-workflow/sessions`; fallback `./.ecaa-sessions`
/// so the unit-test path doesn't panic on a HOME-less environment.
fn sessions_dir() -> PathBuf {
    if let Ok(d) = std::env::var("ECAA_CHAT_SESSIONS_DIR") {
        return PathBuf::from(d);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".ecaa-workflow/sessions");
    }
    PathBuf::from("./.ecaa-sessions")
}

/// Resolve the config dir for `local-extension-graduation.yaml`. Honors
/// `ECAA_CONFIG_DIR`; falls back to `./config`.
fn config_dir() -> PathBuf {
    std::env::var("ECAA_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config"))
}

/// `GET /api/chat/session/:id/graduation/candidates` — list every
/// LocalExtension currently eligible to graduate to an upstream
/// ontology.
pub(super) async fn list_candidates(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    // 404 the session-doesn't-exist case so the UI doesn't render
    // candidates against a phantom id.
    if app.conversation.get_session(session_id).await.is_none() {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }
    let thresholds = GraduationConfig::try_load_default(config_dir())
        .ok()
        .flatten()
        .map(|c| c.graduation)
        .unwrap_or_default();
    let aggregator = CrossSessionAggregator::new(sessions_dir());
    // Primary-ontology list left empty here — the UI surfaces the
    // graduation_target_ontology resolved by the aggregator (defaults
    // to "EDAM"). Per-modality routing belongs in the wiring of
    // `materialize_workflow_intent`, not the cross-session candidate
    // list endpoint.
    let candidates = aggregator.list_graduation_candidates(&thresholds, &[]);
    Json(GraduationCandidatesResponse {
        thresholds,
        candidates,
    })
    .into_response()
}

/// `POST /api/chat/session/:id/graduation/:iri/annotate` — record an
/// SME annotation on a graduation candidate. Writes the annotation to
/// the session decision-log so the audit trail captures who annotated
/// when and with what submission reference.
pub(super) async fn annotate_for_upstream(
    State(app): State<ChatAppState>,
    Path((session_id, iri)): Path<(Uuid, String)>,
    Json(body): Json<AnnotateGraduationRequest>,
) -> impl IntoResponse {
    if body.annotated_by.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "annotated_by must be a non-empty string",
        )
            .into_response();
    }
    if iri.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "iri must be a non-empty string").into_response();
    }

    // Verify the iri actually exists in the registry so the annotation
    // doesn't dangle. We don't recompute graduation eligibility here —
    // the SME might be annotating an entry that has already been sent
    // to a submission tracker but not yet accepted.
    let aggregator = CrossSessionAggregator::new(sessions_dir());
    let exists = match aggregator.list_existing() {
        Ok(list) => list.iter().any(|e| e.iri == iri),
        Err(e) => {
            tracing::warn!(
                target: "graduation_candidates",
                err = ?e,
                "list_existing failed; admitting annotation optimistically"
            );
            true
        }
    };
    if !exists {
        return (
            StatusCode::NOT_FOUND,
            format!("iri '{iri}' not in local-extension registry"),
        )
            .into_response();
    }

    let store = app.conversation.store_handle();
    let session_id_str = session_id.to_string();
    let iri_for_record = iri.clone();
    let annotated_by = body.annotated_by;
    let submission_ref = body.submission_ref;
    let rationale = body.rationale;
    match store
        .update(session_id, move |s| {
            let body = format!(
                "Graduation annotation for {iri_for_record} by {annotated_by} \
                 (submission_ref={submission_ref}): {rationale}",
            );
            s.decisions
                .push(ecaa_workflow_core::decision_log::DecisionRecord::new(
                    session_id_str.clone(),
                    ecaa_workflow_core::decision_log::DecisionType::UserNote {
                        task_id: format!("graduation_annotate:{iri_for_record}").into(),
                        body,
                        author: annotated_by.clone(),
                    },
                    ecaa_workflow_core::decision_log::DecisionActor::Sme,
                    if rationale.trim().is_empty() {
                        None
                    } else {
                        Some(rationale.clone())
                    },
                ));
            Ok(())
        })
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            let msg = e.to_string();
            // Typed `ApiError` envelope; UI branches on
            // `code` rather than substring-matching the body.
            if msg.contains("not found") {
                crate::error::ApiError::NotFound(msg).into_response()
            } else {
                crate::error::ApiError::Internal(anyhow::anyhow!(msg)).into_response()
            }
        }
    }
}

/// Route inventory for the doc-as-contract gate.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/chat/session/:id/graduation/candidates"),
    ("POST", "/api/chat/session/:id/graduation/:iri/annotate"),
];

pub(super) fn routes() -> Router<ChatAppState> {
    Router::new()
        .route(
            "/api/chat/session/:id/graduation/candidates",
            axum::routing::get(list_candidates),
        )
        .route(
            "/api/chat/session/:id/graduation/:iri/annotate",
            axum::routing::post(annotate_for_upstream),
        )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{body_json, make_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    #[tokio::test]
    async fn list_candidates_returns_thresholds_for_known_session() {
        // Use a tempdir-backed ECAA_CHAT_SESSIONS_DIR so the test
        // doesn't read the developer's real ~/.ecaa-workflow/sessions
        // registry (and accidentally include in-flight graduation
        // candidates from prior runs). Drop the env var on teardown via
        // a guard so concurrent tests don't trample each other.
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("ECAA_CHAT_SESSIONS_DIR", dir.path());
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/graduation/candidates", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert!(body.get("thresholds").is_some());
        assert!(body.get("candidates").is_some());
        std::env::remove_var("ECAA_CHAT_SESSIONS_DIR");
    }

    #[tokio::test]
    async fn annotate_rejects_empty_annotator() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/graduation/ecaax:x/annotate",
                id
            ))
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"annotated_by": "", "submission_ref": "x", "rationale": "y"}"#,
            ))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
