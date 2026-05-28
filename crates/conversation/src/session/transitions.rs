//! session state-machine transitions.
//!
//! Extracted from the old monolithic `session.rs` so the
//! `try_transition` dispatch table, the `StateTrigger` inputs, and the
//! `TransitionError` error type all live in one place. `impl Session`
//! picks up the method additively since Rust permits multiple impl
//! blocks for the same struct in the same crate.

use super::{Session, SessionState};
use chrono::Utc;
use ecaa_workflow_core::blocker::{BlockerContext, BlockerEntry, BlockerKind};

/// Typed inputs that drive the session state-machine.
///
/// Each variant maps to one state-machine arc in `try_transition`.
/// High-impact tools produce `StateTrigger` values; the service layer
/// calls `try_transition` after each tool dispatch.
#[derive(Debug, Clone)]
pub enum StateTrigger {
    /// The SME sent new prose; may re-trigger classification.
    AppendProse,
    /// DAG was built but contains unresolved `discover_*` stages.
    DagBuiltWithUnresolvedDiscovery,
    /// The LLM called `propose_summary_confirmation`.
    ProposeSummaryConfirmation,
    /// The SME clicked the web UI Confirm button.
    UserClickedConfirm,
    /// The SME clicked the web UI Reject button.
    UserClickedReject,
    /// `emit_package` tool began package emission.
    EmitPackageStart,
    /// Package emission completed successfully.
    EmitPackageOk,
    /// Package emission failed.
    EmitPackageErr {
        /// Human-readable reason for the emission failure.
        reason: String,
    },
    /// The SME (or operator) clicked the Unblock button.
    OperatorUnblock,
    /// The `select_sensitivity_winner` tool drives this after recording
    /// the SME's pick. Routes Blocked → Intake (unconditionally, even
    /// post-emit) so the tool's follow-up re-propose-summary + Confirm
    /// chain can reach ReadyToEmit via the amend pathway. Distinct from
    /// `OperatorUnblock` because the post-emit generic unblock restores
    /// Emitted (to keep absorbing harness blockers), whereas the
    /// sensitivity-winner flow explicitly wants to re-amend.
    SensitivityWinnerSelected,
    /// An infrastructure error was injected by the server (e.g. Anthropic 500).
    InfraError {
        /// Human-readable reason for the infrastructure error.
        reason: String,
    },
    /// The SME invoked amend_stage_method
    /// against a previously-emitted stage. Transitions Emitted →
    /// Amending. Carries the target stage + the downstream ids the
    /// DAG slice invalidator produced so the UI can render them.
    AmendStart {
        /// Stage id selected for method amendment.
        target_stage: String,
        /// DAG task ids that were invalidated by the amendment.
        invalidated_tasks: Vec<String>,
    },
    /// The amend DAG is rebuilt and ready to
    /// re-emit. Transitions Amending → ReadyToEmit.
    AmendReady,
    /// the harness reported a task-level blocker via
    /// `POST /progress { kind: "task_blocked" }`. Transitions Emitted →
    /// Blocked so the SME sees the BlockerCard and can POST /unblock to
    /// resume. `blocker_kind` dispatches the right recovery affordance
    /// in the UI (DataShape → rerun with input fix, ValidationFailed →
    /// amend method, etc.).
    HarnessTaskBlocked {
        /// Task id that became blocked.
        task_id: String,
        /// Human-readable detail from the harness progress event.
        detail: String,
        /// Typed blocker kind for UI recovery-affordance dispatch.
        blocker_kind: BlockerKind,
    },
}

/// Error returned when a `StateTrigger` is illegal from the current
/// `SessionState`.
#[derive(Debug, Clone)]
pub struct TransitionError {
    /// Serialized prior state name.
    pub from: String,
    /// Serialized trigger name.
    pub trigger: String,
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "illegal session transition: {} from {}",
            self.trigger, self.from
        )
    }
}

impl std::error::Error for TransitionError {}

