//! Deterministic state-transition methods on [`ConversationService`].
//!
//! Architecture-improvement-plan §4.3 — these are the SME-button-and-
//! harness-triggered transitions that don't go through the LLM tool
//! loop. They share the same shape: hold an `update` lock on the
//! session, fire a `StateTrigger`, optionally append a `DecisionRecord`
//! to the audit log, and persist. Pure code movement from `mod.rs`,
//! no behavior change.
//!
//! Methods land here in two pairs (plain + `_with_rationale`) plus the
//! two harness-driven entry points (`block_from_harness`,
//! `inject_infra_error`) and the lineage fork (`branch_session`).

use super::{ConversationService, ServiceError};
use crate::session::{Session, SessionId, SessionState, StateTrigger};
use std::path::PathBuf;
use std::sync::Arc;

/// Return shape for [`ConversationService::amend_stage_method_from_rest`].
/// Includes the prior method prose so callers can surface an Undo toast
/// without a second round-trip to read the session state back.
#[derive(Debug, Default, Clone)]
pub struct AmendResult {
    /// Task ids invalidated by this method amendment.
    pub invalidated_tasks: Vec<String>,
    /// Empty string when the stage carried no prior method (first-time
    /// authoring, not an amendment).
    pub prior_method_prose: String,
}

/// Return shape for [`ConversationService::try_auto_emit_after_confirm`].
///
/// `Some(outcome)` indicates the deterministic-gate `emit_package`
/// pipeline ran successfully without an LLM turn; the caller can wire
/// the cross-version-diff broadcast + git-commit hook off
/// `emitted_package_path`. `None` indicates a deliberate skip — the
/// legacy LLM-driven `emit_package` path remains available on the
/// session's next turn.
#[derive(Debug, Clone)]
pub struct AutoEmitOutcome {
    /// Absolute path of the freshly-emitted package root.
    pub emitted_package_path: PathBuf,
}

impl ConversationService {
    /// Server-side counterpart
    /// to the LLM-callable `branch_session` tool: looks up the parent
    /// session, forks via `Session::branch_from`, and persists the
    /// new branched session. Returns the new session id so the chat
    /// pane can route the SME there.
    #[tracing::instrument(skip(self), fields(parent_id = %parent_id))]
    pub async fn branch_session(
        &self,
        parent_id: SessionId,
        careful_mode: bool,
    ) -> Result<SessionId, ServiceError> {
        self.branch_session_with_rationale(parent_id, careful_mode, None)
            .await
    }

    /// Variant of [`Self::branch_session`] that records an SME rationale
    /// on the parent session's decision log. Called by the server
    /// endpoint when the `BranchFromHereCard` includes a note.
    ///
    /// R1.4 — atomic: holds the parent's per-session lock from the
    /// initial read through the child save and the parent decision-log
    /// append. The previous shape released the parent's lock between
    /// `get` and `update`, letting a concurrent confirm/amend/turn on
    /// the parent drift its state so the forked child carried a stale
    /// snapshot.
    ///
    /// The child save lands inside the parent's `transaction`. That's
    /// safe — `SessionStore::save` writes bytes for a different
    /// session_id (the child) and inserts a new handle keyed on the
    /// child id; it does NOT acquire the parent's mutex, so there is
    /// no re-entrancy on the per-session lock that `transaction`
    /// holds.
    pub async fn branch_session_with_rationale(
        &self,
        parent_id: SessionId,
        careful_mode: bool,
        rationale: Option<String>,
    ) -> Result<SessionId, ServiceError> {
        self.branch_session_with_rationale_and_task(parent_id, careful_mode, rationale, None)
            .await
    }

