//! The tool-use loop that drives each assistant turn. `send_turn` pushes
//! the user message onto the transcript then calls `run_tool_loop`; this
//! module handles everything in between:
//!
//! - Issuing LLM calls with the streaming backend and a per-call delta sink
//! - Dispatching tool batches against the 1s status-pill threshold timer
//! - Accumulating assistant text + quick_replies + confirmation card across iterations
//! - Building the Anthropic Messages API tool_use / tool_result exchange blocks
//! - Breaking out with an `EndTurn` turn once the model stops calling tools
//!
//! Also hosts `maybe_auto_append`, the pre-loop hook that hydrates a fresh
//! session with the SME's prose so the first iteration starts with a DAG.

use super::retry::{
    backoff_with_jitter, classify_retriable, extract_retry_after_secs, retry_exhausted_turn,
    MAX_RETRIES_PER_TURN,
};
use super::{ConversationService, ServiceError};
use crate::anthropic::client::{context_editing_enabled, CONTEXT_MGMT_KEEP_TOOL_USES};
use crate::anthropic::delta_sink::DeltaSink;
use crate::anthropic::{StopReason, TurnRequest, Usage};
use crate::model_policy::ModelPolicy;
use crate::prompt::build_system_prompt;
use crate::session::{AssistantIntent, ConfirmationCard, Session, SessionState, Turn};
use crate::tool_schemas::{tool_schemas_for_state, tool_status_line};
use crate::tools::{dispatch_batch, BatchableTool, Tool, ToolContext};
use std::sync::Arc;
use std::time::Duration;

/// Pill threshold: only fire ToolCallStarted if the call exceeds this. The
/// service starts a timer per tool call and only emits the event once the
/// timer fires before the call returns.
///
/// Exposed as `pub` so the `documented_constants` integration test can
/// assert the value matches the CLAUDE.md claim.
pub const TOOL_PILL_THRESHOLD: Duration = Duration::from_millis(1000);

/// Tool-loop iteration cap. Latency-baseline fixtures all complete in
/// ≤ 8 iterations. Combined with the soft-landing hint below, this
/// caps pathological-turn cost at ~37% of what an unbounded loop would
/// permit. The iteration counter is also surfaced in metrics
/// (`tool_loop_iterations_per_turn` histogram) so the cap can be
/// re-justified with real data.
pub const TOOL_LOOP_CAP: usize = 10;

/// §3.2 — at or past this iteration count, inject a one-line nudge into
/// the uncached system-prompt suffix so the model prefers to wrap up
/// the turn. Placed below the cacheable prefix so toggling it does not
/// invalidate the cache.
pub const SOFT_LANDING_ITERATION: usize = 7;

/// Iter-9 escalation nudge (Round-2 §3.10). At
/// iter ≥ 9 (one before TOOL_LOOP_CAP=10), shift the prompt from
/// "prefer to wrap up" to "ask the user, don't infer." Without this
/// the loop spent its last iteration making one more speculative
/// tool call; with it the model surfaces the ambiguity to the SME
/// while there's still room for a clean turn boundary.
pub(super) const ESCALATION_ITERATION: usize = 9;

pub(super) const TEMPERATURE: f32 = 0.4;
pub(super) const MAX_TOKENS: u32 = 4096;

/// Client-side mirror of the Anthropic context-management keep window.
/// The server preserves the latest tool-use exchanges; the client sends
/// the same window so the request prefix remains stable across loop
/// iterations and prompt-cache hits stay warm.
pub(super) const KEEP_TOOL_EXCHANGES: usize = CONTEXT_MGMT_KEEP_TOOL_USES as usize;
const TOOL_EXCHANGE_ENTRIES_PER_ITERATION: usize = 2;

/// Trim `tool_exchange` to the latest exchanges before each Anthropic
/// send. Each loop iteration contributes an assistant `tool_use` message
/// and a user `tool_result` message, so the vec window is twice the
/// exchange count. The Anthropic context-editing kill switch also
/// disables this local trim so A/B runs compare like with like.
fn trim_for_beta(tool_exchange: &[serde_json::Value]) -> Vec<serde_json::Value> {
    if !context_editing_enabled() {
        return tool_exchange.to_vec();
    }
    let keep = KEEP_TOOL_EXCHANGES * TOOL_EXCHANGE_ENTRIES_PER_ITERATION;
    if tool_exchange.len() <= keep {
        return tool_exchange.to_vec();
    }
    tool_exchange[tool_exchange.len() - keep..].to_vec()
}