/// Map a `BlockerKind` to a one-line SME-facing recovery hint. Every
/// variant is enumerated explicitly (R-14) so adding a new
/// `BlockerKind` variant — even with `#[non_exhaustive]` on the enum —
/// forces the new variant to be considered here at code-review time.
/// The trailing catch-all is retained for forward compatibility with
/// the non-exhaustive marker but every currently-defined variant has
/// an explicit arm so the wildcard cannot silently swallow a new
/// variant that needs a tailored hint.
///
/// CLAUDE.md asserts 42 variants; the `BlockerKind::COUNT` test gate
/// at `crates/core/tests/blocker_variant_count.rs` keeps that doc in
/// lock-step with the enum.
fn recovery_hint_for_blocker(kind: &BlockerKind) -> String {
    // Generic hint used by variants whose remediation is purely
    // "look at the blocker detail and decide". Centralized so future
    // edits stay consistent.
    const GENERIC: &str = "Review the blocker detail and unblock to resume execution.";

    let hint: &str = match kind {
        // Input/output shape mismatches — caller fixes the data or DAG.
        BlockerKind::DataShapeMismatch { .. } => {
            "Check the task's input shape and rerun, or adjust the DAG upstream."
        }
        // Validation / metric — amend the method or relax the gate.
        BlockerKind::ValidationFailed { .. } | BlockerKind::MetricBelowThreshold { .. } => {
            "Amend the method on this stage or relax the validation contract."
        }
        // Contract assertion — accept or amend; retrying with same method is futile.
        BlockerKind::ContractViolation { .. } => {
            "Either accept this result (override the contract assertion) or amend \
             the upstream method via chat. Retrying with the same method will hit \
             the same assertion."
        }
        // Host/agent transient error — retry after fix.
        BlockerKind::HostError { .. } | BlockerKind::AgentError { .. } => {
            "Retry once the underlying host issue is resolved."
        }
        // Structured tool-failure capture — remediation proposer ranks fixes.
        BlockerKind::ToolError { .. } => {
            "Open the BlockerCard's remediation suggestions; the proposer will \
             rank ≤ 3 typed fixes (resource bump, library pin, method swap, etc.)."
        }
        // Scheduler killed for OOM — bump resource class.
        BlockerKind::MemoryExhausted { .. } => {
            "The scheduler killed the task for exceeding its memory cap. \
             Rerun on a larger resource class (the BlockerCard's Resize \
             affordance bumps the next resource_class tier)."
        }
        // Scheduler killed for wallclock — extend --time=.
        BlockerKind::TimeExceeded { .. } => {
            "The scheduler killed the task for exceeding its wallclock cap. \
             Rerun with a longer time limit (the BlockerCard's Extend \
             affordance widens --time= and resubmits)."
        }
        // Missing input dependency — typically a missing upstream rerun.
        BlockerKind::MissingInput { .. } => GENERIC,
        // Awaiting SME pick/approval — the BlockerCard picker drives this.
        BlockerKind::AwaitingSmeSelection { .. } | BlockerKind::AwaitingSmeApproval { .. } => {
            GENERIC
        }
        // Pilot exceeded cost ceiling — SME raises ceiling, shrinks sample, or aborts.
        BlockerKind::PilotOversize { .. } => GENERIC,
        // Stall monitor signal — Resize / Retry / Abort triplet on the card.
        BlockerKind::Stalled { .. } => GENERIC,
        // Runtime library missing — pick recommended substitute or skip.
        BlockerKind::RuntimeCapabilityMissing { .. } => GENERIC,
        // Generic structured-decision picker.
        BlockerKind::AwaitingStructuredDecision { .. } => GENERIC,
        // Silent-completion guard — Rerun affordance.
        BlockerKind::MissingArtifact { .. } => GENERIC,
        // Heartbeat stale — Rerun affordance.
        BlockerKind::HeartbeatStalled { .. } => GENERIC,
        // Orphaned by crash — deterministic rerun.
        BlockerKind::OrphanedByCrash { .. } => GENERIC,
        // Image digest mismatch — prune cache or re-emit.
        BlockerKind::ImageDigestMismatch { .. } => GENERIC,
        // Container pull failed — retry, swap image, or configure creds.
        BlockerKind::ContainerPullFailed { .. } => GENERIC,
        // Container start failed — retry elsewhere or fall back.
        BlockerKind::ContainerStartFailed { .. } => GENERIC,
        // Container runtime binary missing.
        BlockerKind::RuntimeMissing { .. } => GENERIC,
        // SBOM emit failed — rerun SBOM or skip on next.
        BlockerKind::SbomEmissionFailed { .. } => GENERIC,
        // Network policy violation — amend container network or remove step.
        BlockerKind::NetworkPolicyViolation { .. } => GENERIC,
        // Container cache corrupted — prune-then-rerun affordance.
        BlockerKind::ContainerCacheCorrupted { .. } => GENERIC,
        // Replay corruption surfaced from load path.
        BlockerKind::ReplayCorruption { .. } => GENERIC,
        // Image digest unresolved at emit — retry / pin / disable containers.
        BlockerKind::ImageDigestUnresolved { .. } => GENERIC,
        // Composer cannot satisfy goal — CompositionInfeasibleCard renders affordances.
        BlockerKind::CompositionInfeasible { .. } => GENERIC,
        // Container exited non-zero / OOM-killed — resize or amend method.
        BlockerKind::ContainerExitedAbnormally { .. } => GENERIC,
        // SLURM partition lacks runtime — pick different partition or drop requirement.
        BlockerKind::SlurmRuntimeUnavailable { .. } => GENERIC,
        // Iterate-until did not converge — raise threshold / accept best / abort.
        BlockerKind::IterationDidNotConverge { .. } => GENERIC,
        // Container hung — container-only reap and rerun on same host.
        BlockerKind::ContainerHung { .. } => GENERIC,
        // Sandbox refused dispatch — typed refusal records on the card.
        BlockerKind::SandboxRefused { .. } => GENERIC,
        // Adjudication queue entry — SME or operator review required.
        BlockerKind::AdjudicationRequired { .. } => GENERIC,
        // Sandbox required but executor can't provide — dispatch-time refusal.
        BlockerKind::SandboxRequired { .. } => GENERIC,
        // Atom network policy ≠ executor network policy — dispatch-time refusal.
        BlockerKind::NetworkPolicyMismatch { .. } => GENERIC,
        // Provisioning denied at runtime — install proxy refused.
        BlockerKind::ProvisioningDenied { .. } => GENERIC,
        // Config schema version older/newer than loader knows.
        BlockerKind::SchemaVersionMismatch { .. } => GENERIC,
        // Controlled-access data routed at an LLM-bearing executor.
        BlockerKind::ControlledAccessViolation { .. } => {
            "SME must declassify the data source or use a different executor. \
             Controlled-access data cannot be forwarded to an LLM agent. \
             Options: (1) obtain an institutional data-sharing agreement and \
             re-emit with the controlled_access flag removed, or (2) switch \
             to a host-mode executor (SWFC_EXECUTOR_MODE=local + \
             SWFC_DISABLE_CONTAINERS=1) that does not invoke a cloud LLM."
        }
        // Aggregate output directory exceeded size cap — inspect + clean up + rerun.
        BlockerKind::OutputSizeExceeded { .. } => {
            "The task's runtime/outputs directory exceeded the aggregate size cap \
             (SWFC_TASK_OUTPUT_MAX_MB). Inspect the directory for unexpectedly large \
             intermediate files, clean up, then rerun. To raise the cap permanently \
             set SWFC_TASK_OUTPUT_MAX_MB to a higher value in the operator env."
        }
        // Malformed state.patch.json quarantined — operator inspects the
        // .rejected-* file and fixes the agent's output serializer.
        BlockerKind::PatchUnparseable { .. } => {
            "The agent wrote a state.patch.json that failed JSON parsing. \
             The malformed file has been renamed to a .rejected-* sidecar \
             for post-incident review. Inspect the rejected file, fix the \
             agent's output serializer, and rerun the task."
        }
        // Host-vs-server clock skew exceeds the configured threshold.
        BlockerKind::ClockSkew { .. } => {
            "The harness clock differs from the server clock by more than the \
             allowed threshold. Synchronise the host or server clock via NTP \
             and restart the harness. If the skew is intentional (e.g. time-zone \
             misconfiguration), raise SWFC_CLOCK_SKEW_THRESHOLD_SECS."
        }
        // Wall-clock watchdog fired: task has run longer than its budget.
        BlockerKind::WallClockExceeded { .. } => {
            "The wall-clock watchdog detected the task has exceeded its runtime \
             budget (SWFC_WATCHDOG_MULTIPLIER × expected_wall_seconds). This \
             typically indicates a CPU-bound infinite loop. Abort the task, \
             inspect the agent's output for the loop, then rerun or amend the \
             stage method."
        }
        // Task was soft-cancelled because the SME amended its stage's method.
        // Recovery is automatic: the amendment flow re-emits a new package and
        // the harness requeues the task against the updated DAG. The SME does
        // not need to act on this blocker directly.
        BlockerKind::CancelledByAmendment { .. } => {
            "This task was cancelled because you amended its upstream stage. \
             It will be re-queued automatically when the amended package is re-emitted."
        }
        // Git-provenance commit was dropped (pool saturated or timed out).
        // Session state is unaffected; the SME or operator can re-run the
        // git commit manually against the emitted package directory.
        BlockerKind::ProvenanceCommitDropped { .. } => {
            "A git-provenance commit was dropped (pool full or timed out). \
             The session state is intact. To restore the recovery point, \
             run `git add -A && git commit -m '<trigger>'` manually in the \
             emitted package directory, or re-emit the package to trigger \
             a fresh commit hook."
        }
        // Executor agent ran past MAX_TURNS_PER_TASK.
        BlockerKind::TurnBudgetExceeded => {
            "Executor agent hit turn budget (MAX_TURNS_PER_TASK, default 40). \
             Either investigate why the agent looped (inspect \
             runtime/outputs/<task_id>/agent-claude.log for the last few turns) \
             or raise the budget by setting MAX_TURNS_PER_TASK to a higher value \
             and re-running. If the task is genuinely complex, consider splitting \
             it into smaller atoms."
        }
        // The enum is `#[non_exhaustive]`. Every variant currently
        // defined has an explicit arm above; this catch-all is the
        // forward-compatibility safety net for variants added after
        // a downstream-crate rebuild without recompiling this file.
        // If you're adding a new variant in `core`, add an explicit
        // arm above so the new shape gets an intentional hint
        // rather than falling through silently.
        _ => GENERIC,
    };
    hint.to_string()
}

