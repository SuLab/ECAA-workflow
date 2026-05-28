//! `POST /api/chat/session/:id/dashboard/summary`
//!
//! Narrative dashboard summary. Reads the `final_reporting` task's
//! narrative artifact + key
//! figures + decision log highlights, bundles them into source
//! material, and asks Haiku 4.5 to produce a 3-paragraph executive
//! summary. Side-call billed, cached until the source fingerprint
//! changes.

use super::{ChatAppState, LlmRateBuckets};
use axum::extract::Path;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use ecaa_workflow_conversation::side_calls::summary as summary_side_call;
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub(super) struct SummaryResponse {
    pub summary: String,
    pub model: String,
    pub cached: bool,
}

pub(super) async fn post_dashboard_summary(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    // dashboard summary fires a Haiku side-call. Cap at 6/min to keep
    // side_call_cost_usd bounded when a client polls the route.
    if let Err(status) = LlmRateBuckets::check(
        &app.llm_buckets.summary,
        session_id,
        app.llm_rate_limits.summary,
    ) {
        return (
            status,
            "rate limit exceeded: /dashboard/summary capped at 6/min/session",
        )
            .into_response();
    }

    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let source = build_source(&session);
    if source.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "no narrative or results available to summarise yet",
        )
            .into_response();
    }
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    let fingerprint = format!("{:016x}", hasher.finish());

    let backend = app.conversation.llm_for_scoring();
    let metrics = app.conversation.metrics();
    match summary_side_call::generate_dashboard_summary(
        backend,
        metrics,
        session_id,
        &source,
        &fingerprint,
    )
    .await
    {
        Ok(r) => Json(SummaryResponse {
            summary: r.summary,
            model: r.model,
            cached: r.cached,
        })
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("summary failed: {}", e),
        )
            .into_response(),
    }
}

pub(super) fn build_source(session: &ecaa_workflow_conversation::Session) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    // Intake prose is a decent reason-for-being orientation.
    if !session.intake_prose.is_empty() {
        let _ = write!(out, "INTAKE:\n{}\n\n", session.intake_prose);
    }
    // Decision highlights: amendments, reruns, confirmed selections.
    let relevant: Vec<_> = session
        .decisions
        .iter()
        .filter(|d| {
            matches!(
                d.decision,
                ecaa_workflow_core::decision_log::DecisionType::Confirm { .. }
                    | ecaa_workflow_core::decision_log::DecisionType::AmendStage { .. }
                    | ecaa_workflow_core::decision_log::DecisionType::RerunTask { .. }
                    | ecaa_workflow_core::decision_log::DecisionType::SelectSensitivityWinner { .. }
                    | ecaa_workflow_core::decision_log::DecisionType::UserNote { .. }
            )
        })
        .take(40)
        .collect();
    if !relevant.is_empty() {
        out.push_str("DECISION HIGHLIGHTS:\n");
        for d in &relevant {
            let _ = writeln!(
                out,
                "- {} [{}]: {:?}",
                d.timestamp.format("%Y-%m-%d %H:%M"),
                match d.actor {
                    ecaa_workflow_core::decision_log::DecisionActor::Sme => "SME",
                    ecaa_workflow_core::decision_log::DecisionActor::Llm => "assistant",
                    ecaa_workflow_core::decision_log::DecisionActor::Harness => "system",
                },
                d.decision,
            );
        }
        out.push('\n');
    }
    // Task results that completed: surface each task id + its brief
    // narrative hook (if any) so the summariser sees every stage.
    if let Some(dag) = session.current_dag() {
        out.push_str("TASK STATUS:\n");
        for (tid, task) in &dag.tasks {
            let status = match task.state {
                ecaa_workflow_core::dag::TaskState::Completed { .. } => "completed",
                ecaa_workflow_core::dag::TaskState::Running { .. } => "running",
                ecaa_workflow_core::dag::TaskState::Failed { .. } => "failed",
                ecaa_workflow_core::dag::TaskState::Blocked { .. } => "blocked",
                ecaa_workflow_core::dag::TaskState::Ready => "ready",
                ecaa_workflow_core::dag::TaskState::Pending => "pending",
            };
            let _ = writeln!(out, "- {} ({}): {}", tid, status, task.description,);
        }
    }
    out
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[("POST", "/api/chat/session/:id/dashboard/summary")];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/dashboard/summary",
        axum::routing::post(post_dashboard_summary),
    )
}