/// §3.7 — default per-session input-token budget. The tool loop halts
/// with a soft-block turn when the running total (input + cache_read +
/// cache_creation summed across the session) exceeds this. Override
/// with `ECAA_SESSION_TOKEN_BUDGET` for testing or power users. Set to
/// 0 to disable. The default is generous enough for multi-branch lotz
/// v1→v5 sessions (~300K tokens with caching) but flags a runaway tool
/// loop before it accrues real cost.
const DEFAULT_SESSION_TOKEN_BUDGET: u64 = 500_000;

fn session_token_budget() -> u64 {
    std::env::var("ECAA_SESSION_TOKEN_BUDGET")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SESSION_TOKEN_BUDGET)
}

/// Substitutes a non-empty placeholder when the LLM produced no text in
/// the iteration that closes the tool loop.
///
/// The Anthropic Messages API rejects any text content block whose
/// `text` is empty (HTTP 400 "messages: text content blocks must be
/// non-empty"). When `last_assistant_text` reaches the exit branches
/// (`no_tool_uses` / `end_turn`) empty — e.g. the model stopped on
/// `end_turn` with neither text nor tool_use — committing a
/// `Turn::assistant("")` to `session.conversation` would cause the
/// NEXT user turn to re-send that empty block, collapsing the request
/// to a 500 backend_error.
///
/// Substituting a placeholder space (and emitting a tracing::warn)
/// preserves the turn structure for downstream consumers (transcript,
/// audit log) while keeping the wire payload Anthropic-valid.
fn non_empty_or_placeholder(text: String, session_id: uuid::Uuid, exit_reason: &str) -> String {
    if text.is_empty() {
        tracing::warn!(
            %session_id,
            %exit_reason,
            "tool_loop exit produced empty assistant text — substituting placeholder"
        );
        " ".to_string()
    } else {
        text
    }
}

/// §3.7 — soft-block turn returned when a session hits its token
/// budget. The LLM sees this the same as any other assistant turn; the
/// UI surfaces `budget_exceeded` via metrics so the SME can decide to
/// branch or start a new session.
fn budget_exceeded_turn(spent: u64, budget: u64) -> Turn {
    let mut t = Turn::assistant(format!(
        "This session has reached its spend ceiling ({spent} / {budget} input tokens). \
         Start a new session to continue, or use Branch From Here to keep the current \
         context while resetting the counter."
    ));
    t.intent = Some(AssistantIntent::Blocker);
    t
}

impl ConversationService {
    /// Pre-hydrate the session with the user's prose so the LLM's first
    /// tool-loop iteration starts with a classified, taxonomied, DAG'd
    /// session instead of an empty one. Only fires on the very first prose
    /// turn of a fresh session — follow-up turns have non-empty
    /// intake_prose and the LLM handles them correctly.
    pub(super) fn maybe_auto_append(&self, session: &mut Session, user_text: &str) {
        if !session.intake_prose.is_empty() {
            return;
        }
        if !matches!(session.state, SessionState::Greeting | SessionState::Intake) {
            return;
        }
        // Handlers no longer drive the state-machine
        // transition themselves; the dispatcher's pre/post hooks own
        // it. This service-side helper bypasses the dispatcher and
        // calls `append_intake_prose` directly, so we fire the
        // AppendProse trigger ourselves to match the dispatcher's
        // pre-handler firing. Illegal-from-state is still tolerated,
        // but logged so transition races are observable in tracing.
        let trigger = crate::session::StateTrigger::AppendProse;
        if let Err(err) = session.try_transition(trigger.clone()) {
            tracing::warn!(
                session_id = %session.id,
                trigger = ?trigger,
                current_state = ?session.state,
                error = ?err,
                "illegal state transition ignored"
            );
        }
        let _ = crate::tools::append_intake_prose(session, user_text, self.config_dir());
    }

