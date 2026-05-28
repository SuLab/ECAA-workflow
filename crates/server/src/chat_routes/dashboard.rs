//! Interactive-dashboard endpoints. `dashboard_index` returns the set
//! of stages and views available for a session; individual view data
//! is streamed via the existing `/artifacts/*` route. The view data
//! files (`runtime/outputs/<stage>/view_data/*.json`) are produced by
//! `runtime.plotting.core.generate()` when the shared plotting library
//! registers VIEWS for a stage.
//!
//! The index is built by walking the session's DAG for tasks whose
//! state is Completed and scanning their `view_data` directory for
//! written JSON payloads. This keeps the dashboard strictly reactive:
//! stages only appear once a view has been written.

use super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;
use std::path::Path as StdPath;
use uuid::Uuid;

/// One completed task that has at least one view_data JSON file.
#[derive(Debug, Serialize)]
pub(super) struct DashboardStage {
    /// Task identifier matching a node in `WORKFLOW.json`.
    pub stage_id: String,
    /// Human-readable label sourced from the task's display_name.
    pub description: String,
    /// Available interactive views produced by the plotting library for this stage.
    pub views: Vec<DashboardView>,
}

/// A single named view produced by the agent's plotting library for a completed stage.
#[derive(Debug, Serialize)]
pub(super) struct DashboardView {
    /// Stable identifier for the view within its stage (e.g. `umap`, `volcano`).
    pub view_id: String,
    /// URL the browser can fetch the raw JSON from. Relative so the
    /// UI can prepend its own origin if the dev proxy rewrites it.
    pub data_url: String,
}

/// Top-level dashboard response: all stages with available views for the session.
#[derive(Debug, Serialize)]
pub(super) struct DashboardIndex {
    /// Session this index belongs to.
    pub session_id: Uuid,
    /// Stages that have at least one written view_data file; empty when none are ready.
    pub stages: Vec<DashboardStage>,
}

/// `GET /api/chat/session/:id/dashboard` — enumerate completed stages with view_data files.
pub async fn dashboard_index(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let Some(pkg) = session.emitted_package_path.clone() else {
        return Json(DashboardIndex {
            session_id,
            stages: Vec::new(),
        })
        .into_response();
    };
    let Some(dag) = session.current_dag() else {
        return Json(DashboardIndex {
            session_id,
            stages: Vec::new(),
        })
        .into_response();
    };

    use scripps_workflow_core::dag::TaskState;
    let mut stages: Vec<DashboardStage> = Vec::new();
    // Iterate DAG task order (BTreeMap — deterministic by key) and
    // include every completed task with at least one view_data file.
    for (task_id, task) in &dag.tasks {
        if !matches!(task.state, TaskState::Completed { .. }) {
            continue;
        }
        let view_dir = pkg
            .join("runtime")
            .join("outputs")
            .join(task_id.as_str())
            .join("view_data");
        let Some(views) = scan_view_data(&view_dir, session_id, task_id.as_str()) else {
            continue;
        };
        if views.is_empty() {
            continue;
        }
        stages.push(DashboardStage {
            stage_id: task_id.to_string(),
            description: task.description.clone(),
            views,
        });
    }

    Json(DashboardIndex { session_id, stages }).into_response()
}

fn scan_view_data(
    view_dir: &StdPath,
    session_id: Uuid,
    task_id: &str,
) -> Option<Vec<DashboardView>> {
    let entries = std::fs::read_dir(view_dir).ok()?;
    let mut views: Vec<DashboardView> = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        // Skip the manifest itself; it's metadata, not a view payload.
        if name == "manifest.json" {
            continue;
        }
        if !name.ends_with(".json") {
            continue;
        }
        let view_id = name.trim_end_matches(".json").to_string();
        let data_url = format!(
            "/api/chat/session/{}/artifacts/runtime/outputs/{}/view_data/{}",
            session_id, task_id, name
        );
        views.push(DashboardView { view_id, data_url });
    }
    views.sort_by(|a, b| a.view_id.cmp(&b.view_id));
    Some(views)
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[("GET", "/api/chat/session/:id/dashboard/index")];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/dashboard/index",
        axum::routing::get(dashboard_index),
    )
}
