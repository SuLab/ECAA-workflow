//! Verification + pilot/diff read endpoints. `verify_task_endpoint`
//! runs the claim-extractor/verifier pair over a completed task's
//! narrative artifact and transitions the session to
//! `Blocked { ValidationFailed }` on mismatch. The pilot/diff reads
//! (`get_pilot_report`, `get_cross_version_diff`,
//! `get_cross_version_diff_table`) are static-artifact fetches —
//! different semantic domain, same subroot, so they share this file.

use super::*;
use axum::{
    body::Body, extract::State, http::StatusCode, response::IntoResponse, response::Response, Json,
};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

/// Cap on bytes read into memory when slurping a JSON sidecar
/// (`runtime/pilot/report.json`, `runtime/cross-version-diff.json`).
/// Without this cap, `tokio::fs::read` would allocate the whole file
/// before the JSON parser ran — a malformed or runaway sidecar from
/// an adversarial agent could allocate hundreds of MB on a single GET.
/// 16 MB comfortably covers the largest pilot reports + cross-version
/// diffs that occur in practice; a sidecar larger than that points
/// at a bug in the producer rather than a real payload the SME needs
/// surfaced.
const SIDECAR_READ_CAP_BYTES: u64 = 16 * 1024 * 1024;

/// Open `path`, read at most `SIDECAR_READ_CAP_BYTES` into memory,
/// and parse as JSON. Returns `Ok(None)` on missing file (treat as
/// 404); `Ok(Some(v))` on successful parse; `Err(_)` on I/O or
/// deserialisation error.
async fn read_capped_json(path: &std::path::Path) -> std::io::Result<Option<serde_json::Value>> {
    let file = match tokio::fs::File::open(path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let mut buf = Vec::new();
    file.take(SIDECAR_READ_CAP_BYTES)
        .read_to_end(&mut buf)
        .await?;
    let v = serde_json::from_slice::<serde_json::Value>(&buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(Some(v))
}

/// Surface the latest pilot report for this session,
/// read from `<emitted_package>/runtime/pilot/report.json`.
///
/// Returns 404 (canonical `ApiError::NotFound` envelope) when the
/// session hasn't emitted or no pilot ran. The UI's polling layer
/// should treat `404 not_found` here as "feature unavailable" and
/// stop polling rather than rendering a console error per tick.
/// A `200 + null body` shape would conflate three distinct states
/// (session missing / package not yet emitted / feature simply not
/// active) into one indistinguishable payload and would prevent
/// UI fetch hooks that branch on HTTP status from telling "no data
/// yet" from "happy success with empty result".
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub async fn get_pilot_report(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return crate::error::ApiError::NotFound("session not found".into()).into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return crate::error::ApiError::NotFound(
            "pilot report not yet produced (package not yet emitted)".into(),
        )
        .into_response();
    };
    let p = root.join("runtime").join("pilot").join("report.json");
    match read_capped_json(&p).await {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => crate::error::ApiError::NotFound(
            "pilot report not yet produced for this session".into(),
        )
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Return the whole-package `CrossVersionReport` for
/// this session's latest emit.
///
/// Returns 404 when no diff was produced (no parent lineage, no
/// overlapping tables, or session not yet emitted). The previous
/// "200 + null body" shape conflated multiple absence states; see
/// the rationale on `get_pilot_report`.
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub async fn get_cross_version_diff(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return crate::error::ApiError::NotFound("session not found".into()).into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return crate::error::ApiError::NotFound(
            "cross-version diff not available (package not yet emitted)".into(),
        )
        .into_response();
    };
    let p = root.join("runtime").join("cross-version-diff.json");
    match read_capped_json(&p).await {
        Ok(Some(v)) => Json(v).into_response(),
        Ok(None) => crate::error::ApiError::NotFound(
            "cross-version diff not produced for this session".into(),
        )
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Return a single table's diff CSV contents. Table
/// name is URL-encoded; the per-table file path is
/// `runtime/cross-version-diff-<sanitised-name>.csv`.
///
/// Streams the file rather than reading it whole — a multi-million-row
/// concordance CSV slurped into a `String` would block the response
/// header behind a multi-GB allocation.
pub async fn get_cross_version_diff_table(
    State(app): State<ChatAppState>,
    Path((session_id, table_name)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return (StatusCode::NOT_FOUND, "session has no emitted package").into_response();
    };
    let safe: String = table_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let p = root
        .join("runtime")
        .join(format!("cross-version-diff-{}.csv", safe));
    let file = match tokio::fs::File::open(&p).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "table diff not found").into_response(),
    };
    let stream = tokio_util::io::ReaderStream::new(file);
    Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/csv")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// return both the parent package's
/// and the current package's raw table content so the UI can render a
/// side-by-side diff without a second round-trip.
///
/// Probes `results/tables/<name>.tsv` first (standard location), then
/// `results/tables/<name>.csv`. 404 when the table is missing from
/// either side. The UI renders "not present in parent / current"
/// as-empty when one side only has the file.
pub(super) async fn get_cross_version_diff_table_pair(
    State(app): State<ChatAppState>,
    Path((session_id, table_name)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(current_root) = session.emitted_package_path.clone() else {
        return (StatusCode::NOT_FOUND, "session has no emitted package").into_response();
    };
    let parent_root: Option<std::path::PathBuf> = session
        .lineage
        .as_ref()
        .and_then(|l| l.parent_emitted_package_path.clone());

    let safe: String = table_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();

    /// Per-side cap on bytes read into the JSON envelope. The endpoint
    /// hands both halves back inline so the UI can render them
    /// side-by-side, which means the in-memory ceiling is fixed at
    /// `2 * READ_CAP_BYTES + json overhead`. Cap chosen to comfortably
    /// fit the largest concordance tables seen in practice (~3 MB)
    /// while refusing to allocate hundreds of MB on a runaway agent CSV.
    const READ_CAP_BYTES: u64 = 8 * 1024 * 1024;
    async fn read_table(root: &std::path::Path, stem: &str) -> Option<(String, &'static str)> {
        for ext in ["tsv", "csv"] {
            let p = root
                .join("results")
                .join("tables")
                .join(format!("{}.{}", stem, ext));
            let Ok(file) = tokio::fs::File::open(&p).await else {
                continue;
            };
            let mut buf = Vec::new();
            if file
                .take(READ_CAP_BYTES)
                .read_to_end(&mut buf)
                .await
                .is_err()
            {
                continue;
            }
            let mime = if ext == "tsv" {
                "text/tab-separated-values"
            } else {
                "text/csv"
            };
            return Some((String::from_utf8_lossy(&buf).into_owned(), mime));
        }
        None
    }

    let current = read_table(&current_root, &safe).await;
    let parent = match parent_root.as_deref() {
        Some(r) => read_table(r, &safe).await,
        None => None,
    };

    if current.is_none() && parent.is_none() {
        return (StatusCode::NOT_FOUND, "table absent on both sides").into_response();
    }

    let body = serde_json::json!({
        "table_name": table_name,
        "current": current.as_ref().map(|(s, mime)| serde_json::json!({
            "mime": *mime,
            "body": s,
        })),
        "parent": parent.as_ref().map(|(s, mime)| serde_json::json!({
            "mime": *mime,
            "body": s,
        })),
    });
    Json(body).into_response()
}

/// POST companion to `get_task_result`: re-runs claim verification for a
/// completed task, and if any mismatch is found transitions the session
/// to `Blocked { ValidationFailed }`. Idempotent — calling it on a
/// session already Blocked with the same task just returns the fresh
/// report without double-transitioning.
#[tracing::instrument(skip(app), fields(session_id = %session_id, task_id = %task_id))]
pub async fn verify_task_endpoint(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return (StatusCode::BAD_REQUEST, "session has no emitted package").into_response();
    };
    // Jail the URL-path `task_id` before passing it
    // to `verify_task_with_context`, which joins it into
    // `package_root.join("runtime").join(task_id)` with no check.
    // safe_segment_join rejects '..' / absolute / separator-bearing
    // task_ids with 400.
    let joined = match super::safe_segment_join(&root.join("runtime"), &task_id) {
        Ok(p) => p,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid task_id: {}", e)).into_response();
        }
    };
    if let Err(e) = super::assert_under_root(&root, &joined) {
        return (StatusCode::FORBIDDEN, format!("path escapes root: {}", e)).into_response();
    }
    let config_dir = super::tasks::config_dir_or_default();
    let Some(verified) = crate::verification::verify_task_with_context(
        &root,
        &task_id,
        &config_dir,
        session.project_class,
        &session.decisions,
        session.mode.is_confirmatory(),
    ) else {
        return Json(serde_json::json!({
            "report": serde_json::Value::Null,
            "reason": "no narrative artifact, no policy, or verification disabled",
        }))
        .into_response();
    };

    // Write a `DecisionType::ClaimVerification` row regardless of
    // mismatch status so the Decisions tab + replay tooling see every
    // verifier round-trip. Without this row, the mismatch path drives
    // a Blocked transition (via `block_from_harness`) but no
    // decision-log row lands; the happy path (no mismatch) writes
    // nothing at all. Every verify call appends one durable audit row.
    let claim_task_id = task_id.clone();
    let n_verified = verified.report.n_verified;
    let n_mismatch = verified.report.n_mismatch;
    let _ = app
        .conversation
        .store_handle()
        .update(session_id, move |s| {
            s.record_decision(
                ecaa_workflow_core::decision_log::DecisionType::ClaimVerification {
                    task_id: claim_task_id.into(),
                    n_verified,
                    n_mismatch,
                },
                ecaa_workflow_core::decision_log::DecisionActor::Harness,
                None,
            );
            Ok(())
        })
        .await;

    // On mismatch, block the session so the UI's BlockerCard surfaces
    // the recovery affordances (amend_stage_method or rerun_task). Use
    // the Validation variant of BlockerKind so the dispatch lands in
    // the right UI branch.
    if verified.report.has_mismatch() {
        let first_mismatch = verified
            .report
            .verdicts
            .iter()
            .find(|v| {
                matches!(
                    &v.status,
                    ecaa_workflow_core::claim_verifier::ClaimStatus::Mismatch { .. }
                )
            })
            .map(|v| v.claim.entity.clone())
            .unwrap_or_else(|| "unknown".into());
        let detail = format!(
            "{} claim mismatch(es) detected while verifying task {} (first: {})",
            verified.report.n_mismatch, task_id, first_mismatch
        );
        let kind = ecaa_workflow_core::blocker::BlockerKind::ValidationFailed {
            check: format!("claim_verification:{}", task_id),
            message: detail.clone(),
            cause: None,
        };
        if let Err(e) = app
            .conversation
            .block_from_harness(session_id, task_id.clone(), detail, kind)
            .await
        {
            // Soft-fail: still return the report. Most likely cause is
            // the session isn't in Emitted anymore (already Blocked),
            // which is the idempotent case — the UI shows the report
            // and the earlier blocker stays surfaced.
            eprintln!("[verify_task_endpoint] block_from_harness no-op: {}", e);
        } else if let Some(s) = app.conversation.get_session(session_id).await {
            app.broadcast(
                session_id,
                SsePayload::StateAdvanced {
                    new_state: s.state.clone(),
                },
            )
            .await;
        }
    }

    Json(serde_json::json!({
        "report": verified.report,
        "narrative_path": verified.narrative_path,
    }))
    .into_response()
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/chat/session/:id/task/:task_id/verify"),
    ("GET", "/api/chat/session/:id/pilot"),
    ("GET", "/api/chat/session/:id/cross-version-diff"),
    (
        "GET",
        "/api/chat/session/:id/cross-version-diff/:table_name",
    ),
    (
        "GET",
        "/api/chat/session/:id/cross-version-diff/tables/:table_name",
    ),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/task/:task_id/verify",
            axum::routing::post(verify_task_endpoint),
        )
        .route(
            "/api/chat/session/:id/pilot",
            axum::routing::get(get_pilot_report),
        )
        .route(
            "/api/chat/session/:id/cross-version-diff",
            axum::routing::get(get_cross_version_diff),
        )
        .route(
            "/api/chat/session/:id/cross-version-diff/:table_name",
            axum::routing::get(get_cross_version_diff_table),
        )
        .route(
            "/api/chat/session/:id/cross-version-diff/tables/:table_name",
            axum::routing::get(get_cross_version_diff_table_pair),
        )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{make_router, seed_session_with_completed_task};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    // ── Path-jail ─────────────────────────────────────────────────────────
    //
    // `verify_task_endpoint` took a URL-path `task_id` and passed it
    // directly to `verify_task_with_context`, which joined into
    // `package_root.join("runtime").join(task_id)` to find the
    // narrative artifact. A `..`-bearing task_id would be silently
    // collapsed to a path outside the package — a probe channel rather
    // than a write sink, but still a violation of the host-executor
    // boundary. safe_segment_join now rejects it with 400.
    #[tokio::test]
    async fn verify_task_endpoint_rejects_traversal_task_id() {
        let pkg = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/task/%2E%2E%2Fescape/verify",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "traversal task_id must be rejected with 400"
        );
    }
}
