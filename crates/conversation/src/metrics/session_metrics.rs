//! Public `SessionMetrics` struct — the snapshot shape the conversation
//! service serializes for `/api/chat/session/:id/metrics`.
//!
//! Pure data shape (`Serialize` only — produced by
//! `SessionCounters::snapshot`, never deserialised back). All cost-bucket
//! and per-surface invariants are documented inline on the fields.

use super::counters::{
    AffordanceFallbackSummary, BlockerEventRecord, PerSurfaceTokens, PerTaskAgentSnapshot,
};
use serde::Serialize;
use std::collections::BTreeMap;

// NOTE: Intentionally NOT ts-rs-derived. u64 fields would render as
// `bigint` in TS, breaking arithmetic across the UI's MetricsTable.
// The hand-maintained mirror in ui/src/api/chatClient.ts uses `number`
// for the same fields with the (accepted) precision-loss caveat past 2^53.

/// Snapshot of per-session conversation metrics served at
/// `/api/chat/session/:id/metrics`. Serialized by `SessionCounters::snapshot`.
#[derive(Debug, Clone, Serialize)]
pub struct SessionMetrics {
    /// Total number of conversation turns completed.
    pub turn_count: u64,
    /// Total number of tool calls dispatched.
    pub tool_call_count: u64,
    /// Total input tokens billed (excludes cache reads).
    pub total_input_tokens: u64,
    /// Total output tokens generated.
    pub total_output_tokens: u64,
    /// Input tokens served from prompt cache.
    pub cache_read_tokens: u64,
    /// Input tokens written into prompt cache.
    pub cache_creation_tokens: u64,
    /// Median turn latency in milliseconds.
    pub p50_turn_ms: u64,
    /// 95th-percentile turn latency in milliseconds.
    pub p95_turn_ms: u64,
    /// 99th-percentile turn latency in milliseconds.
    pub p99_turn_ms: u64,
    /// Mean turn latency in milliseconds.
    pub mean_turn_ms: u64,
    /// Maximum observed turn latency in milliseconds.
    pub max_turn_ms: u64,
    /// Kept for UI backward compatibility. Equal to
    /// `per_model_turns["sonnet_4_6"]` when present; zero otherwise.
    pub sonnet_turns: u64,
    /// Kept for UI backward compatibility. Equal to
    /// `per_model_turns["opus_4_6"]` when present; zero otherwise.
    pub opus_turns: u64,
    /// Per-model turn count. Keyed by the `ModelId` serde name
    /// (`sonnet_4_6` / `opus_4_6` / `haiku_4_5` / future variants) so
    /// the UI can render arbitrary models without a Rust change. Absent
    /// entries are never written — a zero turn-count model is simply
    /// not in the map.
    pub per_model_turns: BTreeMap<String, u64>,
    /// Per-model estimated cost in USD. Same keying as
    /// `per_model_turns`. Sums to `total_cost_usd`.
    pub per_model_cost_usd: BTreeMap<String, f64>,
    /// total instance-seconds (sum over all remote
    /// executions). The UI surfaces this as a floor(seconds/3600) hour
    /// count + the nearest quarter-hour.
    pub total_instance_seconds: u64,
    /// Per-instance-type breakdown of the same counter. Surfaced as a
    /// stacked-bar distribution in the Metrics tab.
    pub instance_type_seconds: BTreeMap<String, u64>,
    /// count of tasks where the realized
    /// compute exceeded the high-water baseline (resize events).
    pub high_water_exceeded_count: u64,
    /// number of harness events the batcher dropped (oldest-first)
    /// when this session's in-window queue hit its cap. Non-zero means
    /// the agent is bursting progress faster than the 10s flush window
    /// can drain; surface in the Metrics tab as an amber warning.
    pub batch_dropped_events: u64,
    /// Estimated Anthropic API spend for this session in USD, summed
    /// across every chat turn + every agent task + every operator-triggered
    /// rubric-scorer call + every side-call (e.g. Haiku-powered session
    /// auto-title) using the `pricing` table.
    /// `chat_cost_usd + agent_cost_usd + scorer_cost_usd + side_call_cost_usd == total_cost_usd`.
    pub total_cost_usd: f64,
    /// Chat-only spend: turns driven by `ConversationService::send_turn`.
    /// Matches pre-Bundle-D `total_cost_usd` semantics for sessions that
    /// don't have agent instrumentation wired up.
    pub chat_cost_usd: f64,
    /// Agent-only spend: LLM usage recorded via
    /// `MetricsStore::record_agent_usage` from harness progress events
    /// carrying an `agent_usage` block. Zero when the agent script
    /// hasn't been updated to emit `runtime/outputs/<task_id>/agent-usage.json`.
    pub agent_cost_usd: f64,
    /// Per-model agent spend breakdown. Same key shape as
    /// `per_model_cost_usd`, but scoped to agent task executions only.
    pub per_model_agent_cost_usd: BTreeMap<String, f64>,
    /// Scorer-only spend: LLM usage recorded via
    /// `MetricsStore::record_scorer_usage` when an operator triggers
    /// `POST /api/chat/session/:id/score`. Zero on sessions that have
    /// never been scored.
    pub scorer_cost_usd: f64,
    /// Per-model scorer spend breakdown. Same key shape as
    /// `per_model_cost_usd`. In practice only `sonnet_4_6` appears
    /// (the scorer pins Sonnet at the call site), but the map shape
    /// matches the other cost buckets for symmetry.
    pub per_model_scorer_cost_usd: BTreeMap<String, f64>,
    /// Side-call spend: cheap, read-only LLM hops routed through
    /// `ModelPolicy::for_side_call()` (Haiku 4.5) that don't
    /// participate in the main conversation's prompt cache. First
    /// caller is session auto-titling; future subagents routed through
    /// `ModelPolicy::for_side_call` flow into the same bucket so the
    /// "cheap side-call" spend stays visibly distinct from chat /
    /// agent / scorer rows.
    #[serde(default)]
    pub side_call_cost_usd: f64,
    /// Per-model side-call spend breakdown. Same key shape as
    /// `per_model_cost_usd`. Usually only `haiku_4_5` (side calls pin
    /// Haiku at the call site), but the map shape matches the other
    /// cost buckets for symmetry and future flexibility.
    #[serde(default)]
    pub per_model_side_call_cost_usd: BTreeMap<String, f64>,
    /// Kept for UI backward compatibility. Equal to
    /// `per_model_cost_usd["sonnet_4_6"]` when present; zero otherwise.
    pub sonnet_cost_usd: f64,
    /// Kept for UI backward compatibility. Equal to
    /// `per_model_cost_usd["opus_4_6"]` when present; zero otherwise.
    pub opus_cost_usd: f64,
    /// Fraction of billed input tokens served from Anthropic's prompt
    /// cache: `cache_read / (input + cache_read + cache_creation)`. A
    /// healthy value for interactive chat is ≥ 0.6 once a session has
    /// warmed up; values near 0 on a multi-turn session signal silent
    /// cache invalidation (the #1 prompt-caching failure mode per
    /// Anthropic's docs — typically caused by mutable field order,
    /// timestamps, or HashMap serialization in the cacheable prefix).
    /// Zero when no input tokens have been recorded yet. Surfaced in
    /// the UI Performance tab.
    pub cache_hit_ratio: f64,
    /// §3.2/§4.3 — per-turn tool-loop iteration-count histogram. Keyed
    /// by the iteration count used (1..=TOOL_LOOP_CAP), value is the
    /// number of turns that used exactly that many iterations. Empty
    /// until at least one turn has completed.
    pub tool_loop_iterations_histogram: BTreeMap<u32, u64>,
    /// §3.3/§4.2 — per-reason Opus escalation count. Keyed by the
    /// serde name of `model_policy::EscalationReason` (`careful_mode`,
    /// `blocked`, `low_confidence`). Absent reasons are simply not in
    /// the map. Lets operators see which trigger dominates Opus spend.
    pub opus_escalation_reasons: BTreeMap<String, u64>,
    /// §S7.11 — composer performance counters. The composer is
    /// synchronous and runs at most once per session-confirmation.
    /// `composer_runs` counts how often `Composer::compose` returned
    /// (success or typed `CompositionError`); the four other counters
    /// are aggregate sums across runs. Performance budget per the
    /// plan: p99 < 500ms at ≤ 200 atoms. Operators read these from
    /// the Performance tab to see when the backward-chain path is
    /// climbing.
    #[serde(default)]
    pub composer_runs: u64,
    /// Total wall-clock milliseconds spent in `Composer::compose` across all runs.
    #[serde(default)]
    pub composer_total_duration_ms: u64,
    /// Total atom nodes evaluated across all composer runs.
    #[serde(default)]
    pub composer_atoms_considered: u64,
    /// Total backward-chain backtracks across all composer runs.
    #[serde(default)]
    pub composer_backtracks: u64,
    /// Total exclusion-rule hits (atoms skipped by exclusion policy) across all runs.
    #[serde(default)]
    pub composer_exclusion_hits: u64,
    /// §4.4 — configured session-wide input-token ceiling, read from
    /// `SWFC_SESSION_TOKEN_BUDGET` (default 500_000) at snapshot time.
    /// `None` when the budget is disabled (`SWFC_SESSION_TOKEN_BUDGET=0`),
    /// `Some(n)` otherwise. The UI renders a progress bar of
    /// `total_input_tokens` against this value when it's set; `None`
    /// suppresses the row entirely. The ceiling is only counted against
    /// the uncached remainder (`total_input_tokens`), not cache reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token_budget: Option<u64>,
    /// `SWFC_AGENT_BILLING` at snapshot time — either "subscription"
    /// (default; per-task agent runs bill the Max/Pro plan via
    /// `~/.claude/.credentials.json`, real Anthropic API charge = $0)
    /// or "api" (forwarded `ANTHROPIC_API_KEY`, real per-token charge).
    /// Under `subscription`, `agent_cost_usd` is *notional* — the Claude
    /// Code CLI's self-reported `total_cost_usd` representing what the
    /// tokens would have cost on the API. The UI uses this to label
    /// the agent-cost row "notional" so operators aren't alarmed by
    /// the nominal figure on subscription-backed runs.
    #[serde(default)]
    pub agent_billing_mode: String,
    /// Per-surface token split. Each of the four cost surfaces (chat,
    /// agent, scorer, side_call) carries its own input / output /
    /// cache_read / cache_creation token sub-totals. The grand totals
    /// live on the flat `total_input_tokens` / `total_output_tokens` /
    /// `cache_read_tokens` / `cache_creation_tokens` fields above — the
    /// surface buckets always sum to those. `#[serde(default)]` for
    /// backward compat with pre-F6 sidecars.
    #[serde(default)]
    pub per_surface_tokens: PerSurfaceTokens,
    /// F1 — per-task agent cost breakdown. One entry per task id that
    /// the harness reported usage for, sorted by `cost_usd` descending
    /// at serialize time so the UI doesn't have to. Empty for sessions
    /// that haven't run any agent tasks yet (e.g. pre-emit chat) or
    /// pre-F1 sidecars. Filed under "any task an agent ran" — does NOT
    /// include chat / scorer / side-call activity (those have their own
    /// per_model rollups).
    #[serde(default)]
    pub per_task_agent: Vec<PerTaskAgentSnapshot>,
    /// F2 — per-tool-name call counter. Bucketed by `Tool::name()` at
    /// the service-layer dispatch boundary. Empty for sessions where
    /// no tool has been dispatched yet. Lets operators see e.g. "the
    /// LLM called `classify_intake` 6 times this session" so runaway
    /// tool loops surface visibly. Note: read-only tools (which run
    /// sub-millisecond) and mutation tools both count here — the
    /// existing aggregate `tool_call_count` is the sum.
    #[serde(default)]
    pub tool_calls_by_name: BTreeMap<String, u64>,
    /// F3 — wall-clock seconds since `Session::created_at`. Populated
    /// by the service-layer call to `metrics_snapshot` which has
    /// access to the Session. Bare `MetricsStore::snapshot` calls
    /// (test-only path) leave this at zero. The UI derives per-hour
    /// burn rates ($/hr, tokens/hr) from this once it exceeds 60s.
    #[serde(default)]
    pub session_duration_seconds: u64,
    /// F5 — total dollar value saved by Anthropic's prompt cache.
    /// Computed as `cache_read_tokens × (input_rate − cache_read_rate)`
    /// per (surface, model) pair, summed across all four surfaces.
    /// Zero when no cache reads have been recorded. Surfaced in the UI
    /// next to `cache_hit_ratio` so operators see the dollar value of
    /// caching at a glance.
    #[serde(default)]
    pub cache_savings_usd: f64,
    /// F5 — same as `cache_savings_usd` but split by cost surface
    /// (chat / agent / scorer / side_call). Only nonzero entries are
    /// emitted. Empty for sessions with no cache reads.
    #[serde(default)]
    pub per_surface_cache_savings_usd: BTreeMap<String, f64>,
    /// Session-level soft budget cap in USD. None when no cap is set.
    /// Set via `POST /api/chat/session/:id/budget` or seeded at session
    /// creation from `SWFC_DEFAULT_SESSION_BUDGET_USD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
    /// Fraction of the budget consumed by `total_cost_usd`. None when
    /// budget_usd is None. 0.0..=1.0 (may exceed 1.0 when over budget).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_used_pct: Option<f64>,
    /// Discrete budget state. "ok" below 75%, "warn" 75%–100%,
    /// "exceeded" above 100%. None when no cap is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_state: Option<String>,
    /// Estimated cost to complete the remaining DAG tasks, derived
    /// from per-stage-class median cost × number of unfinished tasks
    /// in that class. Zero when there are no prior completions with a
    /// stage_class we can use as a basis.
    #[serde(default)]
    pub projected_remaining_usd: f64,
    /// Sum of `total_cost_usd + projected_remaining_usd`. UI uses this
    /// alongside `budget_usd` to render "on track / projected over
    /// budget" chips.
    #[serde(default)]
    pub projected_finish_usd: f64,
    /// Catalog-gap telemetry. One entry per distinct
    /// `(semantic_type, primitive)` pair that the affordance resolver
    /// fell back to a structural primitive for during this session.
    /// Sorted by count descending, then `semantic_type` ascending, then
    /// `primitive` ascending (mirrors
    /// `AffordanceFallbackCounter::all_gaps_sorted_by_count_desc`).
    ///
    /// `#[serde(default)]` for backward compat with sessions persisted
    /// before this field was added — they deserialize with an empty list and
    /// the UI catalog-gaps card stays hidden until real data accumulates.
    #[serde(default)]
    pub affordance_fallbacks: Vec<AffordanceFallbackSummary>,