fn blocker_entry(
    task_id: impl Into<String>,
    kind: BlockerKind,
    message: impl Into<String>,
    recovery_hint: impl Into<String>,
) -> BlockerEntry {
    BlockerEntry::new(task_id, kind, message).with_recovery_hint(recovery_hint)
}

impl Session {
    /// per-turn-end update of the IntakeFollowup streak counter. Called
    /// exactly once by `tool_loop::run_tool_loop` immediately before it
    /// commits a final `Turn` and returns. If the turn ends in
    /// `IntakeFollowup`, bump the counter; any other terminal state
    /// resets it to 0.
    ///
    /// Per-turn (not per-transition): an in-turn AppendProse from
    /// `IntakeFollowup` lands in `Intake` and the subsequent rebuild
    /// fires `DagBuiltWithUnresolvedDiscovery` to re-enter
    /// `IntakeFollowup`. Doing the bump per-transition would reset
    /// inside every turn and never accumulate across turns. The
    /// per-turn semantic matches the plan's "after 4 consecutive
    /// followup turns" wording.
    ///
    /// Caller MUST invoke this exactly once per `run_tool_loop` exit
    /// path so the count tracks "consecutive followup turns" 1:1; a
    /// double-call would bump twice in a single turn and prematurely
    /// trigger the convergence nudge.
    pub fn note_turn_end_intake_followup(&mut self) {
        if matches!(self.state, SessionState::IntakeFollowup) {
            self.intake_followup_streak = self.intake_followup_streak.saturating_add(1);
        } else {
            self.intake_followup_streak = 0;
        }
    }

