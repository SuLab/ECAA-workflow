//! `amend_stage_method` tool body.
//!
//! Post-emission method swap for a previously-emitted stage. Routes
//! `Emitted → Amending → ReadyToEmit` via the state machine so the LLM
//! can call `emit_package` in the next turn to write the lineage-linked
//! amendment. `rerun_task` (in `execution.rs`) delegates here with the
//! same method_prose to preserve the entire amend pathway.

use super::invalidate_and_rebuild;
use crate::errors::{ToolError, ToolResult};
use crate::session::{Session, StateTrigger};
use std::path::Path;

/// Post-emission, swap the method for a
/// previously-emitted stage. Preconditions: session is in `Emitted`,
/// the stage exists in the DAG, and the new method prose is non-empty.
/// Effect: mutate intake_methods, invalidate the forward slice of the
/// stage, rebuild the DAG, and route Emitted → Amending → ReadyToEmit.
/// The conversation service / server will re-emit a
/// lineage-linked amendment package on the next emit_package call.
pub(crate) fn amend_stage_method(
    session: &mut Session,
    stage: &str,
    method_prose: &str,
    rationale: Option<&str>,
    config_dir: &Path,
) -> ToolResult {
    use crate::session::SessionState;

    if method_prose.trim().is_empty() {
        return ToolResult::err(ToolError::empty_string(
            "method_prose",
            "Pass the SME's replacement method description.",
        ));
    }
    if !matches!(session.state, SessionState::Emitted) {
        return ToolResult::err(ToolError::wrong_state(
            "Emitted",
            format!("{:?}", session.state),
        ));
    }
    // Confirmatory + prespecified stage ⇒ rationale required.
    // `PostHocDeviation` decision record writes below.
    let is_prespecified = session.mode.is_prespecified(stage);
    let rationale_trimmed = rationale.map(|r| r.trim().to_string()).unwrap_or_default();
    if is_prespecified && rationale_trimmed.is_empty() {
        return ToolResult::err(ToolError::RationaleRequired {
            stage: stage.to_string(),
            hint: "This stage is prespecified in a confirmatory session. \
                   Pass a non-empty rationale explaining the deviation; \
                   it will be recorded as a PostHocDeviation in decisions.jsonl."
                .into(),
        });
    }
    // Validate the stage exists in the DAG; if not, return suggestions.
    let stage_known = session
        .dag
        .as_ref()
        .map(|d| d.tasks.contains_key(stage))
        .unwrap_or(false);
    if !stage_known {
        let candidates: Vec<String> = session
            .dag
            .as_ref()
            .map(|d| d.tasks.keys().take(8).map(|id| id.to_string()).collect())
            .unwrap_or_default();
        return ToolResult::err(ToolError::unknown_stage(
            stage,
            candidates,
            "Use a stage id returned by get_session_state.",
        ));
    }

    // Record the new method in intake_methods so rebuild_dag picks it up.
    let prose_owned = method_prose.trim().to_string();
    let prior_method = session
        .intake_methods
        .0
        .get(stage)
        .map(|r| r.method.clone())
        .unwrap_or_default();
    session
        .intake_methods
        .set(stage, Some(prose_owned.clone()), None);

    // `invalidate_and_rebuild` collapses the downstream-slice
    // invalidation + DAG rebuild shared with select_sensitivity_winner.
    let invalidated = match invalidate_and_rebuild(session, stage, config_dir) {
        Ok(list) => list,
        Err(e) => return ToolResult::err(e),
    };
    // Capture the parent emit path before the next
    // emit overwrites `session.emitted_package_path` so the
    // RO-Crate `prov:wasDerivedFrom` and `UpdateAction` entities can
    // point at the parent. `parent_package_path` is `None`-safe at
    // emit time: missing parent ⇒ no lineage written, plain emit.
    if let Some(parent_path) = session.emitted_package_path.clone() {
        session.pending_amendment = Some(crate::session::PendingAmendment {
            target_stage: stage.to_string(),
            invalidated_tasks: invalidated.clone(),
            parent_package_path: parent_path,
            rationale: rationale
                .map(|r| r.trim())
                .filter(|r| !r.is_empty())
                .map(str::to_string),
        });
    }
    // Defer the Emitted → Amending → ReadyToEmit
    // transition pair to the dispatcher's post-handler hook (see
    // `tools/mod.rs::drain_deferred_state_triggers_post_ok`). The
    // dispatcher drains `session.deferred_state_triggers` and fires
    // each in order; this is the centralized auditability surface
    // that replaces handler-side `try_transition` calls.
    session
        .deferred_state_triggers
        .push(StateTrigger::AmendStart {
            target_stage: stage.to_string(),
            invalidated_tasks: invalidated.clone(),
        });
    session
        .deferred_state_triggers
        .push(StateTrigger::AmendReady);

    // An amendment changes the emit shape (different stage method,
    // different downstream task slice). Even though the SME confirmed
    // the prior plan, that confirmation no longer authorizes the
    // amended package; the next emit_package call must require a
    // fresh `/confirm` click. Clear both the token and the pending
    // emission id so the AmendReady → ReadyToEmit transition flows
    // through propose_summary_confirmation (which will mint a fresh
    // pending_emission_id when the new card is raised). Also note
    // the summary hash would already have drifted (intake_methods
    // mutated above), so `is_confirmed()` would have returned false
    // even without this explicit clear; the clear keeps the on-disk
    // state explicit instead of relying on the hash-drift check.
    session.clear_confirmation();
    session.pending_emission_id = None;

    let _ = config_dir; // placeholder for future policy reloads

    session.record_decision(
        ecaa_workflow_core::decision_log::DecisionType::AmendStage {
            stage: stage.to_string(),
            method_prose: prose_owned.clone(),
        },
        ecaa_workflow_core::decision_log::DecisionActor::Llm,
        None,
    );

    // In addition to the `AmendStage` record, a
    // confirmatory+prespecified amendment writes a `PostHocDeviation`
    // record that claim demotion reads off of.
    if is_prespecified {
        session.record_decision(
            ecaa_workflow_core::decision_log::DecisionType::PostHocDeviation {
                target_stage: stage.to_string(),
                prior_method: prior_method.clone(),
                new_method: prose_owned.clone(),
                reason: rationale_trimmed.clone(),
            },
            ecaa_workflow_core::decision_log::DecisionActor::Sme,
            Some(rationale_trimmed.clone()),
        );
    }

    // Amendment cost surfacing. The SME UI already has
    // a richer projection via `POST /impact-preview` (per-task cost
    // ranges with stage-class medians); the ToolResult cost summary
    // is a lightweight count-only preview the LLM uses in its
    // post-amend turn copy ("Amending will rerun N tasks. Run
    // impact-preview for cost projection."). The dedicated UI route
    // remains the source of truth for $X / Yh ranges since that
    // endpoint already reads MetricsStore which the conversation
    // crate doesn't have direct access to from this tool body.
    let cost_summary = if invalidated.is_empty() {
        "No downstream tasks invalidated — amendment is a no-op.".to_string()
    } else if invalidated.len() == 1 {
        "Amending will rerun 1 task. Run impact-preview for cost projection.".to_string()
    } else {
        format!(
            "Amending will rerun {} tasks. Run impact-preview for cost projection.",
            invalidated.len()
        )
    };

    ToolResult::ok(serde_json::json!({
        "stage": stage,
        "invalidated_tasks": invalidated,
        "method_prose": session
            .intake_methods
            .0
            .get(stage)
            .map(|r| r.method.clone())
            .unwrap_or_default(),
        // Include the prior prose so the REST wrapper can thread it
        // back to the UI's Undo toast. Empty string when the stage
        // carried no prior method (initial authoring, not an
        // amendment).
        "prior_method_prose": prior_method,
        "cost_preview": cost_summary,
        "next_step": "Call emit_package in the next turn to write the amendment.",
    }))
}