    /// Full-featured branch variant (M1.3): accepts an optional `task_id`
    /// that pins the branch to a specific DAG boundary.
    ///
    /// When `task_id` is `Some`, the child's DAG is snapshotted via
    /// `Session::branch_from_at_task` which resets the named task to
    /// `Ready` and all transitive successors to `Pending`, leaving
    /// predecessor `Completed` tasks intact. The child lineage record
    /// carries `branched_from_task_id`.
    ///
    /// When `task_id` is `None`, delegates to the existing session-scoped
    /// behaviour (`Session::branch_from`), preserving M1.1 compatibility.
    pub async fn branch_session_with_rationale_and_task(
        &self,
        parent_id: SessionId,
        careful_mode: bool,
        rationale: Option<String>,
        task_id: Option<String>,
    ) -> Result<SessionId, ServiceError> {
        let store = self.store_handle();
        // Carry the child_id out of the transaction closure for the
        // return value. `Arc<Mutex<Option<SessionId>>>` mirrors the
        // pattern in `try_auto_emit_after_confirm` — std::sync::Mutex
        // is fine here because the closure is sync-bounded within the
        // async transaction body and we never await while holding it.
        let child_id_cell: Arc<std::sync::Mutex<Option<SessionId>>> =
            Arc::new(std::sync::Mutex::new(None));
        let child_id_writer = child_id_cell.clone();
        let store_clone = store.clone();
        store
            .transaction(parent_id, move |parent: &mut Session| {
                let rationale = rationale.clone();
                let task_id = task_id.clone();
                let store_inner = store_clone.clone();
                let child_id_writer = child_id_writer.clone();
                Box::pin(async move {
                    // Fork from the parent under the held lock so the
                    // child's `branch_from_at_task` sees the freshest
                    // intake + classification state.
                    let mut child = Session::branch_from_at_task(parent, careful_mode, task_id);
                    let should_emit_child_package =
                        parent.emitted_package_path.is_some() && child.workflow_dag.is_some();
                    if should_emit_child_package {
                        child.state = SessionState::ReadyToEmit;
                        if child.pending_emission_id.is_none() {
                            let summary_hash = child.current_summary_hash();
                            child.pending_emission_id = Some(uuid::Uuid::new_v5(
                                &uuid::Uuid::NAMESPACE_OID,
                                summary_hash.as_bytes(),
                            ));
                        }
                        let _ = child.mint_confirmation_token(
                            chrono::Utc::now(),
                            crate::audit_actor::AuditActor::System,
                        );
                    }
                    let child_id = child.id;
                    // Persist the child within the transaction. Safe
                    // because `save` is keyed on the child id, not the
                    // parent id whose mutex we hold.
                    store_inner.save(&child).await?;
                    // Record the fork on the parent's decision log
                    // before the transaction's tail-write persists the
                    // parent atomically.
                    parent.record_decision(
                        ecaa_workflow_core::decision_log::DecisionType::Branch {
                            child_session_id: child_id.to_string(),
                        },
                        ecaa_workflow_core::decision_log::DecisionActor::Sme,
                        rationale,
                    );
                    let mut guard = child_id_writer.lock().unwrap_or_else(|p| p.into_inner());
                    *guard = Some(child_id);
                    Ok(())
                })
            })
            .await
            .map_err(|e| {
                // `store.transaction` fails with anyhow!("no session '{}'", id)
                // when the parent isn't loadable. Translate to the typed
                // `SessionNotFound` so the server handler returns 404
                // rather than 500 (regression caught by
                // `branch_endpoint_unknown_parent_is_404`).
                let s = e.to_string();
                if s.starts_with("no session '") {
                    ServiceError::SessionNotFound
                } else {
                    ServiceError::Internal(s)
                }
            })?;
        let child_id = child_id_cell
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .ok_or_else(|| {
                ServiceError::Internal("branch transaction completed without child id".into())
            })?;
        Ok(child_id)
    }

    /// User clicked Confirm — flip user_confirmed and advance state.
    #[tracing::instrument(skip(self), fields(session_id = %id))]
    pub async fn confirm(&self, id: SessionId) -> Result<(), ServiceError> {
        self.confirm_with_rationale(id, None).await
    }

    /// Variant of [`Self::confirm`] that records an SME rationale on
    /// the decision log.
    pub async fn confirm_with_rationale(
        &self,
        id: SessionId,
        rationale: Option<String>,
    ) -> Result<(), ServiceError> {
        self.confirm_with_mode(id, rationale, None).await
    }

    /// Confirm with optional `SessionMode` + `CheckpointMode` overrides
    /// carried from the `ConfirmationTurnCard` dropdowns. Both are set
    /// on the first confirm (if supplied) and then locked by
    /// `mode_locked` for the remainder of the session. Confirmatory +
    /// fast is rejected — a confirmatory session cannot auto-advance
    /// prespecified stages.
    pub async fn confirm_with_mode(
        &self,
        id: SessionId,
        rationale: Option<String>,
        mode: Option<ecaa_workflow_core::session_mode::SessionMode>,
    ) -> Result<(), ServiceError> {
        self.confirm_with_modes(id, rationale, mode, None).await
    }

