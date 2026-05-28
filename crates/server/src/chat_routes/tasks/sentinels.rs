//! Status-sentinel scan for `runtime/outputs/<task_id>/`.
//!
//! Endpoint (plan §S16.2):
//! - GET /session/:id/task/:task_id/status-sentinels
//!
//! Long-running scripts (Seurat install, integration runs, smoke tests)
//! write companion files like `integration_status.OK` /
//! `integration_status.FAILED` / `install_status` to signal completion
//! out-of-band of `WORKFLOW.json`. Today these are invisible to the
//! SME — the UI only tails `progress.log`. Surfacing them with a
//! status-coloured chip lets the SME see at a glance that an agent
//! sub-script crashed even when the parent task hasn't yet
//! transitioned to Blocked/Failed.

use super::super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use uuid::Uuid;

/// Recognised patterns (top-level files only, no recursion):
/// * `*.OK` → status = "ok"
/// * `*.FAILED` → status = "failed"
/// * `*.PENDING` → status = "pending"
/// * `<name>_status` (no suffix) → status from first non-blank line
///   ("ok" / "failed" / "running" — anything else preserved verbatim)
///
/// Always 200 with a possibly-empty array. Truncates each sentinel's
/// `body` to the first 512 bytes so the UI can show inline context
/// (e.g. `exit=$status` from a wrapper) without unbounded payloads.
pub async fn get_task_status_sentinels(
    State(app): State<ChatAppState>,
    Path((session_id, task_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return Json(serde_json::json!({ "sentinels": [] })).into_response();
    };
    let dir = match super::super::_path_jail::runtime_outputs_for_task(&pkg, &task_id) {
        Ok(d) => d,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid task_id").into_response(),
    };
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return Json(serde_json::json!({ "sentinels": [] })).into_response(),
    };
    let mut out: Vec<serde_json::Value> = Vec::new();
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let name = match entry.file_name().into_string() {
            Ok(n) => n,
            Err(_) => continue,
        };
        let kind = classify_status_filename(&name);
        let Some(kind) = kind else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let body = std::fs::read_to_string(entry.path())
            .ok()
            .map(|s| s.chars().take(512).collect::<String>())
            .unwrap_or_default();
        out.push(serde_json::json!({
            "name": name,
            "kind": kind,
            "mtime_unix": mtime,
            "body": body.trim().to_string(),
        }));
    }
    out.sort_by(|a, b| {
        b["mtime_unix"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&a["mtime_unix"].as_u64().unwrap_or(0))
    });
    Json(serde_json::json!({ "sentinels": out })).into_response()
}

/// `*.OK` / `*.FAILED` / `*.PENDING` are the explicit-suffix forms.
/// `<name>_status` (no suffix) is also recognised — read the file body
/// to derive the kind. Returns `None` for unrelated files so the scan
/// only surfaces real status sentinels.
fn classify_status_filename(name: &str) -> Option<&'static str> {
    if name.ends_with(".OK") {
        return Some("ok");
    }
    if name.ends_with(".FAILED") {
        return Some("failed");
    }
    if name.ends_with(".PENDING") {
        return Some("pending");
    }
    if name == "install_status" || name == "install_bpcells_status" {
        // Common wrapper-written sentinels seen in IVD packages.
        // Body holds "ok" / non-zero exit. Treat as a sentinel; the
        // UI inspects body to colour-code.
        return Some("status_file");
    }
    None
}

pub(crate) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/task/:task_id/status-sentinels",
        axum::routing::get(get_task_status_sentinels),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn sentinels_handler_rejects_traversal_task_id() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        // Direct check of the resolver — handler-level integration covered
        // by the existing chat_routes integration tests.
        assert!(super::super::super::_path_jail::runtime_outputs_for_task(pkg, "../etc").is_err());
    }
}
