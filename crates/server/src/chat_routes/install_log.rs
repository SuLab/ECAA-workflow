//! `GET /api/chat/session/:id/install-log` — return the parsed entries
//! from the emitted package's `runtime/install-log.jsonl`.
//!
//! The AuditTab surfaces runtime install events
//! the agent triggered through the install-proxy shims. Each JSONL line
//! is shaped by the proxies as
//! `{ timestamp: f64, atom_id: String, package: String, registry: String, source: String }`.
//! The endpoint reads the file, parses each line as JSON, and returns a
//! `{ entries }` envelope so the UI can render a row per install.
//!
//! Status-code contract:
//! - 200 — file exists and parses; returns the array (possibly empty)
//! - 200 + empty `entries` — session has no emitted package yet OR the
//!   package has no install-log (sealed atoms / declared_only with
//!   nothing dynamic). Matches the per-task logs.rs pattern of always
//!   returning 200 + empty so the UI renders a placeholder rather than
//!   an error.
//! - 400 — path-jail rejected the resolved file path (defense in depth;
//!   the session's emitted_package_path is server-controlled, but
//!   the path-jail invariant requires verification before any FS op).
//! - 404 — unknown session id
//!
//! Path-jail per the install-log file is resolved under
//! `session.emitted_package_path` using `safe_relative_join` (the
//! relative path `runtime/install-log.jsonl` is a server constant — no
//! user input — but we route through the jail helper for belt-and-
//! suspenders coverage and consistency with the rest of `chat_routes`).

use super::{assert_under_root, safe_relative_join, ChatAppState};
use axum::extract::Path;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;
use std::path::PathBuf;
use uuid::Uuid;

/// One row in the install-log surface. Mirrors the proxy's JSONL shape
/// but uses `serde_json::Value` for forward-compatibility — if the
/// proxy adds new fields, the AuditTab can pick them up without
/// re-shipping the server. The canonical fields are validated by the
/// proxy itself, so the server doesn't re-validate here.
#[derive(Debug, Clone, Serialize)]
pub(super) struct InstallLogResponse {
    pub entries: Vec<serde_json::Value>,
}

pub(super) async fn get_install_log(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    // Fast path: unknown session. 404 matches the rest of the chat
    // routes for missing-session responses.
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };

    // No emitted package yet — return an empty list so the UI renders
    // a "no installs recorded" placeholder. Mirrors the progress-log
    // route in tasks/logs.rs.
    let Some(pkg) = session.emitted_package_path.clone() else {
        return Json(InstallLogResponse { entries: vec![] }).into_response();
    };

    // Path-jail. The relative path is a server constant so
    // safe_relative_join will not reject it, but routing through the
    // helper keeps the FS-write/read paths uniform.
    let rel = PathBuf::from("runtime").join("install-log.jsonl");
    let full = match safe_relative_join(&pkg, &rel) {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("install-log path rejected: {e}"),
            )
                .into_response();
        }
    };
    if let Err(e) = assert_under_root(&pkg, &full) {
        return (
            StatusCode::BAD_REQUEST,
            format!("install-log path escapes session root: {e}"),
        )
            .into_response();
    }

    // Missing file is the dominant case for legacy / sealed packages —
    // return empty entries (200) rather than 404 so the AuditTab
    // doesn't surface a scary error for the common "no runtime
    // installs" path.
    let raw = match tokio::fs::read_to_string(&full).await {
        Ok(s) => s,
        Err(_) => {
            return Json(InstallLogResponse { entries: vec![] }).into_response();
        }
    };

    // JSONL parser tolerant of trailing empty lines + blank lines.
    // Malformed lines are skipped quietly rather than failing the whole
    // request — the proxy is the source of truth, and we'd rather show
    // 9 of 10 entries than break the entire pane on one corrupt row.
    let entries: Vec<serde_json::Value> = raw
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect();

    Json(InstallLogResponse { entries }).into_response()
}

/// Route inventory for the doc-as-contract gate +
/// route-count parity assertion against CLAUDE.md. (METHOD, PATH).
pub(super) const ROUTES: &[(&str, &str)] = &[("GET", "/api/chat/session/:id/install-log")];

