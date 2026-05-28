//! Modularity split. The user-facing `send_turn` flow,
//! the `metrics_snapshot` accessor, and the background `maybe_auto_title`
//! detached-task helper live here so `service/mod.rs` stays a thin
//! re-export shell. Pure code movement from `service/mod.rs`; no
//! behavior change.
//!
//! Cohesive unit: every method here participates in the per-turn
//! lifecycle the SME observes from the chat surface — read session,
//! drive the tool loop, persist the merged result, fire telemetry,
//! enqueue the auto-title side-call.

use super::{ConversationService, ServiceError};
use crate::model_policy::ModelPolicy;
use crate::session::{Session, SessionId, SessionState, Turn};
use chrono::Utc;
use std::time::Instant;

impl ConversationService {
    /// Read the per-session metrics snapshot, if any turns have been
    /// recorded yet. F3 — populates `session_duration_seconds` from
    /// `Session::created_at` when the session can be loaded; F4 —
    /// hydrates `stage_class` on per-task agent entries from the
    /// session's DAG. Both are best-effort: callers that only have a
    /// MetricsStore handle (e.g. some tests) get zero / `None` in
    /// those fields and everything else still works.
    pub async fn metrics_snapshot(&self, id: SessionId) -> Option<crate::metrics::SessionMetrics> {
        let mut snap = self.metrics_store().snapshot(id).await?;
        if let Some(session) = self.store_handle().get(id).await {
            // F3 — wall-clock duration since session creation.
            let now = Utc::now();
            let elapsed = now.signed_duration_since(session.created_at).num_seconds();
            snap.session_duration_seconds = elapsed.max(0) as u64;
            // F4 — populate stage_class per task from the DAG. The
            // snapshot already carries per_task_agent sorted by cost
            // desc; we just patch the optional stage_class field.
            // Task::spec is Option<serde_json::Value> in core; the
            // taxonomies serialize stage_class as a top-level string
            // inside that JSON blob.
            if let Some(dag) = &session.dag {
                for entry in snap.per_task_agent.iter_mut() {
                    if let Some(task) = dag.tasks.get(entry.task_id.as_str()) {
                        if let Some(spec) = &task.spec {
                            if let Some(sc) = spec.get("stage_class").and_then(|v| v.as_str()) {
                                if !sc.is_empty() {
                                    entry.stage_class = Some(sc.to_string());
                                }
                            }
                        }
                    }
                }
            }
            // Catalog-gap telemetry. Drain the session-scoped
            // `AffordanceFallbackCounter` into the metrics snapshot so
            // the `/metrics` endpoint exposes the current sorted gap
            // list. The counter is transient (serde-skip) so this gives
            // the "since last restart" view, which is all the UI needs
            // for the catalog-gaps card.
            snap.affordance_fallbacks = session
                .affordance_fallback_counter
                .all_gaps_sorted_by_count_desc()
                .into_iter()
                .map(|(semantic_type, primitive, count)| {
                    crate::metrics::AffordanceFallbackSummary {
                        semantic_type,
                        primitive,
                        count,
                    }
                })
                .collect();
            // Budget fields, projected finish. Compute projected
            // remaining from per-stage-class median cost × unfinished
            // tasks in that class, plus a fallback overall median ×
            // remaining when no per-class history exists. Budget state
            // chip rule: 0–74% ok / 75–100% warn / >100% exceeded.
            snap.budget_usd = session.budget_usd;
            let remaining = project_remaining_cost(&snap.per_task_agent, session.dag.as_ref());
            snap.projected_remaining_usd = remaining;
            snap.projected_finish_usd = snap.total_cost_usd + remaining;
            if let Some(cap) = session.budget_usd {
                if cap > 0.0 {
                    let used = snap.total_cost_usd / cap;
                    snap.budget_used_pct = Some(used);
                    let label = if used >= 1.0 {
                        "exceeded"
                    } else if used >= 0.75 {
                        "warn"
                    } else {
                        "ok"
                    };
                    snap.budget_state = Some(label.to_string());
                }
            }
        }
        Some(snap)
    }