    #[tracing::instrument(skip(self, session), fields(session_id = %session.id, state = ?session.state))]
    pub(super) async fn run_tool_loop(
        &self,
        session: &mut Session,
    ) -> Result<(Turn, Usage), ServiceError> {
        // Emit a single tracing event at the start of
        // the tool loop. OTel collectors that subscribe see the
        // session_id + state + careful_mode fields and can
        // correlate via the inherited subscriber context. We
        // deliberately do NOT use `.entered()` because the
        // returned `EnteredSpan` is `!Send` and would poison the
        // async future for axum's Handler trait. `Instrument`
        // would work but the per-iteration sub-spans inside the
        // loop already carry their own context.
        tracing::info!(
            session_id = %session.id,
            state = ?session.state,
            careful_mode = session.careful_mode,
            "tool_loop_start",
        );
        let tool_loop_started_at = std::time::Instant::now();
        let mut iterations: usize = 0;
        let session_id_for_hist = session.id;
        // Baseline tool_call_log length so the committed assistant Turn
        // can carry only the records this tool-loop produced. Without
        // this, `Turn.tool_calls` stays empty and any consumer that
        // walks the transcript history (e.g. the heuristic mock backend
        // that infers prior tool dispatches from `Turn.tool_calls`) sees
        // no record of what fired in earlier turns.
        let baseline_tool_call_log_len = session.tool_call_log.len();
        // §3.7 — snapshot session-wide token spend BEFORE this turn so
        // we can compare `pre_turn + this_turn` against the budget on
        // every iteration.
        let budget = session_token_budget();
        let pre_turn_tokens: u64 = if budget == 0 {
            0
        } else {
            self.metrics()
                .snapshot(session.id)
                .await
                .map(|m| m.total_input_tokens + m.cache_read_tokens + m.cache_creation_tokens)
                .unwrap_or(0)
        };
        // Only the final iteration's assistant text becomes the
        // committed Turn.content. Intermediate iterations' text (e.g.
        // "Looking at the taxonomy...", "Found it, checking state...")
        // is not accumulated for replay in conversation history.
        // Streaming SSE deltas already deliver it to the UI in real time,
        // so dropping it here is invisible to the user and shrinks the
        // long-session replay cost. Regression guard:
        // `service::tests::repeated_tool_calls_resolve_to_acknowledge_*`
        let mut last_assistant_text = String::new();
        let mut accumulated_quick_replies: Vec<String> = Vec::new();
        let mut accumulated_card: Option<ConfirmationCard> = None;
        let mut accumulated_usage = Usage::default();
        // Proper Anthropic Messages API tool exchange: carries tool_use
        // (assistant) and tool_result (user) content blocks between
        // iterations so the model sees its own tool calls and their results.
        let mut tool_exchange: Vec<serde_json::Value> = Vec::new();

        // snapshot the conversation once and share the Arc across
        // every tool-loop iteration. Previous versions did
        // `session.conversation.clone()` per iteration; with a 25-turn
        // session × TOOL_LOOP_CAP iterations that materialized ~hundreds of KB
        // per turn for no benefit (the transcript never mutates mid-loop —
        // the new turn is only committed after the loop completes).
        // `Session.conversation` is already `Arc<Vec<Turn>>` so this is
        // a pointer bump, not a Vec deep clone.
        let conversation_arc = session.conversation.clone();

        loop {
            iterations += 1;
            if iterations > TOOL_LOOP_CAP {
                self.metrics()
                    .record_tool_loop_iterations(session_id_for_hist, TOOL_LOOP_CAP)
                    .await;
                tracing::info!(
                    session_id = %session.id,
                    iterations = TOOL_LOOP_CAP,
                    total_elapsed_ms = tool_loop_started_at.elapsed().as_millis() as u64,
                    "tool_loop_complete reason=cap_exhausted",
                );
                // record the per-turn IntakeFollowup observation so the
                // convergence nudge can fire after 4 consecutive
                // followups. See `Session::note_turn_end_intake_followup`.
                session.note_turn_end_intake_followup();
                return Ok((retry_exhausted_turn(), accumulated_usage));
            }

            // §3.7 — session-wide token budget check. Fires before each
            // API call so a runaway tool loop can't blow past the cap.
            if budget > 0 {
                let turn_tokens = accumulated_usage.input_tokens as u64
                    + accumulated_usage.cache_read_input_tokens as u64
                    + accumulated_usage.cache_creation_input_tokens as u64;
                if pre_turn_tokens.saturating_add(turn_tokens) >= budget {
                    self.metrics()
                        .record_tool_loop_iterations(session_id_for_hist, iterations)
                        .await;
                    tracing::info!(
                        session_id = %session.id,
                        iterations,
                        total_elapsed_ms = tool_loop_started_at.elapsed().as_millis() as u64,
                        spent_tokens = pre_turn_tokens + turn_tokens,
                        budget,
                        "tool_loop_complete reason=budget_exceeded",
                    );
                    // plan — see other exit-point comment in this file.
                    session.note_turn_end_intake_followup();
                    return Ok((
                        budget_exceeded_turn(pre_turn_tokens + turn_tokens, budget),
                        accumulated_usage,
                    ));
                }
            }

            let model = ModelPolicy::choose(session);
            // §3.1 — filter tool schemas by session state. Early-session
            // states get a trimmed vocabulary; Blocked keeps everything.
            let schemas = tool_schemas_for_state(&session.state);
            let mut system_prompt = build_system_prompt(session);
            if iterations >= ESCALATION_ITERATION {
                // At iter ≥ 9 the prompt pivots from
                // "wrap up" to "ask the user, don't infer." Without
                // this escalation the model spent the last iteration
                // making one more speculative tool call; with it the
                // model surfaces ambiguity to the SME via a clean
                // turn boundary.
                system_prompt.push(crate::prompt::SystemPromptBlock {
                    text: format!(
                        "ESCALATION: this turn has used {}/{} tool-call iterations and is \
                         about to hit the cap. Do NOT make another speculative tool call. \
                         Surface the ambiguity to the user with a direct question and end \
                         the turn — the SME can re-engage with the answer next turn.",
                        iterations, TOOL_LOOP_CAP
                    ),
                    cache: false,
                });
            } else if iterations >= SOFT_LANDING_ITERATION {
                // §3.2 soft-landing hint — uncached so it doesn't
                // invalidate the cacheable prefix. The model can see
                // how close it is to the cap and is asked to prefer
                // finalization over further tool calls.
                system_prompt.push(crate::prompt::SystemPromptBlock {
                    text: format!(
                        "TURN BUDGET: this turn has used {}/{} tool-call iterations. \
                         Prefer `propose_summary_confirmation` or end the turn rather \
                         than issuing another tool call unless it's strictly necessary.",
                        iterations, TOOL_LOOP_CAP
                    ),
                    cache: false,
                });
            }
            // First Intake turn force: when the session has just
            // transitioned into Intake from Greeting (no prior
            // classification, no accumulated tool exchanges) and this
            // is the very first tool-loop iteration, force the model
            // to invoke `classify_intake` before composing any
            // user-visible response. Without this guardrail Sonnet
            // drifts to conversational acknowledgment on long technical
            // prompts and fabricates "backend classifier issue"
            // excuses.
            //
            // Only fires on iteration 1 + state=Intake + classification
            // unset + tool_exchange empty. Subsequent iterations and
            // post-classification turns leave `tool_choice` at its
            // default `None` (`auto`) so the model can freely
            // sequence the rest of the intake conversation.
            // Match both `Intake` and `IntakeFollowup`: depending on the
            // session's transition history the LLM-mediated tool_loop
            // can be entered with either state (e.g. a first POST /turn
            // where the user supplied the full prompt + a clarifying
            // request advances Greeting → IntakeFollowup directly). In
            // both cases, an empty `classification` means we haven't
            // run the deterministic classifier yet, and we want to
            // force that before the model composes any user-visible
            // text.
            // Gate on `tool_exchange.is_empty()` rather than
            // `classification.is_none()`: the deterministic server-side
            // `append_intake_prose` already populated
            // `session.classification` before the tool-loop runs, but
            // the **LLM** hasn't observed the classification result
            // yet. The force ensures the model sees the classifier's
            // output via a tool_use → tool_result round-trip on its
            // first iteration, after which it has enough grounding to
            // proceed without drifting to conversational acknowledgment.
            let tool_choice = if iterations == 1
                && matches!(
                    session.state,
                    crate::session::SessionState::Intake
                        | crate::session::SessionState::IntakeFollowup
                )
                && tool_exchange.is_empty()
            {
                tracing::info!(
                    session_id = %session.id,
                    state = ?session.state,
                    "force_classify_intake_on_first_intake_turn"
                );
                Some(crate::anthropic::client::ToolChoice::Tool(
                    "classify_intake".to_string(),
                ))
            } else {
                None
            };

            let request = TurnRequest {
                system_prompt,
                conversation: conversation_arc.clone(),
                tool_schemas: schemas.clone(),
                model,
                temperature: TEMPERATURE,
                max_tokens: MAX_TOKENS,
                tool_exchange: trim_for_beta(&tool_exchange),
                tool_choice,
            };

            // Plan S2.16 — count_tokens preflight when the session is
            // closing on its budget. Anthropic's count_tokens endpoint
            // is free; the cost of one extra HTTP round-trip is
            // negligible compared to the alternative of a 100k-token
            // send_turn that would push the session over its cap.
            //
            // Only fires when:
            // 1. ECAA_BUDGET_HARD_STOP=1 is set (operator opt-in)
            // 2. budget != 0 (env didn't disable budgeting)
            // 3. We're past the warm-up — within 80% of the cap
            //
            // Refuses the turn with ServiceError::Backend("budget…")
            // when the projected post-turn tokens would breach the
            // ceiling. Backends that don't implement count_tokens
            // (mock, older clients) return None and the preflight
            // is a no-op.
            if budget != 0 && ecaa_workflow_core::env_helpers::env_bool("ECAA_BUDGET_HARD_STOP")
            {
                let warm_up_threshold = (budget as f64 * 0.8) as u64;
                let prior_input = pre_turn_tokens + (accumulated_usage.input_tokens as u64);
                if prior_input >= warm_up_threshold {
                    if let Ok(Some(this_turn_estimate)) = self.llm().count_tokens(&request).await {
                        let projected = prior_input + this_turn_estimate as u64;
                        if projected > budget {
                            return Err(ServiceError::Backend(format!(
                                "session token budget exceeded: prior={} + projected={} = {} > budget={}; \
                                 ECAA_BUDGET_HARD_STOP=1 refused the turn pre-flight",
                                prior_input, this_turn_estimate, projected, budget
                            )));
                        }
                    }
                    // count_tokens unsupported (mock, transport error) →
                    // skip the preflight, fall through to the standard
                    // post-turn budget check.
                }
            }

            // R-27 — only the first iteration (the user-visible
            // assistant text) goes through the streaming path so the
            // UI gets `assistant_token_delta` events live. Iterations
            // 2..=TOOL_LOOP_CAP are tool-dispatch round trips: the
            // model is acknowledging tool_results and either calling
            // the next tool or composing the final assistant turn
            // (which surfaces via the EndTurn / no-tool_uses exits
            // below; only the LAST iteration's text becomes
            // `Turn.content`). Streaming SSE for inner iterations
            // costs the long-poll connection budget for no UX gain
            // and is sensitive to mid-turn connection drops; the
            // non-streaming path uses a single HTTP POST that
            // survives transient connection drops via the retry
            // policy below.
            //
            // Mock backends fold streaming into non-streaming via
            // the trait's default impl, so this branch is invisible
            // to fixtures.
            let session_id_for_sink = session.id;
            let sink_for_stream = self.event_sink().clone();
            let on_delta: DeltaSink = Arc::new(move |text: &str| {
                if let Some(sink) = &sink_for_stream {
                    sink.assistant_token_delta(session_id_for_sink, text);
                }
            });
            let use_streaming = iterations == 1;
            // Plan S2.6 — wrap the live Anthropic call in the retry
            // policy. Retriable failures (429, 5xx, transient
            // connection errors) re-fire up to MAX_RETRIES_PER_TURN
            // times with exponential backoff + ±15% jitter; on 429 we
            // honour the server-supplied retry-after when present.
            // Terminal errors (4xx other than 429, parse failures,
            // unexpected response shape) bubble immediately.
            //
            // Each retry charges to the same `chat_cost_usd` bucket as
            // the original call (the metrics layer accumulates across
            // attempts via `accumulated_usage`), so the SME isn't
            // double-billed when a transient blip auto-recovers.
            let iter_started_at = std::time::Instant::now();
            let response = {
                let mut last_err: Option<anyhow::Error> = None;
                let mut response_opt = None;
                for attempt in 0..=MAX_RETRIES_PER_TURN {
                    let req_iter = request.clone();
                    let call_result = if use_streaming {
                        let on_delta_iter = on_delta.clone();
                        self.llm()
                            .send_turn_streaming(req_iter, on_delta_iter)
                            .await
                    } else {
                        self.llm().send_turn(req_iter).await
                    };
                    match call_result {
                        Ok(r) => {
                            response_opt = Some(r);
                            last_err = None;
                            break;
                        }
                        Err(e) => {
                            let msg = e.to_string();
                            if attempt < MAX_RETRIES_PER_TURN && classify_retriable(&msg) {
                                let wait = extract_retry_after_secs(&msg)
                                    .map(std::time::Duration::from_secs)
                                    .unwrap_or_else(|| backoff_with_jitter(attempt));
                                tracing::warn!(
                                    attempt,
                                    backoff_ms = wait.as_millis() as u64,
                                    error = %msg,
                                    "anthropic call retriable; backing off",
                                );
                                tokio::time::sleep(wait).await;
                                last_err = Some(e);
                                continue;
                            }
                            last_err = Some(e);
                            break;
                        }
                    }
                }
                match response_opt {
                    Some(r) => r,
                    None => {
                        let err = last_err.unwrap_or_else(|| {
                            anyhow::anyhow!("anthropic call failed without an explicit error")
                        });
                        // D8 mitigation — log the failure shape + iteration
                        // index + elapsed wall-clock so ops can tell a
                        // request-body-timeout exit from an iter-1 schema
                        // reject without combing through raw error strings.
                        let total_elapsed_ms = tool_loop_started_at.elapsed().as_millis() as u64;
                        let iter_elapsed_ms = iter_started_at.elapsed().as_millis() as u64;
                        let err_str = err.to_string();
                        let timed_out =
                            err_str.contains(crate::anthropic::client::REQUEST_BODY_TIMEOUT_MARKER);
                        tracing::error!(
                            session_id = %session.id,
                            iteration = iterations,
                            iter_elapsed_ms,
                            total_elapsed_ms,
                            timed_out,
                            error = %err_str,
                            "tool_loop_complete reason=backend_error",
                        );
                        return Err(ServiceError::Backend(err_str));
                    }
                }
            };
            // D8 mitigation — per-iteration heartbeat. With only the
            // single `tool_loop_start` log today, ops can't tell whether
            // a slow turn is making progress (each iter completing in a
            // few seconds) or wedged on one stuck Anthropic call. The
            // `anthropic_request_duration_ms` field captures the LLM
            // round-trip including retries; tokens + stop_reason
            // contextualise whether the iter is finalizing or about to
            // dispatch another tool.
            let anthropic_request_duration_ms = iter_started_at.elapsed().as_millis() as u64;
            tracing::info!(
                session_id = %session.id,
                iteration = iterations,
                anthropic_request_duration_ms,
                input_tokens = response.usage.input_tokens,
                output_tokens = response.usage.output_tokens,
                cache_read_input_tokens = response.usage.cache_read_input_tokens,
                cache_creation_input_tokens = response.usage.cache_creation_input_tokens,
                stop_reason = ?response.stop_reason,
                tool_use_count = response.tool_uses.len(),
                "tool_loop_iteration_complete",
            );

            accumulated_usage.input_tokens = accumulated_usage
                .input_tokens
                .saturating_add(response.usage.input_tokens);
            accumulated_usage.output_tokens = accumulated_usage
                .output_tokens
                .saturating_add(response.usage.output_tokens);
            accumulated_usage.cache_read_input_tokens = accumulated_usage
                .cache_read_input_tokens
                .saturating_add(response.usage.cache_read_input_tokens);
            accumulated_usage.cache_creation_input_tokens = accumulated_usage
                .cache_creation_input_tokens
                .saturating_add(response.usage.cache_creation_input_tokens);

            // §3.5 — replace (not append) so the final Turn carries
            // only the last iteration's text. Intermediate iterations'
            // text already streamed to the UI via assistant_token_delta.
            if !response.assistant_content.is_empty() {
                last_assistant_text = response.assistant_content.clone();
            }

            if response.tool_uses.is_empty() {
                let mut turn = Turn::assistant(non_empty_or_placeholder(
                    last_assistant_text,
                    session.id,
                    "no_tool_uses",
                ));
                turn.intent = Some(AssistantIntent::Acknowledge);
                turn.quick_replies = accumulated_quick_replies;
                turn.confirmation_card = accumulated_card;
                // Attach every ToolCallRecord this loop appended so the
                // committed Turn carries a per-turn ledger of what
                // fired. Source-of-truth remains `session.tool_call_log`;
                // this is a per-turn projection for replay-side
                // consumers (e.g. the heuristic mock backend infers
                // prior-turn tool dispatches from this field).
                turn.tool_calls = session
                    .tool_call_log
                    .iter()
                    .skip(baseline_tool_call_log_len)
                    .cloned()
                    .collect();
                self.metrics()
                    .record_tool_loop_iterations(session_id_for_hist, iterations)
                    .await;
                tracing::info!(
                    session_id = %session.id,
                    iterations,
                    total_elapsed_ms = tool_loop_started_at.elapsed().as_millis() as u64,
                    "tool_loop_complete reason=no_tool_uses",
                );
                // see other exit-point comment in this file.
                session.note_turn_end_intake_followup();
                return Ok((turn, accumulated_usage));
            }

            // Dispatch tool batch with a 1s threshold timer. Pill events fire
            // only if dispatch is still running when the timer expires —
            // sub-second calls (which today is everything except disk-bound
            // emit_package) never produce pill flicker.
            let ctx = ToolContext::new(self.config_dir().clone(), model.api_id())
                .with_session_sink(session.id, self.event_sink().clone())
                .with_metrics(self.metrics_store().clone())
                // Thread the store handle so `emit_package` can
                // re-read user_confirmed / proposals fresh at gate time.
                .with_store(self.store_handle());
            let tool_uses_for_dispatch = response.tool_uses.clone();
            let session_id = session.id;
            let event_sink = self.event_sink().clone();
            let pill_tools: Vec<(String, &'static str)> = response
                .tool_uses
                .iter()
                .map(|(_, t)| (t.name().to_string(), tool_status_line(t)))
                .collect();

            let mut dispatched = {
                let dispatch_fut = dispatch_batch(tool_uses_for_dispatch, session, &ctx);
                tokio::pin!(dispatch_fut);
                let timer = tokio::time::sleep(TOOL_PILL_THRESHOLD);
                tokio::pin!(timer);
                let mut started_fired = false;
                let result;
                loop {
                    tokio::select! {
                        biased;
                        _ = &mut timer, if !started_fired => {
                            if let Some(sink) = &event_sink {
                                for (name, status) in &pill_tools {
                                    sink.tool_call_started(session_id, name, status);
                                }
                            }
                            started_fired = true;
                        }
                        out = &mut dispatch_fut => {
                            if started_fired {
                                if let Some(sink) = &event_sink {
                                    for (name, _) in &pill_tools {
                                        sink.tool_call_finished(session_id, name);
                                    }
                                }
                            }
                            result = out;
                            break;
                        }
                    }
                }
                result
            };

            // The `state_advanced` event fires from `send_turn` AFTER
            // the merge `store.update` completes, not from the tool-loop's
            // local session clone. Firing earlier would let a subscriber
            // refetch /state and see the stale persisted state (the
            // transition would only exist in the clone) with the merged
            // state arriving milliseconds later — a transient flicker
            // visible in BlockerCard. See `send_turn.rs::send_turn`
            // post-merge for the canonical fire site.
            // Touch _dispatched to silence unused mut warning when no further
            // mutation happens (it's read below).
            let _ = &mut dispatched;

            // Surface any quick-reply / confirmation results to the final turn.
            for (i, (_, result)) in dispatched.iter().enumerate() {
                let (_, tool) = &response.tool_uses[i];
                if !result.is_error {
                    if let Tool::Batchable(BatchableTool::ProposeQuickReplies { options, .. }) =
                        tool
                    {
                        accumulated_quick_replies = options.clone();
                    }
                    if let Tool::Batchable(BatchableTool::ProposeSummaryConfirmation {
                        summary_markdown,
                    }) = tool
                    {
                        // Fingerprint the summary text
                        // so the SME-visible card and the durable audit
                        // record can be matched after the fact. Uses
                        // SHA-256 over the UTF-8 bytes of the markdown
                        // exactly as the LLM emitted it (no trim, no
                        // normalization) so a single-character delta in
                        // the text produces a different fingerprint.
                        let summary_hash =
                            crate::tools::conversational::summary_hash_of(summary_markdown);
                        accumulated_card = Some(ConfirmationCard {
                            summary_markdown: summary_markdown.clone(),
                            summary_hash,
                            resource_estimate: None,
                        });
                    }
                }
            }

            // Build proper Anthropic Messages API tool_use / tool_result
            // exchange so the model sees its own tool calls and their
            // results on the next iteration.
            {
                // Assistant message with tool_use content blocks.
                let mut assistant_content: Vec<serde_json::Value> = Vec::new();
                if !response.assistant_content.is_empty() {
                    assistant_content.push(serde_json::json!({
                        "type": "text",
                        "text": response.assistant_content,
                    }));
                }
                for (id, tool) in &response.tool_uses {
                    // Failure here is "should never happen" for our closed
                    // tool vocabulary, but a silent unwrap_or_default() would
                    // ship a `null` `input` block to Anthropic and the model
                    // would respond as though we asked nothing — masking the
                    // real bug. Surface it as a typed ServiceError so the
                    // turn fails fast and bubbles up to InfraErrorBanner.
                    let input = serde_json::to_value(tool).map_err(|e| {
                        ServiceError::Internal(format!(
                            "tool_use serialization failed for `{}`: {}",
                            tool.name(),
                            e
                        ))
                    })?;
                    assistant_content.push(serde_json::json!({
                        "type": "tool_use",
                        "id": id.to_string(),
                        "name": tool.name(),
                        "input": input,
                    }));
                }
                tool_exchange.push(serde_json::json!({
                    "role": "assistant",
                    "content": assistant_content,
                }));

                // User message with tool_result content blocks. §3.16 —
                // homogeneous tabular payloads serialize as CSV instead
                // of JSON; other shapes fall through to JSON unchanged.
                let mut result_content: Vec<serde_json::Value> = Vec::new();
                for (i, (_, result)) in dispatched.iter().enumerate() {
                    let (id, _) = &response.tool_uses[i];
                    let compacted = crate::tools::compact_tabular(&result.content);
                    // Plan S2.9 — soft cap so a runaway tool can't
                    // burn the per-turn token budget. Capped content
                    // gets a truncation marker; full payload is
                    // preserved in result.content for the audit log.
                    let capped = crate::tools::cap_tool_result_length(compacted);
                    result_content.push(serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": id.to_string(),
                        "content": capped,
                        "is_error": result.is_error,
                    }));
                }
                tool_exchange.push(serde_json::json!({
                    "role": "user",
                    "content": result_content,
                }));
            }

            if response.stop_reason == StopReason::EndTurn {
                let mut turn = Turn::assistant(non_empty_or_placeholder(
                    last_assistant_text,
                    session.id,
                    "end_turn",
                ));
                turn.intent = Some(AssistantIntent::Acknowledge);
                turn.quick_replies = accumulated_quick_replies;
                turn.confirmation_card = accumulated_card;
                // Attach every ToolCallRecord this loop appended so the
                // committed Turn carries a per-turn ledger of what
                // fired. Source-of-truth remains `session.tool_call_log`;
                // this is a per-turn projection for replay-side
                // consumers (e.g. the heuristic mock backend infers
                // prior-turn tool dispatches from this field).
                turn.tool_calls = session
                    .tool_call_log
                    .iter()
                    .skip(baseline_tool_call_log_len)
                    .cloned()
                    .collect();
                self.metrics()
                    .record_tool_loop_iterations(session_id_for_hist, iterations)
                    .await;
                tracing::info!(
                    session_id = %session.id,
                    iterations,
                    total_elapsed_ms = tool_loop_started_at.elapsed().as_millis() as u64,
                    "tool_loop_complete reason=end_turn",
                );
                // see other exit-point comment in this file.
                session.note_turn_end_intake_followup();
                return Ok((turn, accumulated_usage));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(unsafe_code)]

    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_mutex() -> &'static Mutex<()> {
        static M: OnceLock<Mutex<()>> = OnceLock::new();
        M.get_or_init(|| Mutex::new(()))
    }