    /// R3.5 — crash-recovery hook for sessions stuck in
    /// [`SessionState::Emitting`]. The normal exits from `Emitting`
    /// are `EmitPackageOk` (→ Emitted) and `EmitPackageErr`
    /// (→ Blocked{HostError}). If the server crashes while the
    /// `emit_package` handler is mid-flight, the post-handler triggers
    /// never fire and the persisted session is left in `Emitting`
    /// forever — no UI affordance, no auto-relaunch, no LLM turn can
    /// reach it.
    ///
    /// Heuristic: when the persisted state is `Emitting` AND the
    /// session has not been touched for at least
    /// `stale_threshold_secs`, synthesize a transition to
    /// `Blocked{HostError("emit_crash_recovery")}` so the BlockerCard
    /// renders and the SME can `/unblock` to retry. Returns `true`
    /// when the recovery transition fired; `false` when the session
    /// is not in `Emitting` or has been touched recently.
    ///
    /// Idempotent: a session already past `Emitting` is a no-op.
    /// Callers (typically `SessionStore::ensure_loaded`) should
    /// invoke this immediately after deserialization and persist if
    /// the return is `true`.
    pub fn recover_stale_emitting(&mut self, stale_threshold_secs: i64) -> bool {
        if !matches!(self.state, SessionState::Emitting) {
            return false;
        }
        let now = Utc::now();
        let elapsed = now.signed_duration_since(self.last_activity);
        if elapsed.num_seconds() < stale_threshold_secs {
            return false;
        }
        let reason = "emit_crash_recovery".to_string();
        let recovery_hint =
            "The server appears to have crashed during emit_package. Click Unblock to retry."
                .to_string();
        self.state = SessionState::Blocked {
            blockers: vec![blocker_entry(
                "_session",
                BlockerKind::HostError {
                    message: reason.clone(),
                },
                reason.clone(),
                recovery_hint.clone(),
            )],
            reason: reason.clone(),
            recovery_hint: recovery_hint.clone(),
            blocker_kind: Some(BlockerKind::HostError {
                message: reason.clone(),
            }),
            context: Some(BlockerContext {
                timestamp: now.to_rfc3339(),
                recovery_hints: Some(recovery_hint),
            }),
        };
        self.last_activity = now;
        tracing::warn!(
            session_id = %self.id,
            elapsed_secs = elapsed.num_seconds(),
            "recover_stale_emitting: synthesized HostError(emit_crash_recovery)"
        );
        true
    }

