//! Disposition scan + ingest paths.
//!
//! Owns the three detection triggers that hydrate the
//! `DispositionQueue` (progress-event ingest, post-task-complete
//! backfill, full disk rescan) plus the GET-shaped read handlers
//! (`/dispositions`, `/dispositions/view/*path`) and the rebuild
//! `POST /dispositions/scan` endpoint.

use super::*;
use axum::{
    extract::{Path as AxumPath, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use uuid::Uuid;

/// Wire shape for `GET /dispositions`. Each entry mirrors the parsed
/// `Disposition` plus a human-friendly count of applicable actions
/// so the UI doesn't need to re-filter on status.
#[derive(Debug, Serialize)]
pub(super) struct DispositionListEntry {
    pub path: String,
    pub task_id: String,
    pub status: DispositionStatus,
    pub schema_version: u32,
    pub action_count: usize,
    pub created_at: Option<String>,
    pub authoritative_interpretation: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct DispositionListResponse {
    pub session_id: Uuid,
    pub dispositions: Vec<DispositionListEntry>,
}

/// Detection trigger (1). Called from the `post_progress` handler in
/// `events.rs` when the harness forwards an agent-written
/// `disposition_proposed` event. `detail` carries the relative path
/// to the disposition file; the server reads + normalises it, enqueues
/// it, writes a `DispositionProposed` decision, and optionally
/// auto-applies when the §5.4 double-gate is open.
pub(crate) async fn ingest_disposition_from_progress_event(
    app: &ChatAppState,
    session_id: SessionId,
    detail: &str,
) {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return;
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return;
    };
    // `detail` is expected to be the relative path
    // `runtime/outputs/<task>/sme_disposition.json`. Reject anything
    // that doesn't match so a rogue agent can't push us to scan random
    // paths.
    //
    // `Path::starts_with` is component-aware but does NOT reject
    // embedded `..` components — `runtime/outputs/../../X` would pass
    // the prefix check. `safe_relative_join` rejects any `..` component
    // before we attempt the load. Belt-and-suspenders: `load_disposition`
    // re-canonicalises, but we fail fast here.
    let rel_path = PathBuf::from(detail.trim());
    if !rel_path.starts_with("runtime/outputs/") || !rel_path.ends_with("sme_disposition.json") {
        eprintln!(
            "[disposition_ingest] ignoring suspicious path: {:?}",
            rel_path
        );
        return;
    }
    if let Err(e) = super::super::safe_relative_join(&pkg, &rel_path) {
        eprintln!(
            "[disposition_ingest] rejecting traversal path {:?}: {}",
            rel_path, e
        );
        return;
    }
    let disposition = match load_disposition(&pkg, &rel_path) {
        Ok(d) => d,
        Err(e) => {
            // Demoted from
            // `eprintln!` so the session-id surface stays in the
            // structured tracing pipeline alongside the http span.
            tracing::warn!(
                ?session_id,
                path = %rel_path.display(),
                error = %e,
                "disposition_ingest failed to load"
            );
            return;
        }
    };
    record_proposed_decision(app, session_id, &rel_path, &disposition).await;
    queue_upsert(
        &app.dispositions,
        session_id,
        rel_path.clone(),
        disposition.clone(),
    )
    .await;
    // v2 escape hatch — apply automatically when both flags align.
    if auto_apply_enabled_globally() && disposition.auto_apply {
        eprintln!(
            "[disposition_ingest] auto-applying {} (env + file both set)",
            rel_path.display()
        );
        let _ = apply_actions(app, session_id, &pkg, &rel_path, true).await;
    }
}

/// Detection trigger (2) — §7.4. Called from the progress handler's
/// `task_completed` branch. A completed task may have left a new
/// `sme_disposition.json` on disk next to its outputs. Loads it and
/// enqueues it (no-op when the file is absent).
pub(crate) async fn scan_after_task_completed(
    app: &ChatAppState,
    session_id: SessionId,
    task_id: &str,
) {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return;
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return;
    };
    let rel_path = PathBuf::from("runtime/outputs")
        .join(task_id)
        .join("sme_disposition.json");
    let disk_path = pkg.join(&rel_path);
    if !disk_path.exists() {
        return;
    }
    // Already in the queue? Skip — first observation wins.
    {
        let guard = app.dispositions.read().await;
        if let Some(map) = guard.get(&session_id) {
            if map.contains_key(&rel_path) {
                return;
            }
        }
    }
    let disposition = match load_disposition(&pkg, &rel_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "[disposition_scan_on_complete] skipping {}: {}",
                disk_path.display(),
                e
            );
            return;
        }
    };
    record_proposed_decision(app, session_id, &rel_path, &disposition).await;
    queue_upsert(
        &app.dispositions,
        session_id,
        rel_path.clone(),
        disposition.clone(),
    )
    .await;
    if auto_apply_enabled_globally() && disposition.auto_apply {
        let _ = apply_actions(app, session_id, &pkg, &rel_path, true).await;
    }
}

