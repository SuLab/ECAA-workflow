//! execution-kickoff + rerun tools.
//!
//! `start_execution_tool` fires the `ServiceEventSink::execution_requested`
//! so the server's harness-spawning layer picks up the request.
//! `rerun_task` is the thin wrapper on `amend_stage_method` that
//! preserves the existing method prose — semantically "rerun this
//! completed task from scratch with the same method". Lives here
//! because the SME triggers it through the same "I want more runs"
//! affordance that drives StartExecution.

use super::{amend::amend_stage_method, ToolContext};
use crate::errors::{ToolError, ToolResult};
use crate::session::Session;
use ecaa_workflow_core::ids::TaskId;
use std::path::Path;

/// Matches DEFAULT_MAX_ITERATIONS in
/// crates/server/src/chat_routes/execution/start.rs (single source of truth).
const DEFAULT_EXECUTION_MAX_ITERATIONS: u32 = 20;

pub(super) fn start_execution_tool(
    session: &mut Session,
    max_iterations: Option<u32>,
    ctx: &ToolContext,
) -> ToolResult {
    use crate::session::SessionState;
    if !matches!(session.state, SessionState::Emitted) {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: format!(
                "start_execution requires an emitted package; session is in {:?}",
                session.state
            ),
            hint: "Call emit_package first, or wait for the emit to complete.".into(),
        });
    }
    let Some(pkg) = session.emitted_package_path.clone() else {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "session has no emitted_package_path despite Emitted state".into(),
            hint: "Re-emit the package.".into(),
        });
    };
    // Fire-and-forget sink notification. The server's impl looks up the
    // session's emitted_package_path and spawns the harness. The tool
    // has no way to confirm the spawn succeeded synchronously, which is
    // fine — the UI polls /execution for status.
    //
    // Security audit C-3: the LLM has no way to supply
    // `agent_path` — the field was dropped from the tool schema. The
    // sink receives `None`, and the server-side allowlist resolves it
    // to the production default.
    if let (Some(sink), Some(sid)) = (&ctx.event_sink, ctx.session_id) {
        sink.execution_requested(sid, None, max_iterations);
    }
    ToolResult::ok(serde_json::json!({
        "status": "requested",
        "package_dir": pkg.to_string_lossy(),
        "agent_path": "scripts/agent-claude.sh",
        "max_iterations": max_iterations.unwrap_or(DEFAULT_EXECUTION_MAX_ITERATIONS),
        "note": "Execution starts in the background. The Jobs tab will stream progress as tasks complete. The SME can unblock/amend via the same chat surface.",
    }))
}

/// Thin wrapper on `amend_stage_method`
/// that preserves the existing method_prose — semantically "rerun
/// this completed task from scratch with the same method". The SME
/// invokes this when the task's inputs drifted (upstream rerun) or
/// the result didn't meet expectations and they want a fresh run
/// rather than a method swap.
///
/// Preconditions:
/// - session is in Emitted (same as amend_stage_method)
/// - task_id must exist in the DAG
/// - the task's stage must have a recorded method in intake_methods
///   (otherwise "rerun with the same method" is meaningless; the
///   LLM should call amend_stage_method with a new method instead)
pub(crate) fn rerun_task(
    session: &mut Session,
    task_id: &str,
    reason: Option<&str>,
    config_dir: &Path,
) -> ToolResult {
    // Resolve the current method for this stage. The task id and the
    // stage id are usually the same except for per-sample expansions,
    // where the task id is "<stage>_<sample>" — strip the sample
    // suffix if there's a known stage prefix.
    let current_method = session
        .intake_methods
        .0
        .get(task_id)
        .map(|r| r.method.clone());
    let resolved_method = match current_method {
        Some(m) if !m.trim().is_empty() => m,
        _ => {
            let candidates: Vec<String> =
                session.intake_methods.0.keys().take(8).cloned().collect();
            return ToolResult::err(ToolError::PreconditionFailure {
                reason: format!(
                    "no recorded method for stage '{}' — rerun_task only works on stages with a prior method",
                    task_id
                ),
                hint: format!(
                    "Use amend_stage_method with a fresh method, or pick a stage with a recorded method: {:?}",
                    candidates
                ),
            });
        }
    };

    // Delegate to amend_stage_method with the same method_prose.
    // Preserves the entire amend pathway: state transitions, lineage,
    // alone-in-turn discipline. The rerun reason rides along in the
    // response but isn't baked into intake_methods (it's a run-level
    // concern, not a method-level one).
    //
    // amend_stage_method records an `AmendStage` decision on success;
    // rerun_task additionally records the caller-level `RerunTask` so
    // the audit log distinguishes a genuine method swap from a rerun.
    // Pass `reason` through as the rationale so `amend_stage_method`
    // can enforce the confirmatory-prespec gate.
    let mut res = amend_stage_method(session, task_id, &resolved_method, reason, config_dir);
    if !res.is_error {
        session.record_decision(
            ecaa_workflow_core::decision_log::DecisionType::RerunTask {
                task_id: TaskId::from(task_id),
                reason: reason.map(|s| s.to_string()),
            },
            ecaa_workflow_core::decision_log::DecisionActor::Llm,
            reason.map(|s| s.to_string()),
        );
        if let Some(obj) = res.content.as_object_mut() {
            obj.insert("rerun".to_string(), serde_json::Value::Bool(true));
            if let Some(r) = reason {
                obj.insert(
                    "rerun_reason".to_string(),
                    serde_json::Value::String(r.to_string()),
                );
            }
        }
    }
    res
}

#[cfg(test)]
mod tests {
    //! `start_execution_tool` agent_path regression.
    //!
    //! Security audit C-3: the tool no longer accepts an
    //! agent_path argument — the field was dropped from the `Tool`
    //! enum and JSON schema, so the LLM has no way to direct the
    //! harness at an arbitrary script. The dispatcher fires the sink
    //! with `agent_path = None`, and the server's allowlist resolves
    //! it to `scripts/agent-claude.sh`. Tests here only cover the
    //! reduced surface.
    use super::*;
    use crate::session::{Session, SessionState};
    use crate::tools::ToolContext;
    use std::path::PathBuf;

    fn config_dir() -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config")
    }

    fn emitted_session() -> Session {
        let mut s = Session::new(false);
        s.state = SessionState::Emitted;
        s.emitted_package_path = Some(PathBuf::from("/tmp/pkg-stub"));
        s
    }

    #[test]
    fn agent_path_echo_is_pinned_to_default() {
        // C-3: the LLM can no longer choose; the response always echoes
        // the production default so the SME can audit the trail.
        let mut s = emitted_session();
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        let res = start_execution_tool(&mut s, None, &ctx);
        assert!(!res.is_error, "tool returned an error: {:?}", res);
        assert_eq!(res.content["agent_path"], "scripts/agent-claude.sh");
    }

    #[test]
    fn max_iterations_default_is_twenty() {
        let mut s = emitted_session();
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        let res = start_execution_tool(&mut s, None, &ctx);
        assert_eq!(res.content["max_iterations"], 20);
    }

    #[test]
    fn explicit_max_iterations_is_echoed() {
        let mut s = emitted_session();
        let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
        let res = start_execution_tool(&mut s, Some(7), &ctx);
        assert_eq!(res.content["max_iterations"], 7);
    }
}