    fn make_exchange(iter_idx: usize) -> [serde_json::Value; 2] {
        [
            serde_json::json!({
                "role": "assistant",
                "iter_idx": iter_idx,
                "content": [{
                    "type": "tool_use",
                    "id": format!("toolu_{iter_idx}"),
                    "name": "get_session_state",
                    "input": {}
                }]
            }),
            serde_json::json!({
                "role": "user",
                "iter_idx": iter_idx,
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": format!("toolu_{iter_idx}"),
                    "content": format!("result {iter_idx}"),
                    "is_error": false
                }]
            }),
        ]
    }

    fn build_exchanges(n: usize) -> Vec<serde_json::Value> {
        let mut v = Vec::with_capacity(n * TOOL_EXCHANGE_ENTRIES_PER_ITERATION);
        for i in 0..n {
            v.extend_from_slice(&make_exchange(i));
        }
        v
    }

    #[test]
    fn trim_for_beta_drops_oldest_to_match_keep_window() {
        let _guard = env_mutex().lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var("ECAA_DISABLE_CONTEXT_EDITING") };

        let exchanges = build_exchanges(10);
        assert_eq!(exchanges.len(), 20);
        let trimmed = trim_for_beta(&exchanges);

        assert_eq!(
            trimmed.len(),
            KEEP_TOOL_EXCHANGES * TOOL_EXCHANGE_ENTRIES_PER_ITERATION
        );
        assert_eq!(trimmed[0]["iter_idx"], 6);
        assert_eq!(trimmed[0]["role"], "assistant");
        assert_eq!(trimmed[1]["iter_idx"], 6);
        assert_eq!(trimmed[1]["role"], "user");
        assert_eq!(trimmed[7]["iter_idx"], 9);
        assert_eq!(trimmed[7]["role"], "user");
    }

    #[test]
    fn trim_for_beta_is_noop_under_keep_window() {
        let _guard = env_mutex().lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var("ECAA_DISABLE_CONTEXT_EDITING") };

        let at_cap = build_exchanges(KEEP_TOOL_EXCHANGES);
        let trimmed = trim_for_beta(&at_cap);
        assert_eq!(
            trimmed.len(),
            KEEP_TOOL_EXCHANGES * TOOL_EXCHANGE_ENTRIES_PER_ITERATION
        );
        assert_eq!(trimmed[0]["iter_idx"], 0);

        let small = build_exchanges(2);
        let trimmed = trim_for_beta(&small);
        assert_eq!(trimmed.len(), 4);
        assert_eq!(trimmed[0]["iter_idx"], 0);
    }

    #[test]
    fn trim_for_beta_disabled_by_env_returns_input_unchanged() {
        let _guard = env_mutex().lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::set_var("ECAA_DISABLE_CONTEXT_EDITING", "1") };

        let exchanges = build_exchanges(10);
        let trimmed = trim_for_beta(&exchanges);
        assert_eq!(trimmed.len(), exchanges.len());
        for (a, b) in exchanges.iter().zip(trimmed.iter()) {
            assert_eq!(a, b);
        }

        unsafe { std::env::remove_var("ECAA_DISABLE_CONTEXT_EDITING") };
    }

    #[test]
    fn trim_for_beta_empty_vec_is_safe() {
        let _guard = env_mutex().lock().unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var("ECAA_DISABLE_CONTEXT_EDITING") };

        let empty: Vec<serde_json::Value> = vec![];
        let trimmed = trim_for_beta(&empty);
        assert!(trimmed.is_empty());
    }
}