/// Full queue rebuild from disk. Powers `POST /dispositions/scan`.
pub(super) async fn scan_session_dispositions(
    app: &ChatAppState,
    session_id: SessionId,
) -> Option<usize> {
    let session = app.conversation.get_session(session_id).await?;
    let pkg = session.emitted_package_path.clone()?;
    let on_disk = scan_disk(&pkg);
    let count = on_disk.len();
    // Record DispositionProposed for any disposition the queue hadn't
    // seen before this scan — replaying the audit trail after a
    // server restart.
    {
        let guard = app.dispositions.read().await;
        let known: std::collections::HashSet<PathBuf> = guard
            .get(&session_id)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        drop(guard);
        for (path, d) in &on_disk {
            if !known.contains(path) && d.status == DispositionStatus::Pending {
                record_proposed_decision(app, session_id, path, d).await;
            }
        }
    }
    let mut guard = app.dispositions.write().await;
    guard.insert(session_id, on_disk);
    Some(count)
}

// ── HTTP handlers ────────────────────────────────────────────────────────────

/// `GET /api/chat/session/:id/dispositions`
pub(super) async fn list_dispositions(
    State(app): State<ChatAppState>,
    AxumPath(session_id): AxumPath<Uuid>,
) -> impl IntoResponse {
    if app.conversation.get_session(session_id).await.is_none() {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    }
    // Hydrate lazily — first call after a server restart should see
    // disk state even without an explicit scan.
    let needs_hydrate = {
        let guard = app.dispositions.read().await;
        guard.get(&session_id).map(|m| m.is_empty()).unwrap_or(true)
    };
    if needs_hydrate {
        let _ = scan_session_dispositions(&app, session_id).await;
    }
    let guard = app.dispositions.read().await;
    let entries: Vec<DispositionListEntry> = guard
        .get(&session_id)
        .map(|m| {
            m.iter()
                .map(|(path, d)| DispositionListEntry {
                    path: path.to_string_lossy().to_string(),
                    task_id: d.task_id.to_string(),
                    status: d.status,
                    schema_version: d.schema_version,
                    action_count: d.actions.len(),
                    created_at: d.created_at.clone(),
                    authoritative_interpretation: d.authoritative_interpretation.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    Json(DispositionListResponse {
        session_id,
        dispositions: entries,
    })
    .into_response()
}

/// `GET /api/chat/session/:id/dispositions/view/*path` — normalized single
/// disposition. 404 when the file or session is missing.
pub(super) async fn get_disposition(
    State(app): State<ChatAppState>,
    AxumPath((session_id, raw_path)): AxumPath<(Uuid, String)>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return (StatusCode::BAD_REQUEST, "session has no emitted package").into_response();
    };
    let rel_path = decode_path(&raw_path);
    match queue_get_or_load(&app.dispositions, session_id, &pkg, &rel_path).await {
        Some(d) => Json(d).into_response(),
        None => (StatusCode::NOT_FOUND, "disposition not found").into_response(),
    }
}

/// `POST /api/chat/session/:id/dispositions/scan` — rebuild the
/// in-memory queue from disk. Used by the UI on pane mount to recover
/// from a server restart that lost the in-memory queue.
pub(super) async fn post_scan(
    State(app): State<ChatAppState>,
    AxumPath(session_id): AxumPath<Uuid>,
) -> impl IntoResponse {
    match scan_session_dispositions(&app, session_id).await {
        Some(n) => Json(serde_json::json!({
            "session_id": session_id,
            "dispositions_found": n,
        }))
        .into_response(),
        None => (StatusCode::NOT_FOUND, "session not found or no package").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_routes::test_support::{make_router, seed_session_with_completed_task};
    use axum::body::Body;
    use axum::http::Request;
    use ecaa_workflow_core::decision_log::DecisionType;
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
    async fn scan_endpoint_discovers_on_disk_disposition() {
        let pkg = tempfile::tempdir().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(
            &app,
            "batch_correction",
            Some(pkg.path().to_path_buf()),
        )
        .await;
        write_disposition(pkg.path(), "results_review", ivd_v0_disposition());

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/dispositions/scan", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = crate::chat_routes::test_support::body_json(resp.into_body()).await;
        assert_eq!(body["dispositions_found"].as_u64().unwrap(), 1);
    }

    #[tokio::test]
    async fn list_endpoint_lazy_hydrates_from_disk() {
        let pkg = tempfile::tempdir().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(
            &app,
            "batch_correction",
            Some(pkg.path().to_path_buf()),
        )
        .await;
        write_disposition(pkg.path(), "results_review", ivd_v0_disposition());

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/dispositions", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = crate::chat_routes::test_support::body_json(resp.into_body()).await;
        let items = body["dispositions"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["task_id"], "results_review");
        assert_eq!(items[0]["status"], "pending");
        assert_eq!(items[0]["schema_version"], 1);
        assert!(items[0]["action_count"].as_u64().unwrap() >= 1);
    }

    #[tokio::test]
    async fn get_endpoint_returns_normalized_disposition() {
        let pkg = tempfile::tempdir().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(
            &app,
            "batch_correction",
            Some(pkg.path().to_path_buf()),
        )
        .await;
        write_disposition(pkg.path(), "results_review", ivd_v0_disposition());

        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/dispositions/view/runtime/outputs/results_review/sme_disposition.json",
                id
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = crate::chat_routes::test_support::body_json(resp.into_body()).await;
        assert_eq!(body["task_id"], "results_review");
        // Normalized to v1 with actions[] regardless of the source.
        let actions = body["actions"].as_array().unwrap();
        assert!(!actions.is_empty());
        assert_eq!(actions[0]["kind"], "amend_method");
    }

    #[tokio::test]
    async fn auto_apply_requires_both_gates() {
        // File-flag alone, no env var ⇒ no auto-apply. The disk file
        // keeps the agent's original shape (no `status` field written
        // by the server at all). Confirms we didn't enter the apply
        // branch.
        std::env::remove_var("SWFC_AUTO_APPLY_DISPOSITIONS");
        let pkg = tempfile::tempdir().unwrap();
        let (_router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(
            &app,
            "batch_correction",
            Some(pkg.path().to_path_buf()),
        )
        .await;
        let mut body = ivd_v0_disposition();
        body["auto_apply"] = serde_json::Value::Bool(true);
        write_disposition(pkg.path(), "results_review", body);
        ingest_disposition_from_progress_event(
            &app,
            id,
            "runtime/outputs/results_review/sme_disposition.json",
        )
        .await;
        // With env unset, disposition file is unchanged on disk —
        // no status key was written back by auto-apply.
        let disk: serde_json::Value = serde_json::from_slice(
            &std::fs::read(
                pkg.path()
                    .join("runtime/outputs/results_review/sme_disposition.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let status = disk.get("status").and_then(|v| v.as_str());
        assert!(
            matches!(status, None | Some("pending")),
            "env-off ⇒ auto-apply must not fire, got status={:?}",
            status
        );
        // Queue should have the ingested disposition.
        let guard = app.dispositions.read().await;
        assert!(guard.get(&id).map(|m| !m.is_empty()).unwrap_or(false));
        // DispositionProposed record should be on the session log.
        let session = app.conversation.get_session(id).await.unwrap();
        assert!(session
            .decisions
            .iter()
            .any(|d| matches!(&d.decision, DecisionType::DispositionProposed { .. })));
    }

    // ── Path-jail (disposition ingest) ───────────────────────────────────
    //
    // The `starts_with("runtime/outputs/")` prefix check is
    // component-aware but `Path::starts_with` does not reject embedded
    // `..` components — a hostile agent could submit
    // `runtime/outputs/../../escape/sme_disposition.json` and the
    // prefix-check would pass, then `load_disposition` would join into
    // a path outside the package root. `safe_relative_join` rejects any
    // `..` component before we attempt the load.
    #[tokio::test]
    async fn ingest_disposition_rejects_traversal_rel_path() {
        std::env::remove_var("SWFC_AUTO_APPLY_DISPOSITIONS");
        let pkg = tempfile::tempdir().unwrap();
        let (_router, app) = make_router(vec![]).await;
        let id = seed_session_with_completed_task(
            &app,
            "batch_correction",
            Some(pkg.path().to_path_buf()),
        )
        .await;
        // Plant a victim file outside the package that an unguarded
        // `pkg.join(rel_path)` would dereference.
        let outside = pkg.path().parent().unwrap().join("escape");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("sme_disposition.json"),
            serde_json::to_vec_pretty(&ivd_v0_disposition()).unwrap(),
        )
        .unwrap();
        // Detail string passes the prefix + suffix check but contains
        // `..` components that escape the package on join.
        ingest_disposition_from_progress_event(
            &app,
            id,
            "runtime/outputs/../../escape/sme_disposition.json",
        )
        .await;
        // Queue must be empty — load was rejected.
        let guard = app.dispositions.read().await;
        assert!(
            guard.get(&id).map(|m| m.is_empty()).unwrap_or(true),
            "traversal rel_path must not be queued"
        );
        let session = app.conversation.get_session(id).await.unwrap();
        assert!(
            !session
                .decisions
                .iter()
                .any(|d| matches!(&d.decision, DecisionType::DispositionProposed { .. })),
            "no DispositionProposed should land for a traversal path"
        );
    }
}