    /// Confirm a pending emission with optional mode and checkpoint overrides.
    pub async fn confirm_with_modes(
        &self,
        id: SessionId,
        rationale: Option<String>,
        mode: Option<ecaa_workflow_core::session_mode::SessionMode>,
        checkpoint_mode: Option<ecaa_workflow_core::checkpoint_mode::CheckpointMode>,
    ) -> Result<(), ServiceError> {
        self.store_handle()
            .update(id, |s| {
                // ClinicalTrial sessions must supply an explicit
                // SessionMode on the first confirm. Without it the
                // confirmation is ambiguous — the audit log would carry
                // "confirmed" with no discipline claim. Bio +
                // TimeSeriesForecast default Exploratory.
                let is_first_confirm = !s.mode_locked;
                if is_first_confirm
                    && s.project_class
                        == ecaa_workflow_core::project_class::ProjectClass::ClinicalTrial
                    && mode.is_none()
                {
                    return Err(anyhow::anyhow!(
                        "precondition_failure: ClinicalTrial sessions must pick \
                         an Analysis discipline (Exploratory or Confirmatory) \
                         before confirming. Re-send /confirm with a non-null `mode` field."
                    ));
                }
                s.try_transition(StateTrigger::UserClickedConfirm)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                // Mint a per-emit token bound to the current
                // `pending_emission_id` and the current plan summary
                // hash. `emit_package`'s precondition verifies the
                // token authorizes the pending emission, so an
                // amendment between confirm and emit forces a
                // re-confirm.
                //
                // pending_emission_id is set when the session
                // transitions INTO PendingConfirmation; if it's
                // somehow missing here, mint as a no-op (returns
                // None) and let `is_confirmed()` keep returning false.
                // The state machine should make this branch
                // unreachable; we don't fail hard so a legacy
                // PendingConfirmation session without the new field
                // can still confirm (the next emit will refuse via
                // the precondition gate).
                //
                // Audit actor: today the inner `update` closure
                // doesn't carry the principal across the boundary;
                // we stamp `System` as a transitional placeholder. A
                // follow-up will thread the actor through
                // `confirm_with_modes` so the token captures the
                // exact owner-user (or share viewer / harness agent)
                // that clicked.
                if s.pending_emission_id.is_none() {
                    // Legacy fallback: PendingConfirmation reached
                    // without going through propose_summary_confirmation.
                    // Derive from the canonical summary hash (UUIDv5
                    // over NAMESPACE_OID) to preserve the §G2 invariant
                    // even on the legacy path — `Uuid::new_v4()` would
                    // mint a random id and break "identical confirmed
                    // plan ⇒ identical emission id" for any session that
                    // bypasses propose_summary_confirmation.
                    let summary_hash = s.current_summary_hash();
                    let derived =
                        uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, summary_hash.as_bytes());
                    s.pending_emission_id = Some(derived);
                }
                let _ = s.mint_confirmation_token(
                    chrono::Utc::now(),
                    crate::audit_actor::AuditActor::System,
                );
                if let Some(m) = mode {
                    if !s.mode_locked {
                        s.mode = m;
                    }
                }
                if let Some(cp) = checkpoint_mode {
                    if !s.mode_locked {
                        s.checkpoint_mode = cp;
                    }
                }
                // Confirmatory + fast is forbidden. defer
                // to the canonical helper so any future surface that
                // mutates checkpoint_mode after confirm enforces the
                // same invariant via one predicate.
                if let Err(reason) = s
                    .checkpoint_mode
                    .ensure_compatible_with_confirmatory(s.mode.is_confirmatory())
                {
                    return Err(anyhow::anyhow!("precondition_failure: {}", reason));
                }
                s.mode_locked = true;
                // Attach the most-recently-raised
                // confirmation card's `summary_hash` so the audit
                // record fingerprints the exact text the SME saw at
                // click time. Walk the conversation tail backwards
                // for the most recent assistant turn that carries a
                // confirmation card. Returns `None` for legacy paths
                // (no card on any turn) so older replayable test
                // sessions keep their existing shape.
                let summary_hash = s
                    .conversation
                    .iter()
                    .rev()
                    .find_map(|t| t.confirmation_card.as_ref().map(|c| c.summary_hash.clone()))
                    .filter(|h| !h.is_empty());
                s.record_decision(
                    ecaa_workflow_core::decision_log::DecisionType::Confirm { summary_hash },
                    ecaa_workflow_core::decision_log::DecisionActor::Sme,
                    rationale,
                );
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        Ok(())
    }

    /// Deterministic auto-fire of `emit_package` after a successful
    /// `/confirm` POST. RCA F9: previously the SME's click flipped
    /// `user_confirmed = true` and the LLM had to call `emit_package`
    /// on its next turn — leaving a model free to litigate scope
    /// (and burn input tokens) over a gate the server had already
    /// opened. This entry point bypasses the LLM round-trip when
    /// every deterministic precondition is satisfied.
    ///
    /// Returns:
    /// - `Ok(Some(outcome))` — the package was just emitted; the
    ///   caller wires the cross-version-diff broadcast + git-commit
    ///   hook off `emitted_package_path`.
    /// - `Ok(None)` — auto-emit was deliberately skipped (session
    ///   not in `ReadyToEmit`, pending proposals awaiting SME
    ///   signoff, confirmation token missing/stale, or
    ///   `emit_package` itself returned a precondition failure that
    ///   the LLM path should handle on a subsequent turn).
    /// - `Err(_)` — store I/O or session-lookup failure.
    ///
    /// The legacy LLM-driven `emit_package` tool stays intact: the
    /// dispatcher still wires `emit_package` for amendments and any
    /// scenario where auto-emit chooses to skip. This is a fast-path
    /// front-runner, not a replacement.
    pub async fn try_auto_emit_after_confirm(
        &self,
        id: SessionId,
    ) -> Result<Option<AutoEmitOutcome>, ServiceError> {
        // The dispatcher consults its own `state_trigger` +
        // `post_handler` machinery, so we use `store.transaction()`
        // (not `update`) to hold the per-session lock across the
        // async `dispatch_one` call. Concurrent /confirm posts or
        // /turn calls serialise through the same lock — no races.
        let store = self.store_handle();
        let config_dir = self.config_dir().clone();
        let metrics_store = self.metrics_store().clone();
        let outcome_cell: Arc<std::sync::Mutex<Option<AutoEmitOutcome>>> =
            Arc::new(std::sync::Mutex::new(None));
        let outcome_writer = outcome_cell.clone();

        store
            .transaction(id, move |session: &mut Session| {
                let config_dir = config_dir.clone();
                let metrics_store = metrics_store.clone();
                let outcome_writer = outcome_writer.clone();
                Box::pin(async move {
                    // Gate 1: state must be ReadyToEmit. /confirm
                    // advances PendingConfirmation → ReadyToEmit in
                    // the same lock-holding update; any other state
                    // (Amending, Emitted, Blocked, etc.) means the
                    // SME-click semantics don't apply here.
                    if !matches!(session.state, SessionState::ReadyToEmit) {
                        tracing::debug!(
                            session_id = %session.id,
                            state = ?session.state,
                            "auto_emit_skip: session not in ReadyToEmit"
                        );
                        return Ok(());
                    }
                    // Gate 2: deterministic confirmation gate. Mirrors
                    // the `emit_package` handler's own precondition
                    // (token + pending_emission_id + summary-hash
                    // freshness). If the click race lost the latch
                    // (e.g. a subsequent amendment cleared the
                    // token), let the LLM path surface the
                    // re-confirmation requirement on the next turn.
                    if !session.is_confirmed() {
                        tracing::debug!(
                            session_id = %session.id,
                            "auto_emit_skip: confirmation token missing or stale"
                        );
                        return Ok(());
                    }
                    // Gate 3: pending atom proposals. The
                    // `emit_package` handler also refuses on this
                    // condition, but the explicit skip here keeps
                    // the LLM-driven path responsible for surfacing
                    // the "approve every proposal first" message
                    // through `propose_summary_confirmation`. We
                    // never auto-emit while any proposal lifecycle
                    // is in an SME-pending state.
                    let any_pending = session
                        .proposals
                        .values()
                        .any(|p| p.lifecycle.is_pending_sme());
                    if any_pending {
                        tracing::debug!(
                            session_id = %session.id,
                            count = session
                                .proposals
                                .values()
                                .filter(|p| p.lifecycle.is_pending_sme())
                                .count(),
                            "auto_emit_skip: proposals awaiting SME signoff"
                        );
                        return Ok(());
                    }
                    // All gates pass — fire the dispatcher with the
                    // same `ToolContext` shape the LLM tool loop
                    // assembles. `dispatch_one` runs the
                    // pre-handler `EmitPackageStart` trigger
                    // (ReadyToEmit → Emitting), then the handler
                    // body, then the post-handler `EmitPackageOk` /
                    // `EmitPackageErr` trigger (Emitting → Emitted
                    // or back to ReadyToEmit) and consumes the
                    // confirmation token. Inheriting that machinery
                    // — instead of reimplementing it — keeps
                    // server-fired and LLM-fired emits on a single
                    // audited code path.
                    //
                    // CRITICAL: do NOT call `.with_store(...)` here.
                    // `emit_package` uses a wired-in store to re-read
                    // the session under its own `store.get()` lock so
                    // a long-lived LLM tool loop sees the freshest
                    // `user_confirmed` / proposals state. In *this*
                    // path we're already inside `store.transaction()`
                    // holding the per-session `tokio::sync::Mutex`,
                    // and the Mutex is NOT re-entrant — a nested
                    // `store.get()` from the handler would deadlock
                    // the request until the upstream 300s timeout
                    // fires and stranded the session in `emitting`.
                    // The auto-emit caller has already verified
                    // `is_confirmed()` and the no-pending-proposal
                    // gate above, so dropping the fresh-read is safe.
                    let ctx = crate::tools::ToolContext::new(config_dir, "server-auto-emit")
                        .with_metrics(metrics_store);
                    let tool =
                        crate::tools::Tool::HighImpact(crate::tools::HighImpactTool::EmitPackage {
                            output_dir: None,
                        });
                    let res = crate::tools::dispatch_one(&tool, session, &ctx).await;
                    if res.is_error {
                        tracing::warn!(
                            session_id = %session.id,
                            content = %res.content,
                            "auto_emit: emit_package returned error; \
                             leaving session in ReadyToEmit for the LLM path \
                             to surface on the next turn"
                        );
                        return Ok(());
                    }
                    // Success: stash the emitted path so the outer
                    // caller can fan out the cross-version-diff
                    // broadcast and trigger the git emit hook,
                    // mirroring the post-emit logic in
                    // `chat_routes::turns::send_turn`.
                    if let Some(pkg) = session.emitted_package_path.clone() {
                        let mut guard = outcome_writer.lock().unwrap_or_else(|p| p.into_inner());
                        *guard = Some(AutoEmitOutcome {
                            emitted_package_path: pkg,
                        });
                    }
                    session.last_activity = chrono::Utc::now();
                    Ok(())
                })
            })
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        let out = outcome_cell
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        Ok(out)
    }

    /// User clicked Make corrections.
    #[tracing::instrument(skip(self), fields(session_id = %id))]
    pub async fn reject(&self, id: SessionId) -> Result<(), ServiceError> {
        self.reject_with_rationale(id, None).await
    }

    /// Reject the pending confirmation, optionally recording a rationale.
    pub async fn reject_with_rationale(
        &self,
        id: SessionId,
        rationale: Option<String>,
    ) -> Result<(), ServiceError> {
        self.store_handle()
            .update(id, |s| {
                s.try_transition(StateTrigger::UserClickedReject)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                // A reject unlocks the mode so the SME can pick a
                // different discipline on the next Confirmation card.
                s.mode_locked = false;
                // Clear the confirmation token on every reject so a
                // subsequent emit_package tool call from the LLM cannot
                // ride on the previous confirm's authorization. The
                // confirmation gate must be re-armed by a fresh SME
                // click before any new emit can succeed.
                //
                // pending_emission_id is also cleared so the next
                // PendingConfirmation cycle mints a fresh UUID — the
                // rejected emission_id must never be re-bindable.
                s.clear_confirmation();
                s.pending_emission_id = None;
                s.record_decision(
                    ecaa_workflow_core::decision_log::DecisionType::Reject,
                    ecaa_workflow_core::decision_log::DecisionActor::Sme,
                    rationale,
                );
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        Ok(())
    }

    #[tracing::instrument(skip(self), fields(session_id = %id))]
    /// Unblock the session (SME clicked the Unblock button).
    pub async fn unblock(&self, id: SessionId) -> Result<(), ServiceError> {
        self.unblock_with_rationale(id, None).await
    }

    /// Unblock the session, optionally recording a rationale.
    pub async fn unblock_with_rationale(
        &self,
        id: SessionId,
        rationale: Option<String>,
    ) -> Result<(), ServiceError> {
        self.store_handle()
            .update(id, |s| {
                s.try_transition(StateTrigger::OperatorUnblock)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                s.record_decision(
                    ecaa_workflow_core::decision_log::DecisionType::Unblock,
                    ecaa_workflow_core::decision_log::DecisionActor::Sme,
                    rationale,
                );
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        // Mark the most-recent unrecovered blocker as
        // recovered. Best-effort: a metrics IO failure never fails the
        // unblock. Called after the store update so we're not in
        // the sync closure.
        self.metrics_store()
            .record_blocker_recovered(id, Some("unblock".to_string()))
            .await;
        Ok(())
    }

    /// invoked by the server's /progress handler when the
    /// harness reports `kind=task_blocked`. Transitions Emitted → Blocked
    /// with a typed BlockerKind so the UI's BlockerCard renders the right
    /// recovery affordance (DataShape → rerun w/ fix, Validation → amend,
    /// Host/Agent → retry). Silently no-ops for sessions that aren't in
    /// Emitted (e.g. already Blocked from a prior event).
    pub async fn block_from_harness(
        &self,
        id: SessionId,
        task_id: String,
        detail: String,
        blocker_kind: ecaa_workflow_core::blocker::BlockerKind,
    ) -> Result<(), ServiceError> {
        // Capture the blocker-kind variant name before the
        // sync closure consumes `blocker_kind`. Debug repr of struct
        // Variants is "VariantName { field:... }"; unit variants are
        // just "VariantName". Strip at the first space or '{' to get
        // only the variant name, matching the eval-adapters expectation.
        let blocker_kind_str = {
            let raw = format!("{:?}", blocker_kind);
            raw.split_once([' ', '{'])
                .map(|(v, _)| v.trim().to_string())
                .unwrap_or(raw)
        };
        let mut did_block = false;
        self.store_handle()
            .update(id, |s| {
                // Accept every execution-side state the state machine
                // accepts for HarnessTaskBlocked. Intake/IntakeFollowup
                // remain tolerated for legacy post-unblock sessions by
                // moving them through Emitted before applying the typed
                // transition.
                let accept = matches!(
                    s.state,
                    crate::session::SessionState::Emitted
                        | crate::session::SessionState::ReadyToEmit
                        | crate::session::SessionState::Amending { .. }
                        | crate::session::SessionState::Blocked { .. }
                        | crate::session::SessionState::Intake
                        | crate::session::SessionState::IntakeFollowup
                );
                if !accept {
                    eprintln!(
                        "[block_from_harness] skipping — session state is {:?} (not an execution-side state) for task {}",
                        s.state, task_id
                    );
                    return Ok(());
                }
                eprintln!(
                    "[block_from_harness] transitioning {:?} → Blocked for task {}",
                    s.state, task_id
                );
                // For Intake / IntakeFollowup we need to move through
                // Emitted first so the existing (Emitted, HarnessTaskBlocked)
                // transition fires. Simplest path: synthesize an intermediate
                // Emitted state directly; the emitted_package_path is already
                // set on the session (harness wouldn't be posting otherwise).
                if matches!(
                    s.state,
                    crate::session::SessionState::Intake
                        | crate::session::SessionState::IntakeFollowup
                ) {
                    s.state = crate::session::SessionState::Emitted;
                }
                s.try_transition(StateTrigger::HarnessTaskBlocked {
                    task_id,
                    detail,
                    blocker_kind,
                })
                .map_err(|e| anyhow::anyhow!("{}", e))?;
                did_block = true;
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        // Record the blocker event. Best-effort.
        if did_block {
            self.metrics_store()
                .record_blocker_entered(id, blocker_kind_str)
                .await;
        }
        Ok(())
    }

    /// Public REST wrapper over the LLM-callable `amend_stage_method`
    /// tool body. Holds the session update lock, replays the same
    /// preconditions (Emitted state, stage known, method non-empty,
    /// confirmatory-prespecified ⇒ rationale required), records the
    /// AmendStage + PostHocDeviation decisions, and leaves the session
    /// in `ReadyToEmit` so the server's /start-execution or auto-relaunch
    /// hook can push the amended package back through the harness.
    ///
    /// Returns the list of invalidated task ids on success so the caller
    /// can drive confirmation UX + post a task_reset event to the
    /// artifact cache.
    pub async fn amend_stage_method_from_rest(
        &self,
        id: SessionId,
        stage: String,
        method_prose: String,
        rationale: Option<String>,
    ) -> Result<AmendResult, ServiceError> {
        let config_dir = self.config_dir().clone();
        let result_cell: Arc<std::sync::Mutex<AmendResult>> =
            Arc::new(std::sync::Mutex::new(AmendResult::default()));
        let result_writer = result_cell.clone();
        self.store_handle()
            .update(id, move |s| {
                let result = crate::tools::amend::amend_stage_method(
                    s,
                    &stage,
                    &method_prose,
                    rationale.as_deref(),
                    &config_dir,
                );
                if result.is_error {
                    // The ToolError serde tagging produces
                    // `{error_kind, reason, hint,...}`. Surface reason
                    // + hint to the caller so the REST client shows a
                    // useful message ("the `method_prose` value was
                    // missing or blank — Pass the SME's replacement…")
                    // instead of a generic rejection string.
                    let reason = result
                        .content
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("amend_stage_method rejected");
                    let hint = result
                        .content
                        .get("hint")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let msg = if hint.is_empty() {
                        reason.to_string()
                    } else {
                        format!("{} — {}", reason, hint)
                    };
                    return Err(anyhow::anyhow!(msg));
                }
                let tids: Vec<String> = result
                    .content
                    .get("invalidated_tasks")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let prior = result
                    .content
                    .get("prior_method_prose")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // PoisonError::into_inner recovery: AmendResult is a
                // pure value type with no cross-field invariants — the
                // tids+prior writes are atomic w.r.t. any reader, and a
                // panicking inner closure would have already aborted the
                // store::update. So a poisoned lock here is recoverable;
                //.unwrap() would surface a higher-level panic the
                // caller can't act on.
                let mut guard = result_writer.lock().unwrap_or_else(|p| p.into_inner());
                *guard = AmendResult {
                    invalidated_tasks: tids,
                    prior_method_prose: prior,
                };
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        let out = result_cell
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        // Record an amendment. Best-effort: metrics
        // failure never fails the amend operation.
        self.metrics_store().record_amendment(id).await;
        Ok(out)
    }

    /// Public REST wrapper for the Undo Amendment toast. Same shape
    /// as `amend_stage_method_from_rest` but records a
    /// `DecisionType::UndoneAmendment` after the amend lands, so the
    /// audit log reflects the round-trip. `reverted_to` is the prose
    /// the session is being returned to.
    pub async fn undo_amendment_from_rest(
        &self,
        id: SessionId,
        stage: String,
        reverted_prose: String,
    ) -> Result<AmendResult, ServiceError> {
        let reverted_copy = reverted_prose.clone();
        let res = self
            .amend_stage_method_from_rest(id, stage.clone(), reverted_prose, None)
            .await?;
        self.store_handle()
            .update(id, move |s| {
                s.record_decision(
                    ecaa_workflow_core::decision_log::DecisionType::UndoneAmendment {
                        stage: stage.clone(),
                        reverted_to: reverted_copy.clone(),
                    },
                    ecaa_workflow_core::decision_log::DecisionActor::Sme,
                    None,
                );
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        Ok(res)
    }

    /// Public REST wrapper over `rerun_task`. Same preconditions as
    /// `amend_stage_method_from_rest` plus: stage must have a recorded
    /// method in `intake_methods` (no method ⇒ rerun is meaningless —
    /// caller should amend instead).
    pub async fn rerun_task_from_rest(
        &self,
        id: SessionId,
        task_id: String,
        reason: Option<String>,
    ) -> Result<Vec<String>, ServiceError> {
        let config_dir = self.config_dir().clone();
        let invalidated_cell: Arc<std::sync::Mutex<Vec<String>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let invalidated_writer = invalidated_cell.clone();
        self.store_handle()
            .update(id, move |s| {
                let result = crate::tools::execution::rerun_task(
                    s,
                    &task_id,
                    reason.as_deref(),
                    &config_dir,
                );
                if result.is_error {
                    let reason = result
                        .content
                        .get("reason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("rerun_task rejected");
                    let hint = result
                        .content
                        .get("hint")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let msg = if hint.is_empty() {
                        reason.to_string()
                    } else {
                        format!("{} — {}", reason, hint)
                    };
                    return Err(anyhow::anyhow!(msg));
                }
                let tids: Vec<String> = result
                    .content
                    .get("invalidated_tasks")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                let mut guard = invalidated_writer.lock().unwrap_or_else(|p| p.into_inner());
                *guard = tids;
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        let out = invalidated_cell
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        Ok(out)
    }

    /// Atom-safety-policy widen the session-scoped
    /// `runtime_packages` override for an atom. Called from the
    /// `POST /api/chat/session/:id/atom/:atom_id/add-runtime-package`
    /// endpoint when the SME clicks the BlockerCard's
    /// `ProvisioningDenied` "Add `<package>` to atom.runtime_packages"
    /// affordance.
    ///
    /// Effect on the session:
    /// 1. Inserts `(atom_id, registry, package)` into
    ///    `Session::atom_runtime_overrides`. Idempotent — adding the
    ///    same triple twice does not duplicate. The widened set is
    ///    `BTreeSet<String>` per registry so the in-memory + serialized
    ///    representations stay byte-deterministic.
    /// 2. Records one `DecisionType::RuntimePackageAdded` entry per
    ///    call (even on idempotent re-add, so the SME's clickstream is
    ///    fully captured in the audit trail).
    /// 3. When the session has an `emitted_package_path`, in-place
    ///    updates `policies/runtime-prereqs.json` so the harness
    ///    install-proxy sees the widened set on retry without waiting
    ///    for a full package re-emission. Soft-fails: an IO error logs
    ///    to stderr but does not roll back the session-side mutation
    ///    (the in-memory + decision-log state remain consistent; the
    ///    on-disk file will heal on the next full emit).
    ///
    /// Validation:
    /// - `package` must be non-empty after trim — `Internal`-wrapped
    ///   error so the REST handler can surface as 400.
    /// - `registry` must be one of `apt`, `dnf`, `pip` / `pypi`, `cran`
    ///   / `r`, `conda`. Unknown registries return an `Internal` error
    ///   listing the allowed set.
    /// - Empty `atom_id` is permitted (the BlockerCard payload can
    ///   carry `"<unknown>"` from a malformed agent reason-prefix); the
    ///   override is still recorded so the SME can manually widen the
    ///   catalog and the harness's next dispatch will look up by exact
    ///   match.
    ///
    /// Returns `()` on success. Mirrors the shape of `unblock` —
    /// the REST handler reads side-effect state via
    /// `get_session_state` rather than the return value.
    pub async fn add_runtime_package_from_rest(
        &self,
        id: SessionId,
        atom_id: String,
        package: String,
        registry: String,
    ) -> Result<(), ServiceError> {
        let package_trimmed = package.trim().to_string();
        if package_trimmed.is_empty() {
            return Err(ServiceError::Internal(
                "package is required and cannot be empty".to_string(),
            ));
        }
        let registry_trimmed = registry.trim().to_string();
        if registry_trimmed.is_empty() {
            return Err(ServiceError::Internal(
                "registry is required and cannot be empty".to_string(),
            ));
        }
        // Normalize known package-manager aliases so duplicate clicks
        // through different registry spellings (`pip` vs `pypi`, `cran`
        // vs `r`) collapse on the same `BTreeSet` key.
        let registry_normalized = normalize_registry(&registry_trimmed);
        let allowed = ["apt", "dnf", "pip", "cran", "conda"];
        if !allowed.contains(&registry_normalized.as_str()) {
            return Err(ServiceError::Internal(format!(
                "unknown registry `{}` — allowed: apt, dnf, pip (pypi), cran (r), conda",
                registry_trimmed
            )));
        }
        let atom_id_owned = atom_id.clone();
        let pkg_for_closure = package_trimmed.clone();
        let registry_for_closure = registry_normalized.clone();
        self.store_handle()
            .update(id, move |s| {
                // Insert into the session-scoped override map.
                // Idempotent BTreeSet semantics absorb the duplicate
                // click case; the decision record still fires below.
                s.atom_runtime_overrides
                    .entry(atom_id_owned.clone())
                    .or_default()
                    .entry(registry_for_closure.clone())
                    .or_default()
                    .insert(pkg_for_closure.clone());
                // Mirror the override into the emitted package's
                // `policies/runtime-prereqs.json` so the harness
                // install-proxy reads the widened set on retry
                // without waiting for a full re-emission. Soft-fails:
                // an IO failure logs to stderr but does not roll back
                // the in-memory override (the in-memory state is the
                // source of truth; the file mirrors it).
                if let Some(pkg) = &s.emitted_package_path {
                    let manifest_path = pkg.join("policies").join("runtime-prereqs.json");
                    if let Err(e) = patch_runtime_prereqs_file(
                        &manifest_path,
                        &registry_for_closure,
                        &pkg_for_closure,
                    ) {
                        eprintln!(
                            "[add_runtime_package] patch failed for {}: {}",
                            manifest_path.display(),
                            e
                        );
                    }
                }
                s.record_decision(
                    ecaa_workflow_core::decision_log::DecisionType::RuntimePackageAdded {
                        atom_id: atom_id_owned.clone().into(),
                        package: pkg_for_closure.clone(),
                        registry: registry_for_closure.clone(),
                    },
                    ecaa_workflow_core::decision_log::DecisionActor::Sme,
                    None,
                );
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        Ok(())
    }

    /// System-driven InfraError trigger — fires StateTrigger::InfraError
    /// from any state into Blocked. Used by:
    ///
    /// - fixtures 17/18 to inject a backend failure for testing
    /// - Future server-side admin endpoint that may need to put a
    ///   misbehaving session on hold
    ///
    /// The InfraError trigger is unique in that it's allowed from every
    /// state (per the state machine table), so the only error path is a
    /// missing session or a persistence failure.
    pub async fn inject_infra_error(
        &self,
        id: SessionId,
        reason: String,
    ) -> Result<(), ServiceError> {
        self.store_handle()
            .update(id, |s| {
                s.try_transition(StateTrigger::InfraError {
                    reason: reason.clone(),
                })
                .map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        // Record an InfraError-driven blocker event.
        // Best-effort: metrics failure never rolls back the transition.
        self.metrics_store()
            .record_blocker_entered(id, "HostError".to_string())
            .await;
        Ok(())
    }

    /// Re-run `rebuild_dag` after
    /// proposal signoff so the freshly-spliced promoted node gains
    /// correct proof-carrying edges immediately, without waiting for
    /// the next user turn.
    ///
    /// The problem: `rebuild_dag` (via `try_build_via_composer`) replaces
    /// `session.workflow_dag` entirely from the composer's output. Any
    /// node spliced into `workflow_dag` by the signoff handler is
    /// **destroyed** the next time a mutation tool fires `rebuild_dag`.
    /// The root fix lands in `tools::rebuild_dag` itself (it now
    /// re-injects `Promoted` nodes from `session.proposals` after the
    /// composer replaces `workflow_dag`). This caller ensures the SME
    /// sees the freshly-edged DAG right after signoff rather than on the
    /// next turn.
    ///
    /// Soft-fail contract mirrors `set_active_policy_bundle`:
    /// - If the session is not found, returns `SessionNotFound`.
    /// - If `rebuild_dag` fails (no taxonomy yet, composer gap, etc.) the
    ///   error is logged but **not propagated** — the signoff is already
    ///   persisted; the promoted node re-injection will happen on the
    ///   next turn's rebuild instead.
    pub async fn rebuild_dag_after_signoff(&self, id: SessionId) -> Result<(), ServiceError> {
        let store = self.store_handle();
        store.get(id).await.ok_or(ServiceError::SessionNotFound)?;
        let config_dir = self.config_dir().clone();

        store
            .update(id, |session| {
                // Only attempt rebuild when the session has enough context for
                // the composer. Without taxonomy or classification the composer
                // returns an error immediately; we skip rather than surface a
                // confusing `PreconditionFailure` to the signoff caller.
                if session.classification.is_some() && session.taxonomy.is_some() {
                    if let Err(e) = crate::tools::rebuild_dag(session, &config_dir) {
                        tracing::warn!(
                            session_id = %id,
                            error = %e,
                            "rebuild_dag_after_signoff: rebuild failed; \
                             promoted node re-injection deferred to next user turn"
                        );
                        // Persist the unchanged session snapshot through update's
                        // normal atomic path so this call remains serialized with
                        // concurrent progress writes.
                    }
                }
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(format!("save after rebuild failed: {e}")))?;
        // If classification/taxonomy are absent, the session is pre-emit
        // and `workflow_dag` is None anyway; the splice from signoff is
        // the only dag content, and the next rebuild will re-inject it
        // via the promoted-node pass added to `rebuild_dag`.
        Ok(())
    }
}

/// Atom-safety-policy collapse known package-manager aliases
/// onto the canonical key used by `RuntimePrereqs`. The wire payload
/// can carry either spelling; the on-disk manifest only knows the
/// canonical form (`pip` / `cran`). Unknown registries are returned
/// unchanged so the caller's allowlist check rejects them.
fn normalize_registry(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    match lower.as_str() {
        "pypi" => "pip".to_string(),
        "r" => "cran".to_string(),
        _ => lower,
    }
}

/// Atom-safety-policy in-place patch of the emitted
/// package's `policies/runtime-prereqs.json` so the harness install-
/// proxy reads the widened package set on retry.
///
/// Reads the existing manifest (creates an empty one when absent),
/// inserts the package into the matching collection (apt/dnf →
/// system_packages; pip/cran/conda → language_packages), and writes
/// back atomically via the `.tmp` + rename pattern used by every
/// other policy writer. Idempotent on the package set (BTreeSet).
///
/// Returns `Ok(())` on success; surfaces IO + parse errors so the
/// caller can log them. Caller treats the write as best-effort
/// because the in-memory session override is authoritative — the
/// file will be regenerated on the next full re-emit either way.
fn patch_runtime_prereqs_file(
    manifest_path: &std::path::Path,
    registry: &str,
    package: &str,
) -> anyhow::Result<()> {
    use ecaa_workflow_core::runtime_prereqs::RuntimePrereqs;
    let mut manifest: RuntimePrereqs = if manifest_path.exists() {
        let bytes = std::fs::read(manifest_path)?;
        serde_json::from_slice(&bytes)?
    } else {
        RuntimePrereqs::new()
    };
    match registry {
        "apt" => {
            manifest.system_packages.apt.insert(package.to_string());
        }
        "dnf" => {
            manifest.system_packages.dnf.insert(package.to_string());
        }
        "pip" => {
            manifest
                .language_packages
                .python
                .insert(package.to_string());
        }
        "cran" => {
            manifest.language_packages.r.insert(package.to_string());
        }
        "conda" => {
            manifest.language_packages.conda.insert(package.to_string());
        }
        // The caller's allowlist runs first, so this branch is
        // unreachable in normal operation; log + skip rather than
        // panic so a future allowlist change can't crash the patch.
        other => {
            eprintln!(
                "[add_runtime_package] unsupported registry `{}` reached patch helper; skipping",
                other
            );
            return Ok(());
        }
    }
    let parent = manifest_path.parent().ok_or_else(|| {
        anyhow::anyhow!("manifest path has no parent: {}", manifest_path.display())
    })?;
    std::fs::create_dir_all(parent)?;
    let json = serde_json::to_string_pretty(&manifest)?;
    //.tmp + fsync + atomic rename so a crash mid-write doesn't leave
    // a corrupted manifest the harness pre-flight would refuse to parse.
    ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(manifest_path, json.as_bytes())?;
    Ok(())
}