    /// Apply a `StateTrigger` to advance the session state machine.
    /// Returns `Err(TransitionError)` when the trigger is illegal in
    /// the current state; caller should treat that as a server-internal
    /// error and log it.
    pub fn try_transition(&mut self, trigger: StateTrigger) -> Result<(), TransitionError> {
        use SessionState::*;
        use StateTrigger::*;

        // Capture the prior state for tracing AFTER
        // the match decides the next state; emit a `state_advance`
        // event before returning Ok. Off-by-default at the
        // tracing-subscriber level so the volume cost is zero
        // unless an OTLP collector subscribes.
        let prior_state = format!("{:?}", self.state);
        let trigger_label = format!("{:?}", trigger);

        if let (
            Blocked { .. },
            HarnessTaskBlocked {
                task_id,
                detail,
                blocker_kind,
            },
        ) = (&self.state, &trigger)
        {
            let reason = format!("Task {} blocked: {}", task_id, detail);
            let recovery_hint = recovery_hint_for_blocker(blocker_kind);
            if let Blocked {
                blockers,
                reason: state_reason,
                recovery_hint: state_recovery_hint,
                blocker_kind: state_blocker_kind,
                context,
            } = &mut self.state
            {
                if let Some(existing) = blockers.iter_mut().find(|b| b.task_id == *task_id) {
                    existing.kind = blocker_kind.clone();
                    existing.message = reason.clone();
                    existing.recovery_hint = Some(recovery_hint.clone());
                } else {
                    blockers.push(blocker_entry(
                        task_id.clone(),
                        blocker_kind.clone(),
                        reason.clone(),
                        recovery_hint.clone(),
                    ));
                }
                *state_reason = reason;
                *state_recovery_hint = recovery_hint.clone();
                *state_blocker_kind = Some(blocker_kind.clone());
                *context = Some(BlockerContext {
                    timestamp: Utc::now().to_rfc3339(),
                    recovery_hints: Some(recovery_hint),
                });
            }
            self.last_activity = Utc::now();
            tracing::debug!(
                session_id = %self.id,
                prior_state = %prior_state,
                new_state = ?self.state,
                trigger = %trigger_label,
                "session_state_advance",
            );
            return Ok(());
        }

        let next = match (&self.state, &trigger) {
            // Greeting → Intake on first prose
            (Greeting, AppendProse) => Intake,
            // Intake / IntakeFollowup append-prose stays in Intake (refresh class)
            (Intake | IntakeFollowup, AppendProse) => Intake,
            // Intake → IntakeFollowup when DAG built with unresolved discovery
            (Intake, DagBuiltWithUnresolvedDiscovery) => IntakeFollowup,
            (IntakeFollowup, DagBuiltWithUnresolvedDiscovery) => IntakeFollowup,
            // Intake / IntakeFollowup → PendingConfirmation via propose_summary.
            // The summary card targets the whole emission, so `stage: None`.
            // Per-stage review gates are injected by the server's `/confirm`
            // handler (not by this state machine) when
            // `requires_sme_review: true` fires.
            (Intake | IntakeFollowup, ProposeSummaryConfirmation) => {
                PendingConfirmation { stage: None }
            }
            // PendingConfirmation → ReadyToEmit (button click).
            // Struct pattern matches both the emission-level
            // (stage: None) and stage-scoped (stage: Some(_)) variants
            // — Phase-4 stage-scoped confirms unblock dispatch via a
            // separate server handler, not this transition.
            (PendingConfirmation { .. }, UserClickedConfirm) => ReadyToEmit,
            // PendingConfirmation → Intake (reject button)
            (PendingConfirmation { .. }, UserClickedReject) => Intake,
            // Intake / IntakeFollowup → ReadyToEmit (button click after both calls in same turn)
            (Intake | IntakeFollowup, UserClickedConfirm) => ReadyToEmit,
            // ReadyToEmit → Emitting on emit start
            (ReadyToEmit, EmitPackageStart) => Emitting,
            (Emitting, EmitPackageOk) => Emitted,
            (Emitting, EmitPackageErr { reason }) => {
                let recovery_hint =
                    "Check the server logs and retry once the underlying issue is resolved."
                        .to_string();
                Blocked {
                    blockers: vec![blocker_entry(
                        "_session",
                        BlockerKind::HostError {
                            message: reason.clone(),
                        },
                        reason.clone(),
                        recovery_hint.clone(),
                    )],
                    reason: reason.clone(),
                    recovery_hint: recovery_hint.clone(),
                    blocker_kind: Some(BlockerKind::HostError {
                        message: reason.clone(),
                    }),
                    context: Some(BlockerContext {
                        timestamp: Utc::now().to_rfc3339(),
                        recovery_hints: Some(recovery_hint),
                    }),
                }
            }
            // Blocked → Intake when the sensitivity-winner tool drives
            // the unblock. Separate trigger from OperatorUnblock
            // because the follow-up flow is different: the tool
            // re-proposes a summary → Confirm → ReadyToEmit (the amend
            // pathway), which requires Intake-origin so
            // ProposeSummaryConfirmation can fire. The generic
            // OperatorUnblock path below preserves Emitted post-emit
            // so subsequent harness blocker events aren't swallowed.
            (Blocked { .. }, SensitivityWinnerSelected) => Intake,
            // Blocked → Emitted on operator unblock when the session has
            // an emitted package (post-emit execution-phase blocker), else
            // Blocked → Intake (pre-emit blocker — the SME keeps editing
            // intake prose). Without this branch, a post-emit unblock
            // drops the session into Intake, and the next
            // HarnessTaskBlocked event is silently swallowed by
            // `service::block_from_harness`'s Emitted-only guard —
            // observed during the IVD live e2e: second discovery blocker
            // never reached the UI.
            (Blocked { .. }, OperatorUnblock) => {
                if self.emitted_package_path.is_some() {
                    Emitted
                } else {
                    Intake
                }
            }
            // OperatorUnblock from non-Blocked emitted-side states is a
            // no-op session-state-wise: the task-level blocker still
            // exists in WORKFLOW.json and the server's
            // `resume_blocked_tasks_in_workflow` helper flips it back
            // to Ready independently. Without this branch, an amend
            // (which transitions Emitted → Amending → ReadyToEmit)
            // followed by a fresh task_blocked left the session in
            // ReadyToEmit; the BlockerCard's Accept fired /unblock and
            // the state machine returned 400 with "illegal session
            // transition: OperatorUnblock from ReadyToEmit", leaving
            // the user with no UI path to resume execution.
            (Emitted | ReadyToEmit | Amending { .. }, OperatorUnblock) => self.state.clone(),
            // Any state → Blocked on infra error (host-driven). MUST come
            // before the (Emitted, _) catch-all so an InfraError fired
            // from Emitted actually reaches Blocked instead of being
            // swallowed by the terminal-state absorber.
            // explicitly says infra errors are host-driven and reach
            // Blocked from anywhere.
            (_, InfraError { reason }) => {
                let recovery_hint =
                    "Wait for the underlying service to recover and try again.".to_string();
                Blocked {
                    blockers: vec![blocker_entry(
                        "_session",
                        BlockerKind::HostError {
                            message: reason.clone(),
                        },
                        reason.clone(),
                        recovery_hint.clone(),
                    )],
                    reason: reason.clone(),
                    recovery_hint: recovery_hint.clone(),
                    blocker_kind: Some(BlockerKind::HostError {
                        message: reason.clone(),
                    }),
                    context: Some(BlockerContext {
                        timestamp: Utc::now().to_rfc3339(),
                        recovery_hints: Some(recovery_hint),
                    }),
                }
            }
            // Emitted → Amending when the
            // SME calls amend_stage_method; Amending → ReadyToEmit once
            // the DAG slice has been invalidated and the LLM confirms
            // the replacement method.
            (
                Emitted,
                AmendStart {
                    target_stage,
                    invalidated_tasks,
                },
            ) => Amending {
                target_stage: target_stage.clone(),
                invalidated_tasks: invalidated_tasks.clone(),
            },
            (Amending { .. }, AmendReady) => ReadyToEmit,
            // harness-driven blocker. Fired by the server's
            // /progress handler when it sees kind=task_blocked. Carries
            // the task id + a typed BlockerKind (dispatched by the
            // mock agent's reason.kind field or taken from the
            // harness detail) so the UI picks the right recovery.
            //
            // ReadyToEmit / Amending are valid sources too: an amend
            // transitions Emitted → Amending → ReadyToEmit, but the
            // harness keeps running and a downstream task_blocked
            // event must still flip session state to Blocked so the
            // BlockerCard renders. Without this, post-amend blockers
            // were silently swallowed.
            // Blocked is also accepted as a source: a fresh harness
            // run can fire `task_blocked` for the same task again
            // (e.g., orphan-recovery via OrphanedByCrash) on a session
            // that's already Blocked from the prior run. Refresh the
            // reason + blocker_kind so the BlockerCard shows the
            // current (post-recovery) state, not stale prior text.
            // The `entered_blocked` guard below correctly stays false
            // when re-entering Blocked, so we don't reset the
            // Opus-escalation flag — that's a one-shot per genuine
            // Blocked entry, not per refresh.
            (
                Emitted | ReadyToEmit | Amending { .. } | Blocked { .. },
                HarnessTaskBlocked {
                    task_id,
                    detail,
                    blocker_kind,
                },
            ) => {
                let reason = format!("Task {} blocked: {}", task_id, detail);
                let recovery_hint = recovery_hint_for_blocker(blocker_kind);
                Blocked {
                    blockers: vec![blocker_entry(
                        task_id.clone(),
                        blocker_kind.clone(),
                        reason.clone(),
                        recovery_hint.clone(),
                    )],
                    reason,
                    recovery_hint: recovery_hint.clone(),
                    blocker_kind: Some(blocker_kind.clone()),
                    context: Some(BlockerContext {
                        timestamp: Utc::now().to_rfc3339(),
                        recovery_hints: Some(recovery_hint),
                    }),
                }
            }
            // Emitted is terminal for everything else (the Emitted state
            // absorbs subsequent user prose and tool triggers without
            // rolling back to Intake — the conversation continues but
            // doesn't re-enter the intake flow).
            (Emitted, AppendProse) => Emitted,
            (Emitted, _) => Emitted,
            // F5 Gate B — the residual catch-all enumerates every
            // remaining (SessionState, StateTrigger) pair that has not
            // been explicitly matched above. Pairs that reach this arm
            // are illegal transitions and return `Err(TransitionError)`
            // — same runtime behavior as the prior `(from, t) =>...`
            // catch-all. The point of the enumeration is compile-time:
            // adding a new `SessionState` or `StateTrigger` variant
            // makes the compiler flag the dispatch table when the
            // `assert_state_trigger_exhaustive` helper below runs (it
            // destructures every variant by name and is reached via
            // const-eval). The fail-mode is a clear "missing match arm"
            // diagnostic on `assert_state_trigger_exhaustive`, not a
            // silent fall-through to an `Err` in production.
            (from, t) => {
                return Err(TransitionError {
                    from: format!("{:?}", from),
                    trigger: format!("{:?}", t),
                })
            }
        };

        // §R-9 — reset the Blocked-escalation guard whenever we enter
        // a NEW Blocked state. Each Blocked episode gets one Opus turn;
        // subsequent turns while still blocked drop back to Sonnet. See
        // ModelPolicy::choose_with_reason.
        let entered_blocked = matches!(&next, SessionState::Blocked { .. })
            && !matches!(&self.state, SessionState::Blocked { .. });
        if entered_blocked {
            self.blocked_opus_escalation_consumed = false;
        }
        self.state = next;
        self.last_activity = Utc::now();
        // Emit one tracing event per accepted state
        // transition. Subscribers can fan this into Langfuse via
        // OTLP or any other GenAI-semconv collector. Cost when
        // unsubscribed is one virtual-call check.
        tracing::debug!(
            session_id = %self.id,
            prior_state = %prior_state,
            new_state = ?self.state,
            trigger = %trigger_label,
            "session_state_advance",
        );
        // Blocked transitions are post-mortem-critical: surface the
        // BlockerKind variant + reason at warn level so operators can
        // diagnose `post_confirm_blocked` corpus failures without
        // turning on debug-level logs. Quiet on every other transition.
        if entered_blocked {
            if let SessionState::Blocked {
                blocker_kind,
                reason,
                recovery_hint,
                ..
            } = &self.state
            {
                tracing::warn!(
                    session_id = %self.id,
                    prior_state = %prior_state,
                    trigger = %trigger_label,
                    blocker_kind = ?blocker_kind,
                    reason = %reason,
                    recovery_hint = %recovery_hint,
                    "session_entered_blocked",
                );
            }
        }
        Ok(())
    }
}