    /// Send a user message and drive the tool-use loop until the LLM
    /// returns `stop_reason = end_turn` (or the cap is reached).
    ///
    /// `client_user_turn_id` is the optimistic UUID the UI generated
    /// for its local user-turn append. When present and well-formed,
    /// the server uses it for the persisted user Turn so the 60s
    /// reconciliation poll's `mergeBy(turn_id)` dedupes against the
    /// optimistic append. Malformed ids are ignored (server mints a
    /// fresh UUID) and a tracing warning is recorded.
    #[tracing::instrument(skip(self, user_message), fields(session_id = %id))]
    pub async fn send_turn(
        &self,
        id: SessionId,
        user_message: String,
        client_user_turn_id: Option<String>,
    ) -> Result<Turn, ServiceError> {
        // Serialize concurrent send_turn calls for the SAME session.
        // Two parallel POST /turn requests on the same session would
        // otherwise both clone the session, both run the tool loop on
        // their local copies in `ReadyToEmit`, and both call
        // `emit_package` — racing on the plotting-library copy. Held
        // across the full turn. Different sessions never contend.
        let turn_lock = self.session_turn_lock_handle(id);
        let _turn_guard = turn_lock.lock().await;

        let mut session = self
            .store_handle()
            .get(id)
            .await
            .ok_or(ServiceError::SessionNotFound)?;

        let user_text = user_message.clone();
        // Capture the initial state *before* the auto-append mutation so
        // the merge logic below can disambiguate "tool loop deliberately
        // transitioned out of this state" from "harness concurrently
        // wrote a new state while the loop was running".
        let initial_state = session.state.clone();
        // Snapshot baselines for delta merge. Concurrent writers
        // (harness_batch::flush, REST endpoints like /confirm, side-call
        // writebacks) may append turns / tool_call entries / decisions to
        // the persisted session while this tool loop is running on a
        // clone. Wholesale-replacing those fields on merge silently
        // clobbers those writes. Capture lengths at snapshot time so
        // the merge can apply DELTAS (the new entries this loop
        // appended) instead of overwriting.
        let baseline_conversation_len = session.conversation.len();
        let baseline_tool_call_log_len = session.tool_call_log.len();
        let baseline_decisions_len = session.decisions.len();
        let mut user_turn = Turn::user(user_message);
        if let Some(client_id) = client_user_turn_id {
            // Validate shape (UUID v4 hex with dashes) before
            // trusting it; malformed input falls back to the
            // server-minted id.
            match uuid::Uuid::parse_str(&client_id) {
                Ok(parsed) => {
                    user_turn.turn_id = parsed;
                }
                Err(_) => {
                    tracing::warn!(
                        client_id = %client_id,
                        "client user_turn_id not a valid UUID; minting server id"
                    );
                }
            }
        }
        std::sync::Arc::make_mut(&mut session.conversation).push(user_turn);
        session.last_activity = Utc::now();
        self.maybe_auto_append(&mut session, &user_text);

        let started = Instant::now();
        let tool_calls_before = session.tool_call_log.len() as u64;
        let (model_chosen, escalation_reason) = ModelPolicy::choose_with_reason(&session);
        if let Some(reason) = escalation_reason {
            self.metrics_store()
                .record_opus_escalation(id, reason)
                .await;
            // §R-9: consume the Blocked-episode's one-shot Opus
            // escalation so later turns in the same Blocked episode
            // drop back to Sonnet. Careful-mode + low-confidence
            // escalations don't have the one-shot property (they're
            // session-level, not episodic) and keep firing every turn.
            if matches!(reason, crate::model_policy::EscalationReason::Blocked) {
                session.blocked_opus_escalation_consumed = true;
            }
        }

        let (final_turn, usage) = self.run_tool_loop(&mut session).await?;
        std::sync::Arc::make_mut(&mut session.conversation).push(final_turn.clone());

        // Turn-completion log. Captures initial → final state so
        // corpus post-mortem can detect "LLM stalled in Intake / IntakeFollowup
        // across N turns" patterns by grouping log lines per session_id.
        // Quiet when no transition occurs at the debug level; bumps to
        // warn when the LLM consumed a turn but didn't advance state.
        let advanced =
            std::mem::discriminant(&initial_state) != std::mem::discriminant(&session.state);
        let is_intake_phase = matches!(
            session.state,
            crate::session::SessionState::Intake
                | crate::session::SessionState::IntakeFollowup
                | crate::session::SessionState::Greeting
        );
        if !advanced && is_intake_phase {
            tracing::warn!(
                session_id = %session.id,
                state = ?session.state,
                conversation_len = session.conversation.len(),
                tool_calls = session.tool_call_log.len() as u64 - tool_calls_before,
                "turn_completed_without_state_advance_in_intake",
            );
        } else {
            tracing::debug!(
                session_id = %session.id,
                prior = ?initial_state,
                next = ?session.state,
                tool_calls = session.tool_call_log.len() as u64 - tool_calls_before,
                "turn_completed",
            );
        }

        // merge against the latest persisted session
        // instead of a bare save. The local `session` was read at the
        // top of this function; if the harness posted task_blocked /
        // task_completed progress events during the turn, those
        // updates (dag task states + Blocked session state) were written
        // to the store concurrently. A bare save of the local copy would
        // clobber them. The merge below keeps the harness-driven dag +
        // state while applying the tool-loop's conversation and
        // intake mutations forward.
        let merged_final_state = session.state.clone();
        // Phase D refactor: session.dag is now a derived memoization
        // cache. Authority lives in `workflow_dag` (structure) +
        // `task_states` (runtime state). The structural-merge gate is
        // gone — invalidating the cache on either side is the
        // single source of truth.
        let local_task_states = session.task_states.clone();
        self.store_handle()
            .update(id, |current| {
                // Tool-loop mutations we always want forward.
                //
                // Append-since-snapshot for conversation /
                // tool_call_log / decisions, and by-id merge for
                // proposals. Concurrent writers (harness_batch::flush,
                // REST /confirm, side-call writebacks, /proposals/:id/
                // approve|reject) may have written to `current` between
                // the snapshot at line 113 and this merge. Wholesale-
                // clone would silently clobber those writes; the
                // delta-merge below preserves them while still
                // forwarding everything this tool-loop produced.
                let new_turns: Vec<Turn> = session
                    .conversation
                    .iter()
                    .skip(baseline_conversation_len)
                    .cloned()
                    .collect();
                std::sync::Arc::make_mut(&mut current.conversation).extend(new_turns);
                let new_tool_calls: Vec<_> = session
                    .tool_call_log
                    .iter()
                    .skip(baseline_tool_call_log_len)
                    .cloned()
                    .collect();
                current.tool_call_log.extend(new_tool_calls);
                current.intake_methods = session.intake_methods.clone();
                // Sub-archetype small-task exclusion list. The LLM is
                // the only writer (via set_intake_excluded_atoms);
                // without this propagation the tool-loop mutation gets
                // clobbered by the merge and rebuild_dag never sees the
                // exclusion set.
                current.excluded_atoms = session.excluded_atoms.clone();
                current.intake_prose = session.intake_prose.clone();
                current.classification = session.classification.clone();
                current.taxonomy = session.taxonomy.clone();
                current.emitted_package_path = session
                    .emitted_package_path
                    .clone()
                    .or(current.emitted_package_path.clone());
                // The previous merge was:
                //   current.user_confirmed = session.user_confirmed
                //       || current.user_confirmed;
                // which silently undid a concurrent `/reject` mid-turn.
                // The LLM cannot mutate the confirmation latch through
                // any tool in the closed vocabulary; only `/confirm` and
                // `/reject` write it. Therefore the persisted
                // `current.confirmation_token` is the authoritative
                // SME-driven latch — don't touch it from the snapshot.
                //
                // user_confirmed is now confirmation_token (per C2);
                // discard both the token AND the pending emission id
                // (server-owned).
                let _ = &session.confirmation_token; // explicitly discard the snapshot
                let _ = session.pending_emission_id; // explicitly discard the snapshot
                                                     // By-id merge for proposals. The tool loop only
                                                     // CREATES proposals (via propose_hypothesized_*); REST
                                                     // /proposals/:id/approve|reject and the server-side gate
                                                     // runner transition `lifecycle` from PendingSme to
                                                     // Promoted/Rejected. So: insert locals whose id is not
                                                     // in current (new proposals from this loop); keep
                                                     // current's entry when both sides know the id (preserve
                                                     // any concurrent lifecycle advance).
                for (id, prop) in &session.proposals {
                    if !current.proposals.contains_key(id) {
                        current.proposals.insert(id.clone(), prop.clone());
                    }
                }
                // Append-since-snapshot for decisions. A
                // concurrent /confirm POST appends UserClickedConfirm;
                // wholesale-clone would revert it.
                let new_decisions: Vec<_> = session
                    .decisions
                    .iter()
                    .skip(baseline_decisions_len)
                    .cloned()
                    .collect();
                current.decisions.extend(new_decisions);
                current.workflow_dag = session
                    .workflow_dag
                    .clone()
                    .or(current.workflow_dag.clone());
                current.compose_outcome = session
                    .compose_outcome
                    .clone()
                    .or(current.compose_outcome.clone());
                if !session.ranked_alternatives.is_empty() {
                    current.ranked_alternatives = session.ranked_alternatives.clone();
                }
                if !session.policy_decisions.is_empty() {
                    current.policy_decisions = session.policy_decisions.clone();
                }
                current.active_policy_bundle = session
                    .active_policy_bundle
                    .clone()
                    .or(current.active_policy_bundle.clone());
                // §R-9: propagate the one-shot Blocked-Opus guard
                // through the store merge. OR-semantics: if either the
                // local turn or the persisted copy already consumed the
                // escalation, future turns in this episode skip Opus.
                current.blocked_opus_escalation_consumed = session.blocked_opus_escalation_consumed
                    || current.blocked_opus_escalation_consumed;
                current.last_activity = Utc::now();
                // Session state + dag: take WHICHEVER is "further along"
                // by preserving the harness-written Blocked state if the
                // store saw it during the turn. A local Emitted stays
                // Emitted when merged with a current Emitted, but yields
                // to a current Blocked.
                //
                // The subtle case: `select_sensitivity_winner` +
                // `OperatorUnblock`-driven tools transition Blocked →
                // Intake deliberately. If we naively prefer a persisted
                // Blocked, we clobber the intentional unblock. The
                // disambiguation is `initial_state`: when the local
                // turn started in Blocked and the persisted state is
                // still Blocked (i.e. no concurrent harness write), the
                // local transition is the authoritative mutation.
                let concurrent_harness_block =
                    matches!(current.state, SessionState::Blocked { .. })
                        && !matches!(initial_state, SessionState::Blocked { .. });
                if concurrent_harness_block
                    && !matches!(merged_final_state, SessionState::Blocked { .. })
                {
                    // Keep current (harness-driven block); the harness's
                    // task_states stays as-is too.
                } else {
                    current.state = merged_final_state.clone();
                    // session.dag is now a derived cache. The structural
                    // merge is gone — authority is `workflow_dag`
                    // (replaced above) + `task_states` (merged below).
                    // Invalidate the cache so the next read re-derives.
                    //
                    // Merge `task_states` with last-writer-wins on a
                    // per-task basis: harness writes that arrived on
                    // `current` during the turn are preserved, and the
                    // local tool-loop's writes (typically reset-on-
                    // rebuild via `invalidate_and_rebuild`) overlay
                    // anything the local turn touched. The harness
                    // path advances Pending → Running → Completed
                    // monotonically per task, so the merge is the
                    // union of both sides keyed by task id.
                    for (tid, state) in &local_task_states {
                        current.task_states.insert(tid.clone(), state.clone());
                    }
                    #[allow(deprecated)]
                    // Deliberate cache-reset for non-workflow_dag state change
                    current.invalidate_dag();
                }
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;

        // Fire `state_advanced` AFTER persistence,
        // not during the tool loop. The previous fire site in
        // `tool_loop.rs` ran against the local clone; if a subscriber
        // refetched /state in response, they could observe the stale
        // persisted state with the merged state landing milliseconds
        // later — a transient flicker visible in BlockerCard. Re-read
        // the persisted state through the store so the event mirrors
        // exactly what `get_state` would now return (including any
        // harness-driven block that the merge preserved). A bare-event
        // miss here (session disappeared between merge and read) is
        // benign — the 60s transcript poll will reconcile.
        if let Some(sink) = self.event_sink() {
            if let Some(persisted) = self.store_handle().get(id).await {
                sink.state_advanced(id, &persisted.state);
            }
        }

        // Record telemetry. Token counts come from Anthropic's `usage` block
        // (both streaming message_delta and non-streaming response), summed
        // across every LLM call inside the tool loop. Mock backends leave
        // Usage at Default (zero), so the mock path still records zeros.
        let tool_calls_in_turn = session.tool_call_log.len() as u64 - tool_calls_before;
        // F2 — bucket per-tool-name calls. Walk the slice of
        // tool_call_log entries appended during this turn (the tool
        // loop pushes one record per dispatch via record_tool_call_ok
        // / err) and increment the per-tool counter.
        let tool_log_slice = &session.tool_call_log[tool_calls_before as usize..];
        for record in tool_log_slice {
            self.metrics_store()
                .record_tool_call(id, &record.tool_name)
                .await;
        }
        self.metrics_store()
            .record_turn(
                id,
                started.elapsed(),
                tool_calls_in_turn,
                usage.input_tokens as u64,
                usage.output_tokens as u64,
                usage.cache_read_input_tokens as u64,
                usage.cache_creation_input_tokens as u64,
                model_chosen,
            )
            .await;
        // Count clarification turns. If the session was in
        // IntakeFollowup when the SME started this turn, this is one
        // additional clarification round before PendingConfirmation.
        // Use `initial_state` (captured before the tool loop) so a turn
        // that both arrives in IntakeFollowup AND reaches
        // PendingConfirmation in the same tool loop still registers.
        // Best-effort: a metrics IO failure never fails the turn.
        if matches!(initial_state, SessionState::IntakeFollowup) {
            self.metrics_store().record_intake_followup_turn(id).await;
        }

        // Optional per-turn token-burn logger, off by default. Set
        // SWFC_DEBUG_TOKEN_BURN=1 to stream a
        // one-line summary to stderr after every turn so operators
        // can watch cache-hit ratios and spend accumulate in real
        // time without opening the UI. Warns on suspicious ratios
        // (<30% after 3+ turns on the session) — the canonical
        // silent-cache-invalidation signal.
        if scripps_workflow_core::env_helpers::env_bool("SWFC_DEBUG_TOKEN_BURN") {
            if let Some(snap) = self.metrics_store().snapshot(id).await {
                let billed =
                    snap.total_input_tokens + snap.cache_read_tokens + snap.cache_creation_tokens;
                let ratio_pct = if billed == 0 {
                    0.0
                } else {
                    100.0 * (snap.cache_read_tokens as f64) / (billed as f64)
                };
                let warn = if snap.turn_count >= 3 && ratio_pct < 30.0 {
                    " ⚠ low cache hit ratio"
                } else {
                    ""
                };
                eprintln!(
                    "[token-burn] session={} turn={} model={} iter_in={} iter_out={} iter_cache_read={} iter_cache_write={} session_ratio={:.0}% session_cost=${:.4}{}",
                    id,
                    snap.turn_count,
                    model_chosen.api_id(),
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_input_tokens,
                    usage.cache_creation_input_tokens,
                    ratio_pct,
                    snap.total_cost_usd,
                    warn
                );
            }
        }

        // emit turn_appended so the UI can append locally
        // instead of waiting for the 15s transcript poll.
        if let Some(sink) = self.event_sink() {
            sink.turn_appended(id, &final_turn);
        }

        // Auto-fire session title once the session has classification
        // + at least 6 non-system turns. Spawned in a detached task so
        // the user's send_turn response doesn't wait on the Haiku
        // call. Idempotent (no-op if title is already set). The
        // dedicated /auto-title route remains for explicit operator
        // triggers.
        self.maybe_auto_title(id).await;

        Ok(final_turn)
    }

    /// Return a snapshot of the session with `id`, or `None` if not found.
    pub async fn get_session(&self, id: SessionId) -> Option<Session> {
        self.store_handle().get(id).await
    }

    /// Record an SME resolution against an unresolved
    /// assumption in the session's cached `WorkflowDag`. Updates
    /// the assumption's `resolution` field in-place AND appends a
    /// `DecisionType::AssumptionResolved` to the session's
    /// decisions log. The decision log is the durable source of
    /// truth (the AssumptionLedger is a typed projection over
    /// decisions.jsonl); the in-memory update is the read cache the
    /// chat_routes/compose endpoint serves.
    pub async fn resolve_assumption(
        &self,
        id: SessionId,
        assumption_id: String,
        resolution: String,
        rationale: Option<String>,
    ) -> Result<(), ServiceError> {
        let store = self.store_handle();
        store.get(id).await.ok_or(ServiceError::SessionNotFound)?;

        // Map UI-side resolution string to the decision-log enum
        // string (matches the `AssumptionResolution` Rust enum's
        // snake_case rename form).
        let decision_resolution = match resolution.as_str() {
            "confirmed" => "accepted",
            "rejected" => "rejected",
            other => other,
        }
        .to_string();

        let saved = store
            .update(id, |session| {
                // Update the in-memory cache. Soft-skip when the
                // assumption isn't found — the decision log entry still
                // captures the SME's intent.
                if let Some(dag) = session.workflow_dag.as_mut() {
                    if let Some(entry) = dag
                        .assumptions
                        .entries
                        .iter_mut()
                        .find(|a| a.id == assumption_id)
                    {
                        use scripps_workflow_core::workflow_contracts::evidence::AssumptionResolution;
                        entry.resolution = match decision_resolution.as_str() {
                            "accepted" => AssumptionResolution::Accepted {
                                rationale: rationale.clone().unwrap_or_default(),
                            },
                            "rejected" => AssumptionResolution::Rejected {
                                rationale: rationale.clone().unwrap_or_default(),
                            },
                            _ => AssumptionResolution::Unresolved,
                        };
                    }
                }

                // Append an audit-log entry. Mirrors the design §6 work
                // item 4 contract that AssumptionLedger lives as a typed
                // projection over decisions.jsonl.
                use scripps_workflow_core::decision_log::{
                    DecisionActor, DecisionRecord, DecisionType,
                };
                let decision = DecisionRecord::new(
                    id.to_string(),
                    DecisionType::AssumptionResolved {
                        id: assumption_id.clone(),
                        resolution: decision_resolution.clone(),
                    },
                    DecisionActor::Sme,
                    rationale,
                );
                session.decisions.push(decision);
                session.last_activity = Utc::now();
                tracing::info!(
                    session_id = %id,
                    assumption_id = %assumption_id,
                    resolution = %decision_resolution,
                    "assumption resolved by SME"
                );
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(format!("save failed: {e}")))?;

        if let Some(sink) = self.event_sink() {
            sink.state_advanced(id, &saved.state);
        }

        Ok(())
    }

    /// Record the SME's confirm/reject decision against an
    /// inserted adapter surfaced by the AdapterWarningCard. Appends a
    /// `DecisionType::AdapterDecisionRecorded` to the audit log so
    /// the decision becomes durable. No DAG rebuild — the adapter
    /// stays in the composition; this only records the SME's
    /// awareness of its risk class.
    pub async fn record_adapter_decision(
        &self,
        id: SessionId,
        adapter_id: String,
        decision: String,
        safety: String,
    ) -> Result<(), ServiceError> {
        let store = self.store_handle();
        store.get(id).await.ok_or(ServiceError::SessionNotFound)?;
        let saved = store
            .update(id, |session| {
                use scripps_workflow_core::decision_log::{
                    DecisionActor, DecisionRecord, DecisionType,
                };
                let record = DecisionRecord::new(
                    id.to_string(),
                    DecisionType::AdapterDecisionRecorded {
                        adapter_id: adapter_id.clone(),
                        decision: decision.clone(),
                        safety,
                    },
                    DecisionActor::Sme,
                    None,
                );
                session.decisions.push(record);
                session.last_activity = Utc::now();
                tracing::info!(
                    session_id = %id,
                    adapter_id = %adapter_id,
                    decision = %decision,
                    "adapter decision recorded by SME"
                );
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(format!("save failed: {e}")))?;
        if let Some(sink) = self.event_sink() {
            sink.state_advanced(id, &saved.state);
        }
        Ok(())
    }

    /// Record an SME accept/reject decision against a
    /// `NovelNodeSpec` outcome from the v4 planner. Persists to the
    /// audit log via `DecisionType::NovelNodeDecisionRecorded`. The
    /// proposed node remains in the proposals registry; promotion
    /// to executable status still requires validation, sandbox, and
    /// promotion gates.
    pub async fn record_novel_node_decision(
        &self,
        id: SessionId,
        node_id: String,
        decision: String,
    ) -> Result<(), ServiceError> {
        let store = self.store_handle();
        store.get(id).await.ok_or(ServiceError::SessionNotFound)?;
        let saved = store
            .update(id, |session| {
                use scripps_workflow_core::decision_log::{
                    DecisionActor, DecisionRecord, DecisionType,
                };
                let record = DecisionRecord::new(
                    id.to_string(),
                    DecisionType::NovelNodeDecisionRecorded {
                        node_id: node_id.clone(),
                        decision: decision.clone(),
                    },
                    DecisionActor::Sme,
                    None,
                );
                session.decisions.push(record);
                session.last_activity = Utc::now();
                tracing::info!(
                    session_id = %id,
                    node_id = %node_id,
                    decision = %decision,
                    "novel-node decision recorded by SME"
                );
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(format!("save failed: {e}")))?;
        if let Some(sink) = self.event_sink() {
            sink.state_advanced(id, &saved.state);
        }
        Ok(())
    }

    /// Record the SME's chosen recovery affordance after
    /// a `Refusal` composition outcome. Appends
    /// `DecisionType::RefusalAcknowledged` to the audit log. The
    /// actual recovery action (branch / amend-policy) is dispatched
    /// separately by the UI via the existing `branch_session` /
    /// `set_active_policy_bundle` endpoints; this record captures
    /// the SME's intent for handoff continuity.
    pub async fn acknowledge_refusal(
        &self,
        id: SessionId,
        refusal_id: String,
        recovery: String,
    ) -> Result<(), ServiceError> {
        let store = self.store_handle();
        store.get(id).await.ok_or(ServiceError::SessionNotFound)?;
        let saved = store
            .update(id, |session| {
                use scripps_workflow_core::decision_log::{
                    DecisionActor, DecisionRecord, DecisionType,
                };
                let record = DecisionRecord::new(
                    id.to_string(),
                    DecisionType::RefusalAcknowledged {
                        refusal_id: refusal_id.clone(),
                        recovery: recovery.clone(),
                    },
                    DecisionActor::Sme,
                    None,
                );
                session.decisions.push(record);
                session.last_activity = Utc::now();
                tracing::info!(
                    session_id = %id,
                    refusal_id = %refusal_id,
                    recovery = %recovery,
                    "refusal acknowledged by SME"
                );
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(format!("save failed: {e}")))?;
        if let Some(sink) = self.event_sink() {
            sink.state_advanced(id, &saved.state);
        }
        Ok(())
    }

    /// Set or clear the session's active policy bundle.
    /// Persists to the session store AND immediately rebuilds the
    /// DAG so the per-node policy gate fires before the SME's next
    /// action (typically `/confirm`). Returns the new active bundle
    /// (or `None` if cleared).
    ///
    /// Why rebuild eagerly rather than wait for the next turn: the
    /// ClinicalConfirmGate flow is "confirm bundle → click Accept";
    /// without an eager rebuild the Composition tab would show
    /// stale policy_decisions and the emit would lower a DAG
    /// composed under the old policy.
    pub async fn set_active_policy_bundle(
        &self,
        id: SessionId,
        bundle_id: Option<String>,
    ) -> Result<Option<String>, ServiceError> {
        let store = self.store_handle();
        store.get(id).await.ok_or(ServiceError::SessionNotFound)?;
        let config_dir = self.config_dir().clone();
        let saved = store
            .update(id, |session| {
                let prior = session.active_policy_bundle.clone();
                session.active_policy_bundle = bundle_id.clone();
                session.last_activity = Utc::now();
                tracing::info!(
                    session_id = %id,
                    prior = ?prior,
                    new = ?bundle_id,
                    "active policy bundle changed"
                );

                // Rebuild the DAG under the new policy. Soft-fail: if the
                // rebuild itself errors (no taxonomy yet, classification
                // missing, etc.) we still persist the bundle change so the
                // next user turn naturally picks it up.
                if session.classification.is_some() && session.taxonomy.is_some() {
                    let _ = crate::tools::rebuild_dag(session, &config_dir);
                }
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(format!("save failed: {e}")))?;

        // Emit state_advanced so the UI's CompositionTab refreshes
        // its cached compose-outcome / policy-decisions data
        // immediately. The state itself doesn't change, but
        // re-emitting the current state is the cheapest way to
        // trigger the existing refresh path.
        if let Some(sink) = self.event_sink() {
            sink.state_advanced(id, &saved.state);
        }

        Ok(bundle_id)
    }

    /// §R7 — best-effort background fire of session auto-title once
    /// the session has enough conversational depth + a classification.
    /// Spawned as a detached tokio task so it doesn't block the user's
    /// turn response. Silent on every failure mode (network blip,
    /// auto-title disabled, etc.) — the dedicated route handles
    /// failures the operator cares about visibly.
    ///
    /// Gated on `SWFC_AUTO_TITLE=1` (same flag as the explicit
    /// `/auto-title` route) so that:
    /// - Production deployments that set the flag get a title once
    ///   the session ripens (no operator action required).
    /// - Test fixtures that don't set the flag stay deterministic
    ///   (no extra LLM call consumed by a background task).
    pub(super) async fn maybe_auto_title(&self, id: SessionId) {
        const AUTO_TITLE_TURN_THRESHOLD: usize = 6;
        if std::env::var("SWFC_AUTO_TITLE").ok().as_deref() != Some("1") {
            return;
        }
        let session = match self.store_handle().get(id).await {
            Some(s) => s,
            None => return,
        };
        if session.title.is_some() {
            return;
        }
        if session.classification.is_none() {
            return;
        }
        let non_system_turns = session
            .conversation
            .iter()
            .filter(|t| t.role != crate::session::TurnRole::System)
            .count();
        if non_system_turns < AUTO_TITLE_TURN_THRESHOLD {
            return;
        }
        // Atomic claim: `DashSet::insert(id)` returns `true` when the
        // SessionId was newly inserted and `false` when an in-flight
        // spawn already claimed the slot. This closes the
        // check-then-spawn race: two concurrent `send_turn` calls that
        // both observed `session.title.is_none()` will only have ONE
        // pass the insert here; the other bails without spawning. The
        // `AutoTitleInFlightGuard` below releases the claim on drop so
        // a later turn can retry if the first attempt failed.
        if !self.auto_title_in_flight.insert(id) {
            return;
        }
        let llm = self.llm().clone();
        let store = self.store_handle();
        let metrics = self.metrics_store().clone();
        let conversation = (*session.conversation).clone();
        // Pass the archetype id (when the composer
        // pinned one via S6.9) into the auto-title side-call so
        // titles surface as `"<summary> — <archetype_id>"` instead
        // of bare `<summary>`. Snapshot is `Option`; legacy /
        // backward-chain sessions stay unchanged. Only the id is
        // threaded — the full archetype is too heavy for a one-
        // sentence prompt context.
        let archetype_id = session.archetype_snapshot.as_ref().map(|a| a.id.clone());
        let in_flight = self.auto_title_in_flight.clone();
        tokio::spawn(async move {
            // RAII guard releases the in-flight claim no matter how the
            // future ends (success, failure, panic, cancellation).
            let _guard = AutoTitleInFlightGuard { in_flight, id };
            match crate::side_calls::generate_session_title(
                llm,
                &metrics,
                id,
                &conversation,
                archetype_id.as_deref(),
            )
            .await
            {
                Ok(title) => {
                    let _ = store
                        .update(id, |s| {
                            if s.title.is_none() {
                                s.title = Some(title);
                            }
                            Ok(())
                        })
                        .await;
                }
                Err(e) => {
                    eprintln!(
                        "[auto-title] background title generation failed for {}: {}",
                        id, e
                    );
                }
            }
        });
    }
}

/// RAII guard that releases an `auto_title_in_flight` claim on drop.
/// Ensures the slot is freed even if the side-call panics or the
/// detached task is cancelled before its body runs to completion.
struct AutoTitleInFlightGuard {
    in_flight: std::sync::Arc<dashmap::DashSet<SessionId>>,
    id: SessionId,
}

impl Drop for AutoTitleInFlightGuard {
    fn drop(&mut self) {
        self.in_flight.remove(&self.id);
    }
}

/// Estimate remaining-task cost by grouping completed tasks' cost by
/// stage_class (median × remaining tasks in that class). Returns 0.0
/// when there are no completions or no DAG. Runs in O(tasks) time
/// with one median computation per stage_class.
fn project_remaining_cost(
    per_task: &[crate::metrics::PerTaskAgentSnapshot],
    dag: Option<&scripps_workflow_core::dag::DAG>,
) -> f64 {
    use std::collections::BTreeMap;
    let Some(dag) = dag else {
        return 0.0;
    };
    // Group completed task costs by stage_class.
    let mut by_class: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for t in per_task {
        if let Some(sc) = t.stage_class.as_deref() {
            if t.cost_usd > 0.0 {
                by_class.entry(sc.to_string()).or_default().push(t.cost_usd);
            }
        }
    }
    if by_class.is_empty() {
        return 0.0;
    }
    // Median per class.
    let mut class_median: BTreeMap<String, f64> = BTreeMap::new();
    for (k, mut v) in by_class {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = v.len() / 2;
        let m = if v.len() % 2 == 0 {
            (v[mid - 1] + v[mid]) / 2.0
        } else {
            v[mid]
        };
        class_median.insert(k, m);
    }
    // Sum median × count of unfinished tasks in each class. A task is
    // "unfinished" when its state is not `completed`.
    let mut out = 0.0;
    for task in dag.tasks.values() {
        let is_completed = matches!(
            task.state,
            scripps_workflow_core::dag::TaskState::Completed { .. }
        );
        if is_completed {
            continue;
        }
        let sc = task
            .spec
            .as_ref()
            .and_then(|s: &serde_json::Value| s.get("stage_class"))
            .and_then(|v: &serde_json::Value| v.as_str())
            .unwrap_or("");
        if sc.is_empty() {
            continue;
        }
        if let Some(m) = class_median.get(sc) {
            out += *m;
        }
    }
    out
}