    // ---------------------------------------------------------------
    // SME-experience fields for Tier 16.2–16.4 eval runners.
    // These are new in this schema version. All carry `#[serde(default)]`
    // so existing `SessionMetrics` consumers (UI, scorer, eval-adapters)
    // continue to work unchanged — they get 0 / empty-vec / None for
    // sessions emitted before this change.
    // ---------------------------------------------------------------
    /// Number of IntakeFollowup turns before `PendingConfirmation` was
    /// proposed. Tier 16.2 uses this as the "clarification rounds" proxy
    /// for unambiguous sessions.
    ///
    /// Incremented by `MetricsStore::record_intake_followup_turn` which
    /// the service layer calls on every turn completed while the session
    /// is in `IntakeFollowup` state.
    #[serde(default)]
    pub followup_count: u32,

    /// Number of post-emission `amend_stage_method` calls. Tier 16.3
    /// reads this as the "amendments to converge" signal.
    ///
    /// Incremented by `MetricsStore::record_amendment` which the service
    /// layer calls on every successful `Emitted → Amending` transition.
    #[serde(default)]
    pub amendment_count: u32,

    /// Append-only log of every `Blocked` state entry with its
    /// recovery outcome. Tier 16.4 reads this to compute
    /// blocker-recovery rate and mean-time-to-recover.
    ///
    /// Each entry is appended by `MetricsStore::record_blocker_entered`
    /// on every `Blocked` transition; `MetricsStore::record_blocker_recovered`
    /// flips the last unrecovered entry's `recovered` flag to `true` on
    /// `/unblock`.
    #[serde(default)]
    pub blockers_encountered: Vec<BlockerEventRecord>,

    /// Whether the session's intake was classified as "ambiguous" at
    /// the time of emission. `None` = unknown (no classification ran
    /// Before emit, or the session was emitted before the field existed);
    /// `Some(true)` = at least one `ClassificationResult.confidence <
    /// 0.6` was recorded; `Some(false)` = all confidence values ≥ 0.6.
    ///
    /// Set by `MetricsStore::record_classification_confidence` from
    /// `classify_intake` / `append_intake_prose` tool dispatch.
    #[serde(default)]
    pub is_ambiguous: Option<bool>,
}