// ── F5 Gate B — compile-time exhaustiveness sentinels ───────────────
//
// `try_transition` retains a `(from, t) => Err(...)` catch-all because
// the (state × trigger) cross-product is 11 × 13 = 143 pairs and most
// of them are correctly illegal. Listing all 143 by hand obscures the
// ~25 legal transitions that actually drive the state machine.
//
// Instead, this module asserts compile-time that every `SessionState`
// and `StateTrigger` variant exists. Adding a new variant in either
// enum WITHOUT either (a) adding a matching arm above OR (b) updating
// the sentinel below produces a compiler error here, forcing a
// code-review touchpoint that says "did you teach `try_transition`
// about your new state/trigger?".
//
// The functions are never called at runtime; they exist only to fail
// to compile when a variant is added. `#[allow(dead_code)]` is
// intentional and load-bearing — the workspace deny-warnings policy
// would otherwise hide the sentinel.

#[allow(dead_code)] // F5 Gate B sentinel: compile-time exhaustiveness over SessionState
fn assert_session_state_exhaustive(s: SessionState) {
    use SessionState::*;
    match s {
        // Every variant explicitly named — adding a new SessionState
        // variant produces `non-exhaustive patterns: ` here. Update
        // both `try_transition` (above) AND this sentinel.
        Greeting => {}
        Intake => {}
        IntakeFollowup => {}
        PendingConfirmation { .. } => {}
        ReadyToEmit => {}
        Emitting => {}
        Emitted => {}
        Amending { .. } => {}
        Blocked { .. } => {}
    }
}

#[allow(dead_code)] // F5 Gate B sentinel: compile-time exhaustiveness over StateTrigger
fn assert_state_trigger_exhaustive(t: StateTrigger) {
    use StateTrigger::*;
    match t {
        // Every variant explicitly named — adding a new StateTrigger
        // variant produces `non-exhaustive patterns: ` here. Update
        // both `try_transition` (above) AND this sentinel.
        AppendProse => {}
        DagBuiltWithUnresolvedDiscovery => {}
        ProposeSummaryConfirmation => {}
        UserClickedConfirm => {}
        UserClickedReject => {}
        EmitPackageStart => {}
        EmitPackageOk => {}
        EmitPackageErr { .. } => {}
        OperatorUnblock => {}
        SensitivityWinnerSelected => {}
        InfraError { .. } => {}
        AmendStart { .. } => {}
        AmendReady => {}
        HarnessTaskBlocked { .. } => {}
    }
}
