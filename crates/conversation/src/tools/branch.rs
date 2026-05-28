//! `branch_session` tool body.
//!
//! The dispatch layer only validates preconditions and returns the
//! metadata the server needs to fork the session; the actual
//! `Session::branch_from` + persistence lives at the chat_routes layer
//! (branches.rs) because only the server can atomically allocate a new
//! session id.

use crate::errors::{ToolError, ToolResult};
use crate::session::Session;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Wire type for the branch endpoint's POST body (M1.3). Extends the
/// existing `CheckpointDecisionRequest.rationale` path with an optional
/// `task_id` that pins the branch to a specific DAG boundary.
///
/// Exported via ts-rs so the UI and API contract stay in sync.
#[derive(Debug, Clone, Default, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BranchInput {
    /// Optional rationale for the branch (surfaced in the decision log).
    #[serde(default)]
    #[ts(optional)]
    pub rationale: Option<String>,
    /// Optional task boundary. When set, the child session's DAG is
    /// snapshotted with this task reset to Ready and its transitive
    /// successors reset to Pending. None means a full session-scoped
    /// branch (M1.1 behaviour).
    #[serde(default)]
    #[ts(optional)]
    pub task_id: Option<String>,
}

/// Tool-dispatch entry point for task-scoped branches (M1.3). Validates
/// preconditions, then — when `task_id` is provided — verifies the task
/// exists in the parent's DAG. Returns a `ToolResult` carrying the
/// metadata the server needs to call `branch_session_with_rationale_and_task`.
///
/// This is `pub` so integration tests can call it directly. The in-crate
/// dispatch wires through `branch_session` (no task_id, M1.1) or this
/// function (with task_id).
pub fn branch_session_with_task(
    session: &Session,
    task_id: Option<&str>,
    rationale: Option<&str>,
) -> ToolResult {
    // Run the same precondition checks as branch_session.
    let base = branch_session(session, rationale);
    if base.is_error {
        return base;
    }
    // Validate the task_id against the parent's dag.
    if let Some(tid) = task_id {
        match &session.dag {
            None => {
                return ToolResult::err(ToolError::PreconditionFailure {
                    reason: format!(
                        "branch_session: task_id '{tid}' supplied but parent session has no DAG yet"
                    ),
                    hint: "Emit a package first so the DAG is present before branching at a task boundary.".into(),
                });
            }
            Some(dag) => {
                if !dag.tasks.contains_key(tid) {
                    return ToolResult::err(ToolError::PreconditionFailure {
                        reason: format!(
                            "branch_session: task_id '{tid}' not found in the parent's DAG"
                        ),
                        hint: "Pass a valid task id from the DAG or omit task_id for a session-scoped branch.".into(),
                    });
                }
            }
        }
    }
    // Re-emit the base ok payload with the added task_id field.
    let branch_index = if session.conversation.is_empty() {
        None
    } else {
        Some(session.conversation.len())
    };
    ToolResult::ok(serde_json::json!({
        "parent_session_id": session.id.to_string(),
        "branched_from_turn_index": branch_index,
        "branched_from_task_id": task_id,
        "rationale": rationale,
        "next_step": "Server allocates the branched session id and routes the SME there.",
    }))
}

/// Validates preconditions and returns the metadata the server needs to fork
/// the session. The actual fork (Session::branch_from + SessionStore
/// persistence + URL routing) lives at the chat_routes layer because it has to
/// atomically allocate a new session id.
///
/// Preconditions (enforced here and reused by `branch_session_with_task`):
///
/// - Session must be past Greeting — otherwise there is nothing to branch from.
/// - Session must not be Amending or Emitting — intermediate states whose
///   semantics don't survive a branch.
/// - Session must not be Blocked — the blocker carries unresolved recovery
///   context that cannot survive a fork. The SME must unblock first.
pub(super) fn branch_session(session: &Session, rationale: Option<&str>) -> ToolResult {
    use crate::session::SessionState;
    if matches!(session.state, SessionState::Greeting) {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "branch_session requires intake to have started — nothing to branch from yet"
                .into(),
            hint: "Append at least one intake prose turn before branching.".into(),
        });
    }
    if matches!(
        session.state,
        SessionState::Emitting | SessionState::Amending { .. }
    ) {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: format!("branch_session is not valid from {:?}", session.state),
            hint: "Wait for the in-flight emit / amend to settle before branching.".into(),
        });
    }
    // Refuse branch from Blocked. The blocker
    // carries unresolved recovery context (failed task, retry knobs,
    // rationale) and branching would either lose that context or
    // duplicate the blocker into a child the SME has no way to recover.
    if matches!(session.state, SessionState::Blocked { .. }) {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "branch_session is not valid from Blocked".into(),
            hint: "Clear the blocker (unblock the session) before branching. The blocker carries \
                   unresolved recovery context that cannot survive a fork."
                .into(),
        });
    }
    let branch_index = if session.conversation.is_empty() {
        None
    } else {
        Some(session.conversation.len())
    };
    ToolResult::ok(serde_json::json!({
        "parent_session_id": session.id.to_string(),
        "branched_from_turn_index": branch_index,
        "rationale": rationale,
        "next_step": "Server allocates the branched session id and routes the SME there.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Session, SessionState};
    use scripps_workflow_core::blocker::{BlockerEntry, BlockerKind};

    fn blocked_state() -> SessionState {
        let entry = BlockerEntry::new(
            "task-x",
            BlockerKind::AgentError {
                message: "test agent error".into(),
            },
            "test agent error",
        );
        SessionState::Blocked {
            blockers: vec![entry],
            reason: "test agent error".into(),
            recovery_hint: "fix the thing".into(),
            blocker_kind: Some(BlockerKind::AgentError {
                message: "test agent error".into(),
            }),
            context: None,
        }
    }

    #[test]
    fn branch_refuses_from_blocked() {
        // branch_session must refuse when the
        // session is in Blocked state because the blocker carries
        // unresolved recovery context that can't survive a fork.
        let mut s = Session::new(false);
        s.state = blocked_state();
        let r = branch_session(&s, Some("trying a fork"));
        assert!(r.is_error, "branch must refuse from Blocked; got {r:?}");
        let body = serde_json::to_string(&r.content).unwrap();
        assert!(
            body.contains("precondition_failure"),
            "must surface precondition_failure error_kind; got {body}"
        );
        assert!(
            body.contains("Blocked"),
            "error message must reference Blocked state; got {body}"
        );
    }

    #[test]
    fn branch_refuses_from_greeting() {
        let s = Session::new(false);
        // fresh session is Greeting
        assert!(matches!(s.state, SessionState::Greeting));
        let r = branch_session(&s, None);
        assert!(r.is_error, "branch must refuse from Greeting");
    }

    #[test]
    fn branch_accepts_from_intake() {
        let mut s = Session::new(false);
        s.state = SessionState::Intake;
        let r = branch_session(&s, Some("forking from intake"));
        assert!(!r.is_error, "branch must succeed from Intake; got {r:?}");
    }
}
