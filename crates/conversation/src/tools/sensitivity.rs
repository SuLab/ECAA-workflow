//! `select_sensitivity_winner` tool body.
//!
//! SME's sensitivity-comparison pick. Transitions the session out of
//! `Blocked { AwaitingSmeSelection }` and re-routes through the amend
//! pathway so the DAG downstream of `stage` rebuilds with the chosen
//! winner as the canonical method.

use super::invalidate_and_rebuild;
use crate::errors::{ToolError, ToolResult};
use crate::session::{Session, StateTrigger};

/// The SME's sensitivity-comparison
/// choice. Preconditions: session is in `Blocked { kind:
/// AwaitingSmeSelection }`, the `stage` matches the blocker's
/// stage_id, and `winner` is a member of the blocker's candidates.
/// Effect: records the winner in `intake_methods`, invalidates the
/// forward slice of `stage`, rebuilds the DAG, transitions
/// Blocked → Intake → ReadyToEmit by driving OperatorUnblock
/// + amend pathway.
pub(super) fn select_sensitivity_winner(
    session: &mut Session,
    stage: &str,
    winner: &str,
    rationale: Option<&str>,
    config_dir: &std::path::Path,
) -> ToolResult {
    use ecaa_workflow_core::blocker::BlockerKind;

    // Precondition: session is Blocked with AwaitingSmeSelection, or
    // the resolved_blocker() helper surfaces one (for legacy sessions
    // without the structured field).
    let (expected_stage, candidates) = match session.state.resolved_blocker() {
        Some((
            BlockerKind::AwaitingSmeSelection {
                stage_id,
                candidates,
            },
            _,
        )) => (stage_id, candidates),
        _ => {
            return ToolResult::err(ToolError::PreconditionFailure {
                reason: "select_sensitivity_winner requires Blocked { kind: AwaitingSmeSelection }"
                    .into(),
                hint: "This tool is only valid when the session is awaiting SME selection.".into(),
            });
        }
    };
    if stage != expected_stage {
        return ToolResult::err(ToolError::ValidationFailure {
            reason: format!(
                "stage '{}' does not match the blocker's awaiting stage '{}'",
                stage, expected_stage
            ),
            valid_alternatives: vec![expected_stage.clone()],
            hint: "Use the stage id from the blocker card.".into(),
        });
    }
    if !candidates.iter().any(|c| c == winner) {
        return ToolResult::err(ToolError::ValidationFailure {
            reason: format!(
                "'{}' is not among the candidates for stage '{}'",
                winner, stage
            ),
            valid_alternatives: candidates.clone(),
            hint: "Pick a variant that's on the comparison card.".into(),
        });
    }

    // Record the winner as the canonical method for the stage.
    session
        .intake_methods
        .set(stage, Some(winner.to_string()), None);

    // Defer Blocked → Intake to the dispatcher's
    // post-handler hook. Distinct from `OperatorUnblock` because the
    // generic operator-unblock path preserves Emitted post-emit (so
    // subsequent harness blockers are absorbed) — that would trap the
    // sensitivity-winner flow since ProposeSummaryConfirmation can't
    // fire from Emitted. The handler validates the winner against
    // candidates while still in Blocked; the dispatcher drains
    // `deferred_state_triggers` and fires SensitivityWinnerSelected
    // after the handler returns.
    session
        .deferred_state_triggers
        .push(StateTrigger::SensitivityWinnerSelected);

    // same `invalidate_forward_slice + rebuild_dag` pair that
    // amend_stage_method uses. `invalidate_and_rebuild` encapsulates it.
    let invalidated = match invalidate_and_rebuild(session, stage, config_dir) {
        Ok(list) => list,
        Err(e) => return ToolResult::err(e),
    };
    // The AmendStart trigger expects Emitted. Since SelectSensitivityWinner
    // can fire from Intake (we just unblocked), we directly reach
    // ReadyToEmit via amend transitions once we're in Emitted. If the
    // session was never Emitted, the Intake → ReadyToEmit move below
    // Uses UserClickedConfirm semantics instead. The // Blocked { AwaitingSmeSelection } state arises pre-emission from
    // the harness, so Intake → ReadyToEmit via ProposeSummaryConfirmation
    // + UserClickedConfirm is the path the LLM will drive after this
    // tool returns.

    // Capture the candidates the SME rejected at this decision so
    // audit replay can reconstruct the counterfactual. `candidates`
    // was resolved from the `Blocked { AwaitingSmeSelection { ... } }`
    // payload BEFORE the dispatcher fires the
    // SensitivityWinnerSelected trigger above (which clears the
    // blocker and drops the list); collecting now keeps the rejected
    // set durable for the audit row.
    let rejected_candidates: Vec<String> = candidates
        .iter()
        .filter(|c| c.as_str() != winner)
        .cloned()
        .collect();
    session.record_decision(
        ecaa_workflow_core::decision_log::DecisionType::SelectSensitivityWinner {
            stage: stage.to_string(),
            winner: winner.to_string(),
            rejected_candidates,
        },
        ecaa_workflow_core::decision_log::DecisionActor::Llm,
        rationale.map(|s| s.to_string()),
    );

    ToolResult::ok(serde_json::json!({
        "stage": stage,
        "winner": winner,
        "rationale": rationale,
        "invalidated_tasks": invalidated,
        "next_step": "Propose summary and confirm, then emit_package.",
    }))
}