/// Per-domain `routes()` builder. `mod.rs::router()`
/// merges every submodule's builder into the single chat surface.
pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/install-log",
        axum::routing::get(get_install_log),
    )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{body_json, make_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    /// Seed an in-memory session, write an install-log fixture under
    /// its emitted_package_path, and assert the endpoint returns the
    /// parsed entries.
    #[tokio::test]
    async fn returns_parsed_entries_when_install_log_present() {
        let (app, state) = make_router(vec![]).await;
        let (session_id, _) = state.conversation.start_session(false).await.unwrap();

        // Plant the install-log under a tempdir + wire it as the
        // emitted_package_path. Mirrors the conversation crate's
        // install_log_registered_as_creative_work_when_present fixture.
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("install-log.jsonl"),
            "{\"timestamp\":1700000000.0,\"atom_id\":\"rnaseq_align\",\"package\":\"samtools\",\"registry\":\"apt\",\"source\":\"agent_runtime\"}\n\
{\"timestamp\":1700000010.5,\"atom_id\":\"rnaseq_align\",\"package\":\"pandas\",\"registry\":\"pip\",\"source\":\"agent_runtime\"}\n",
        )
        .unwrap();
        state
            .conversation
            .store_handle()
            .update(session_id, |s| {
                s.emitted_package_path = Some(tmp.path().to_path_buf());
                Ok(())
            })
            .await
            .unwrap();

        let res = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/chat/session/{session_id}/install-log"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res.into_body()).await;
        let entries = body["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["atom_id"], "rnaseq_align");
        assert_eq!(entries[0]["package"], "samtools");
        assert_eq!(entries[1]["registry"], "pip");
    }

    /// Missing install-log file (sealed packages / declared_only with
    /// no runtime installs) — the endpoint must return 200 + empty
    /// entries so the AuditTab can render a "no installs recorded"
    /// placeholder rather than an error.
    #[tokio::test]
    async fn returns_empty_entries_when_install_log_missing() {
        let (app, state) = make_router(vec![]).await;
        let (session_id, _) = state.conversation.start_session(false).await.unwrap();
        let tmp = tempfile::tempdir().unwrap();
        // No file planted under runtime/.
        state
            .conversation
            .store_handle()
            .update(session_id, |s| {
                s.emitted_package_path = Some(tmp.path().to_path_buf());
                Ok(())
            })
            .await
            .unwrap();

        let res = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/chat/session/{session_id}/install-log"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res.into_body()).await;
        assert_eq!(body["entries"].as_array().unwrap().len(), 0);
    }

    /// Session with no emitted package — returns 200 + empty entries.
    #[tokio::test]
    async fn returns_empty_entries_when_no_package_emitted() {
        let (app, state) = make_router(vec![]).await;
        let (session_id, _) = state.conversation.start_session(false).await.unwrap();
        let res = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/chat/session/{session_id}/install-log"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res.into_body()).await;
        assert_eq!(body["entries"].as_array().unwrap().len(), 0);
    }

    /// Skips malformed JSONL lines so the panel keeps rendering the
    /// well-formed rows. Defensive: the proxy is the source of truth,
    /// but a corrupt line shouldn't kill the entire pane.
    #[tokio::test]
    async fn skips_malformed_jsonl_lines() {
        let (app, state) = make_router(vec![]).await;
        let (session_id, _) = state.conversation.start_session(false).await.unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let runtime = tmp.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(
            runtime.join("install-log.jsonl"),
            "{\"atom_id\":\"a\",\"package\":\"p1\"}\n\
not a json line\n\
{\"atom_id\":\"a\",\"package\":\"p2\"}\n",
        )
        .unwrap();
        state
            .conversation
            .store_handle()
            .update(session_id, |s| {
                s.emitted_package_path = Some(tmp.path().to_path_buf());
                Ok(())
            })
            .await
            .unwrap();

        let res = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/chat/session/{session_id}/install-log"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = body_json(res.into_body()).await;
        let entries = body["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["package"], "p1");
        assert_eq!(entries[1]["package"], "p2");
    }

    /// Unknown session returns 404.
    #[tokio::test]
    async fn unknown_session_returns_404() {
        let (app, _) = make_router(vec![]).await;
        let bogus = uuid::Uuid::new_v4();
        let res = app
            .oneshot(
                Request::builder()
                    .uri(format!("/api/chat/session/{bogus}/install-log"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}
