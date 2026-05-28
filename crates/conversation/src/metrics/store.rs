//! Thread-safe per-session telemetry storage. Houses the internal
//! `SessionCounters` shape (sidecar-format, with the legacy-Bundle-C
//! `Deserialize` migration baked in) and the `MetricsStore` wrapper that
//! the conversation service layer hands to chat routes / harness events
//! / scorer endpoints.
//!
//! The store is the only mutable surface in the metrics module — all
//! cost-bucket invariants documented in `crate::metrics` are enforced
//! here, at the call sites of `record_turn` / `record_agent_usage` /
//! `record_scorer_usage` / `record_side_call_usage`. The snapshot path
//! (`SessionCounters::snapshot`) is pure: it reads counters in, returns
//! a `SessionMetrics`, never touches the store.

use super::counters::{
    BlockerEventRecord, ModelCounters, PerSurfaceTokens, PerTaskAgentCounters,
    PerTaskAgentSnapshot, ITERATIONS_HISTOGRAM_BUCKETS,
};
use super::pricing;
use super::session_metrics::SessionMetrics;
use crate::model_policy::ModelId;
use crate::session::SessionId;
use scripps_workflow_core::cost::Cost;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, RwLock};

#[derive(Debug, Default, Clone, Serialize)]
pub(crate) struct SessionCounters {
    pub(crate) turn_count: u64,
    pub(crate) tool_call_count: u64,
    pub(crate) turn_durations_ms: Vec<u64>,
    // Per-model counters keyed by ModelId. Serialized with the enum's
    // snake_case representation so sidecars stay readable. Legacy
    // sidecars carrying flat `sonnet_*` / `opus_*` fields are translated
    // into this map by the custom Deserialize impl below.
    pub(crate) per_model: BTreeMap<ModelId, ModelCounters>,
    // Bundle D: agent-side LLM usage recorded separately from chat
    // turns. The server calls `record_agent_usage` when a harness
    // progress event carries an `agent_usage` block. Keyed by ModelId
    // same as `per_model`. Absent when the agent script hasn't been
    // updated to emit `runtime/outputs/<task_id>/agent-usage.json`.
    #[serde(default)]
    pub(crate) agent_per_model: BTreeMap<ModelId, ModelCounters>,
    // Scorer-side LLM usage recorded via `record_scorer_usage` when an
    // operator triggers `POST /api/chat/session/:id/score`. Same keying
    // as `per_model` / `agent_per_model`. Scorer pins Sonnet 4.6 at
    // the call site so in practice only that key is present, but the
    // map shape matches the other buckets for symmetry.
    #[serde(default)]
    pub(crate) scorer_per_model: BTreeMap<ModelId, ModelCounters>,
    // Side-call LLM usage recorded via `record_side_call_usage`. Side
    // calls are cheap, read-only, model-independent LLM hops that don't
    // participate in the main conversation's prompt cache (session
    // auto-title via Haiku 4.5 is the first caller). Bucketed
    // separately so the scorer and chat rows stay interpretable; sums
    // into `total_cost_usd` via `SessionCounters::snapshot`.
    #[serde(default)]
    pub(crate) side_call_per_model: BTreeMap<ModelId, ModelCounters>,
    /// F1 — per-task agent counters keyed by task_id. Populated by
    /// `record_agent_usage` when the harness forwards an `agent_usage`
    /// event with a task_id. Each entry carries the model the agent ran
    /// plus the four token sub-totals. The snapshot folds this into a
    /// sorted `per_task_agent` list for the UI; it also aggregates into
    /// `agent_per_model` so the existing `agent_cost_usd` /
    /// `per_model_agent` rollups stay correct.
    #[serde(default)]
    pub(crate) per_task_agent: BTreeMap<String, PerTaskAgentCounters>,
    /// F2 — per-tool-name call counters. Bucketed at the service-layer
    /// tool boundary in `service/tool_loop.rs` via
    /// `MetricsStore::record_tool_call`. Empty until at least one tool
    /// has been dispatched in this session. Keyed by `Tool::name()`
    /// (e.g. `append_intake_prose`, `emit_package`).
    #[serde(default)]
    pub(crate) tool_calls_by_name: BTreeMap<String, u64>,
    // Compute-usage counters populated
    // from harness progress events that carry a remote executor. Keyed
    // by EC2 instance type so the operator dashboard can surface the
    // type-distribution alongside absolute instance-hours.
    pub(crate) instance_seconds: BTreeMap<String, u64>,
    // Per-task start timestamps (monotonic ms-since-epoch) so
    // instance_seconds can accumulate on task_completed.
    pub(crate) running_task_starts: BTreeMap<String, (String, u64)>,
    // separate counter so the UI can warn when
    // the realized shape exceeded the high-water baseline.
    pub(crate) high_water_exceeded_count: u64,
    // count of harness events dropped by the batcher when a
    // session's per-window queue hits HARNESS_BATCH_MAX_EVENTS. Silent
    // drop-oldest is acceptable (the batcher is already a lossy 10s
    // summary) but the operator needs to see when it's happening.
    pub(crate) batch_dropped_events: u64,
    // Per-turn tool-loop iteration count histogram. Keyed by the
    // iteration count used (1..=TOOL_LOOP_CAP), value is how many turns
    // used exactly that many iterations. Used to empirically justify
    // TOOL_LOOP_CAP — if no turns reach the cap, we can lower it
    // further; if many do, we need to revisit.
    #[serde(default)]
    pub(crate) tool_loop_iterations_histogram: BTreeMap<u32, u64>,
    // Per-reason Opus escalation count. Keyed by the serde name of
    // `model_policy::EscalationReason` so adding a new reason is
    // additive.
    #[serde(default)]
    pub(crate) opus_escalation_reasons: BTreeMap<String, u64>,
    // §S7.11 — composer performance counters. The composer is
    // synchronous and runs at most once per session-confirmation, so
    // we collect aggregate counters rather than per-run histograms.
    // Operators read these from the Performance tab to see when the
    // backward-chain path is climbing toward the planned p99 < 500ms
    // budget at ≤ 200 atoms.
    #[serde(default)]
    pub(crate) composer_runs: u64,
    #[serde(default)]
    pub(crate) composer_total_duration_ms: u64,
    #[serde(default)]
    pub(crate) composer_atoms_considered: u64,
    #[serde(default)]
    pub(crate) composer_backtracks: u64,
    #[serde(default)]
    pub(crate) composer_exclusion_hits: u64,

    // SME-experience counters for the Tier 16.2–16.4 eval
    // runners. Populated by the service-layer state-transition hooks.
    //
    // `followup_count` — number of turns recorded while the session was
    // in `IntakeFollowup` state, counting how many clarification rounds
    // the SME needed before `PendingConfirmation` was proposed.
    //
    // `amendment_count` — number of times the session entered
    // `Amending{}` (i.e., `amend_stage_method` was called post-emit).
    //
    // `blockers_encountered` — append-only log of every `Blocked` entry
    // with recovery outcome; used by Tier 16.4.
    //
    // `is_ambiguous` — `None` until the classifier runs; `Some(true)`
    // when any `ClassificationResult.confidence < 0.6` was recorded for
    // this session, `Some(false)` otherwise.
    #[serde(default)]
    pub(crate) followup_count: u32,
    #[serde(default)]
    pub(crate) amendment_count: u32,
    #[serde(default)]
    pub(crate) blockers_encountered: Vec<BlockerEventRecord>,
    #[serde(default)]
    pub(crate) is_ambiguous: Option<bool>,

    // Iteration telemetry. Populated by the
    // `record_iteration_*` setters when a `StageCardinality::IterateUntil`
    // atom converges, hits max_iterations, or runs an iteration.
    // The Performance tab's Iteration row reads these for the SME
    // dashboard.
    #[serde(default)]
    pub(crate) iterations_run_total: u64,
    #[serde(default)]
    /// Histogram of `iter_count` at convergence. Bucketed at 1, 2,
    /// 4, 8, 16, 32, 64+ — matches the typical k8s-reconciler
    /// distribution where most loops converge fast and the tail
    /// is rare. 7-bucket count matches `ITERATIONS_HISTOGRAM_BUCKETS`.
    pub(crate) converged_at_iter: Vec<u64>,
    #[serde(default)]
    pub(crate) max_iterations_hit_count: u64,
    #[serde(default)]
    pub(crate) iteration_total_duration_ms: u64,
}

// Custom deserialize that accepts BOTH the new `per_model` shape AND the
// legacy flat `sonnet_*` / `opus_*` fields written by pre-Bundle-C
// clients. Sidecar files survive a rolling upgrade.
impl<'de> Deserialize<'de> for SessionCounters {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Raw {
            turn_count: u64,
            tool_call_count: u64,
            turn_durations_ms: Vec<u64>,
            #[serde(default)]
            per_model: BTreeMap<ModelId, ModelCounters>,
            #[serde(default)]
            agent_per_model: BTreeMap<ModelId, ModelCounters>,
            #[serde(default)]
            scorer_per_model: BTreeMap<ModelId, ModelCounters>,
            #[serde(default)]
            side_call_per_model: BTreeMap<ModelId, ModelCounters>,
            #[serde(default)]
            per_task_agent: BTreeMap<String, PerTaskAgentCounters>,
            #[serde(default)]
            tool_calls_by_name: BTreeMap<String, u64>,
            // Legacy flat fields — present on sidecars written before
            // Bundle C. Folded into `per_model` after the fact.
            #[serde(default)]
            sonnet_count: u64,
            #[serde(default)]
            opus_count: u64,
            #[serde(default)]
            sonnet_input_tokens: u64,
            #[serde(default)]
            sonnet_output_tokens: u64,
            #[serde(default)]
            sonnet_cache_read_tokens: u64,
            #[serde(default)]
            sonnet_cache_creation_tokens: u64,
            #[serde(default)]
            opus_input_tokens: u64,
            #[serde(default)]
            opus_output_tokens: u64,
            #[serde(default)]
            opus_cache_read_tokens: u64,
            #[serde(default)]
            opus_cache_creation_tokens: u64,
            instance_seconds: BTreeMap<String, u64>,
            running_task_starts: BTreeMap<String, (String, u64)>,
            high_water_exceeded_count: u64,
            batch_dropped_events: u64,
            #[serde(default)]
            tool_loop_iterations_histogram: BTreeMap<u32, u64>,
            #[serde(default)]
            opus_escalation_reasons: BTreeMap<String, u64>,
            #[serde(default)]
            composer_runs: u64,
            #[serde(default)]
            composer_total_duration_ms: u64,
            #[serde(default)]
            composer_atoms_considered: u64,
            #[serde(default)]
            composer_backtracks: u64,
            #[serde(default)]
            composer_exclusion_hits: u64,
            #[serde(default)]
            iterations_run_total: u64,
            #[serde(default)]
            converged_at_iter: Vec<u64>,
            #[serde(default)]
            max_iterations_hit_count: u64,
            #[serde(default)]
            iteration_total_duration_ms: u64,
            // New SME-experience fields. Always defaulted so
            // sidecars written before this change deserialize cleanly.
            #[serde(default)]
            followup_count: u32,
            #[serde(default)]
            amendment_count: u32,
            #[serde(default)]
            blockers_encountered: Vec<BlockerEventRecord>,
            #[serde(default)]
            is_ambiguous: Option<bool>,
        }
        let raw = Raw::deserialize(deserializer)?;
        let mut per_model = raw.per_model;
        // Fold legacy sonnet_* / opus_* fields into per_model if the new
        // map didn't already cover them. Any value already in per_model
        // wins so a hybrid sidecar (new client that saw an old crash
        // recover path) doesn't double-count.
        if raw.sonnet_count > 0 || raw.sonnet_input_tokens > 0 {
            per_model.entry(ModelId::Sonnet46).or_insert(ModelCounters {
                count: raw.sonnet_count,
                input_tokens: raw.sonnet_input_tokens,
                output_tokens: raw.sonnet_output_tokens,
                cache_read_tokens: raw.sonnet_cache_read_tokens,
                cache_creation_tokens: raw.sonnet_cache_creation_tokens,
            });
        }
        if raw.opus_count > 0 || raw.opus_input_tokens > 0 {
            per_model.entry(ModelId::Opus46).or_insert(ModelCounters {
                count: raw.opus_count,
                input_tokens: raw.opus_input_tokens,
                output_tokens: raw.opus_output_tokens,
                cache_read_tokens: raw.opus_cache_read_tokens,
                cache_creation_tokens: raw.opus_cache_creation_tokens,
            });
        }
        Ok(SessionCounters {
            turn_count: raw.turn_count,
            tool_call_count: raw.tool_call_count,
            turn_durations_ms: raw.turn_durations_ms,
            per_model,
            agent_per_model: raw.agent_per_model,
            scorer_per_model: raw.scorer_per_model,
            side_call_per_model: raw.side_call_per_model,
            per_task_agent: raw.per_task_agent,
            tool_calls_by_name: raw.tool_calls_by_name,
            instance_seconds: raw.instance_seconds,
            running_task_starts: raw.running_task_starts,
            high_water_exceeded_count: raw.high_water_exceeded_count,
            batch_dropped_events: raw.batch_dropped_events,
            tool_loop_iterations_histogram: raw.tool_loop_iterations_histogram,
            opus_escalation_reasons: raw.opus_escalation_reasons,
            composer_runs: raw.composer_runs,
            composer_total_duration_ms: raw.composer_total_duration_ms,
            composer_atoms_considered: raw.composer_atoms_considered,
            composer_backtracks: raw.composer_backtracks,
            composer_exclusion_hits: raw.composer_exclusion_hits,
            iterations_run_total: raw.iterations_run_total,
            converged_at_iter: raw.converged_at_iter,
            max_iterations_hit_count: raw.max_iterations_hit_count,
            iteration_total_duration_ms: raw.iteration_total_duration_ms,
            followup_count: raw.followup_count,
            amendment_count: raw.amendment_count,
            blockers_encountered: raw.blockers_encountered,
            is_ambiguous: raw.is_ambiguous,
        })
    }
}

/// Serialize a `ModelId` as its serde name (`sonnet_46`, `opus_46`, …)
/// for map keys in JSON output. Round-trips cleanly via the enum's
/// Deserialize impl if we ever want to parse them back.
fn model_id_key(m: ModelId) -> String {
    serde_json::to_value(m)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| format!("{:?}", m).to_lowercase())
}

impl SessionCounters {
    pub(crate) fn snapshot(&self) -> SessionMetrics {
        let (p50, p95, p99, mean, max) = percentiles(&self.turn_durations_ms);
        let total_instance_seconds = self.instance_seconds.values().sum();

        // F6 — per-surface token split. Each surface bucket accumulates
        // (input/output/cache_read/cache_creation) from every model
        // contributing to that surface. Aggregated alongside the per-
        // surface cost rollups below so we don't walk the per-model
        // maps twice.
        let mut per_surface_tokens = PerSurfaceTokens::default();

        let mut per_model_turns = BTreeMap::new();
        let mut per_model_cost_usd = BTreeMap::new();
        let mut total_input = 0u64;
        let mut total_output = 0u64;
        let mut total_cache_read = 0u64;
        let mut total_cache_creation = 0u64;
        // Accumulate cost buckets as `Cost` (micro-USD, saturating
        // arithmetic) to prevent NaN/INF propagation from agent-reported
        // usage or pricing-table fail-open paths. Converted to f64 at
        // the end via `Cost::as_usd()` for the backward-compatible
        // `SessionMetrics` f64 fields.
        let mut chat_cost = Cost::ZERO;
        for (&model, c) in &self.per_model {
            let cost_f = pricing::cost_usd(
                pricing::prices_for(model),
                c.input_tokens,
                c.output_tokens,
                c.cache_read_tokens,
                c.cache_creation_tokens,
            );
            let cost = Cost::from_usd_f64(cost_f).unwrap_or(Cost::ZERO);
            let key = model_id_key(model);
            per_model_turns.insert(key.clone(), c.count);
            per_model_cost_usd.insert(key, cost_f);
            total_input += c.input_tokens;
            total_output += c.output_tokens;
            total_cache_read += c.cache_read_tokens;
            total_cache_creation += c.cache_creation_tokens;
            chat_cost = chat_cost.saturating_add(cost);
            per_surface_tokens.chat.add_from(c);
        }

        // Agent-side spend: priced per-model too, but bucketed separately
        // so the SME can see chat vs agent as distinct lines. Agent
        // tokens also add to the total_{input,output,cache_*} running
        // sums so the top-level `total_input_tokens` field reflects
        // every billable token in the session.
        let mut per_model_agent_cost_usd = BTreeMap::new();
        let mut agent_cost = Cost::ZERO;
        for (&model, c) in &self.agent_per_model {
            let cost_f = pricing::cost_usd(
                pricing::prices_for(model),
                c.input_tokens,
                c.output_tokens,
                c.cache_read_tokens,
                c.cache_creation_tokens,
            );
            let cost = Cost::from_usd_f64(cost_f).unwrap_or(Cost::ZERO);
            per_model_agent_cost_usd.insert(model_id_key(model), cost_f);
            total_input += c.input_tokens;
            total_output += c.output_tokens;
            total_cache_read += c.cache_read_tokens;
            total_cache_creation += c.cache_creation_tokens;
            agent_cost = agent_cost.saturating_add(cost);
            per_surface_tokens.agent.add_from(c);
        }
        // Scorer-side spend: same shape as agent_per_model, own bucket
        // so the three cost rows (chat / agent / scorer) stay legible
        // in the UI. Scorer tokens fold into the top-level totals for
        // the same "every billable token is counted" invariant.
        let mut per_model_scorer_cost_usd = BTreeMap::new();
        let mut scorer_cost = Cost::ZERO;
        for (&model, c) in &self.scorer_per_model {
            let cost_f = pricing::cost_usd(
                pricing::prices_for(model),
                c.input_tokens,
                c.output_tokens,
                c.cache_read_tokens,
                c.cache_creation_tokens,
            );
            let cost = Cost::from_usd_f64(cost_f).unwrap_or(Cost::ZERO);
            per_model_scorer_cost_usd.insert(model_id_key(model), cost_f);
            total_input += c.input_tokens;
            total_output += c.output_tokens;
            total_cache_read += c.cache_read_tokens;
            total_cache_creation += c.cache_creation_tokens;
            scorer_cost = scorer_cost.saturating_add(cost);
            per_surface_tokens.scorer.add_from(c);
        }
        // Side-call spend: same pattern as the scorer bucket above.
        // Side calls fold into the top-level totals so the "every
        // billable token counted" invariant holds across the four
        // buckets.
        let mut per_model_side_call_cost_usd = BTreeMap::new();
        let mut side_call_cost = Cost::ZERO;
        for (&model, c) in &self.side_call_per_model {
            let cost_f = pricing::cost_usd(
                pricing::prices_for(model),
                c.input_tokens,
                c.output_tokens,
                c.cache_read_tokens,
                c.cache_creation_tokens,
            );
            let cost = Cost::from_usd_f64(cost_f).unwrap_or(Cost::ZERO);
            per_model_side_call_cost_usd.insert(model_id_key(model), cost_f);
            total_input += c.input_tokens;
            total_output += c.output_tokens;
            total_cache_read += c.cache_read_tokens;
            total_cache_creation += c.cache_creation_tokens;
            side_call_cost = side_call_cost.saturating_add(cost);
            per_surface_tokens.side_call.add_from(c);
        }
        // Saturating sum across buckets prevents NaN/INF from infecting
        // `total_cost_usd` even when individual per-model costs are
        // zero'd on conversion failure.
        let total_cost = chat_cost
            .saturating_add(agent_cost.saturating_add(scorer_cost.saturating_add(side_call_cost)));
        // Convert back to f64 for the backward-compatible SessionMetrics
        // shape that the UI and serialization layer consume.
        let chat_cost_usd = chat_cost.as_usd();
        let agent_cost_usd = agent_cost.as_usd();
        let scorer_cost_usd = scorer_cost.as_usd();
        let side_call_cost_usd = side_call_cost.as_usd();
        let total_cost_usd = total_cost.as_usd();
        // UI back-compat mirrors of the two historically surfaced models.
        // `opus_turns` / `opus_cost_usd` aggregate ALL Opus variants
        // (4.6 + 4.7) so the UI row stays continuous across the 4.6→4.7
        // escalation-target upgrade. Rendering of Opus 4.7 specifically
        // flows through the `per_model_turns` / `per_model_cost_usd`
        // maps keyed by serde name.
        let sonnet_turns = self
            .per_model
            .get(&ModelId::Sonnet46)
            .map(|c| c.count)
            .unwrap_or(0);
        let opus_turns: u64 = self
            .per_model
            .iter()
            .filter(|(m, _)| m.is_opus())
            .map(|(_, c)| c.count)
            .sum();
        let sonnet_cost_usd = per_model_cost_usd
            .get(&model_id_key(ModelId::Sonnet46))
            .copied()
            .unwrap_or(0.0);
        let opus_cost_usd: f64 = ModelId::ALL
            .iter()
            .copied()
            .filter(|m| m.is_opus())
            .filter_map(|m| per_model_cost_usd.get(&model_id_key(m)).copied())
            .sum();

        // Cache-hit ratio across billed input tokens. Denominator is the
        // sum of all three input-side counters (uncached input, cache
        // read, cache creation). Zero when nothing's been recorded —
        // signals "not enough data yet" to the UI rather than a dead
        // cache.
        let total_billed_input = total_input + total_cache_read + total_cache_creation;
        let cache_hit_ratio = if total_billed_input == 0 {
            0.0
        } else {
            total_cache_read as f64 / total_billed_input as f64
        };

        SessionMetrics {
            turn_count: self.turn_count,
            tool_call_count: self.tool_call_count,
            total_input_tokens: total_input,
            total_output_tokens: total_output,
            cache_read_tokens: total_cache_read,
            cache_creation_tokens: total_cache_creation,
            p50_turn_ms: p50,
            p95_turn_ms: p95,
            p99_turn_ms: p99,
            mean_turn_ms: mean,
            max_turn_ms: max,
            sonnet_turns,
            opus_turns,
            per_model_turns,
            per_model_cost_usd,
            total_instance_seconds,
            instance_type_seconds: self.instance_seconds.clone(),
            high_water_exceeded_count: self.high_water_exceeded_count,
            batch_dropped_events: self.batch_dropped_events,
            total_cost_usd,
            chat_cost_usd,
            agent_cost_usd,
            per_model_agent_cost_usd,
            scorer_cost_usd,
            per_model_scorer_cost_usd,
            side_call_cost_usd,
            per_model_side_call_cost_usd,
            sonnet_cost_usd,
            opus_cost_usd,
            cache_hit_ratio,
            tool_loop_iterations_histogram: self.tool_loop_iterations_histogram.clone(),
            opus_escalation_reasons: self.opus_escalation_reasons.clone(),
            composer_runs: self.composer_runs,
            composer_total_duration_ms: self.composer_total_duration_ms,
            composer_atoms_considered: self.composer_atoms_considered,
            composer_backtracks: self.composer_backtracks,
            composer_exclusion_hits: self.composer_exclusion_hits,
            session_token_budget: read_session_token_budget(),
            agent_billing_mode: read_agent_billing_mode(),
            per_surface_tokens,
            per_task_agent: build_per_task_agent_snapshots(&self.per_task_agent),
            tool_calls_by_name: self.tool_calls_by_name.clone(),
            // F3 — Bare snapshot doesn't have access to Session.created_at;
            // the service-layer wrapper `metrics_snapshot` patches this
            // field after the fact when a Session is loadable.
            session_duration_seconds: 0,
            // F5 — derive cache savings from each per-model counter
            // map directly. Inline call below to keep the snapshot
            // function locally readable.
            cache_savings_usd: {
                let chat_s = sum_cache_savings(&self.per_model);
                let agent_s = sum_cache_savings(&self.agent_per_model);
                let scorer_s = sum_cache_savings(&self.scorer_per_model);
                let side_s = sum_cache_savings(&self.side_call_per_model);
                chat_s + agent_s + scorer_s + side_s
            },
            per_surface_cache_savings_usd: {
                let mut m = BTreeMap::new();
                let chat_s = sum_cache_savings(&self.per_model);
                if chat_s > 0.0 {
                    m.insert("chat".to_string(), chat_s);
                }
                let agent_s = sum_cache_savings(&self.agent_per_model);
                if agent_s > 0.0 {
                    m.insert("agent".to_string(), agent_s);
                }
                let scorer_s = sum_cache_savings(&self.scorer_per_model);
                if scorer_s > 0.0 {
                    m.insert("scorer".to_string(), scorer_s);
                }
                let side_s = sum_cache_savings(&self.side_call_per_model);
                if side_s > 0.0 {
                    m.insert("side_call".to_string(), side_s);
                }
                m
            },
            // Budget fields are patched in by the service-layer
            // `metrics_snapshot` from the Session's own `budget_usd` +
            // from the DAG for projections. Bare snapshot leaves them
            // None / zero so tests without a Session still work.
            budget_usd: None,
            budget_used_pct: None,
            budget_state: None,
            projected_remaining_usd: 0.0,
            projected_finish_usd: 0.0,
            // Patched in by the service-layer `metrics_snapshot`
            // from the Session's `affordance_fallback_counter`. Bare
            // snapshot leaves it empty so tests without a Session still
            // work (same pattern as budget_usd above).
            affordance_fallbacks: Vec::new(),
            // SME-experience counters lifted directly from
            // `SessionCounters`. No service-layer patching needed; the
            // values come entirely from in-memory counters managed by
            // the `record_*` methods below.
            followup_count: self.followup_count,
            amendment_count: self.amendment_count,
            blockers_encountered: self.blockers_encountered.clone(),
            is_ambiguous: self.is_ambiguous,
        }
    }
}

/// F5 — savings from prompt caching for a single surface's per-model
/// counter map. `cache_read_tokens × (input_rate − cache_read_rate)`
/// for each model present, summed. Returns 0 when the map is empty.
fn sum_cache_savings(per_model: &BTreeMap<ModelId, ModelCounters>) -> f64 {
    let mut total = 0.0;
    for (&model, c) in per_model {
        if c.cache_read_tokens == 0 {
            continue;
        }
        let prices = pricing::prices_for(model);
        let savings_per_mtok = prices.input_per_mtok - prices.cache_read_per_mtok;
        total += (c.cache_read_tokens as f64 / 1_000_000.0) * savings_per_mtok;
    }
    total
}

/// F1 — fold the per-task counter map into a sorted `Vec` for the UI.
/// Sorted by `cost_usd` descending so the operator sees the biggest
/// contributors first; ties broken by `task_id` ascending for stable
/// rendering. `stage_class` is left as-is on the counter — the
/// public `MetricsStore::snapshot_with_session` variant fills it in
/// when called with a `Session` whose DAG is loaded.
fn build_per_task_agent_snapshots(
    per_task: &BTreeMap<String, PerTaskAgentCounters>,
) -> Vec<PerTaskAgentSnapshot> {
    let mut out: Vec<PerTaskAgentSnapshot> = per_task
        .iter()
        .map(|(task_id, entry)| {
            let cost = pricing::cost_usd(
                pricing::prices_for(entry.model),
                entry.counters.input_tokens,
                entry.counters.output_tokens,
                entry.counters.cache_read_tokens,
                entry.counters.cache_creation_tokens,
            );
            PerTaskAgentSnapshot {
                task_id: task_id.clone(),
                model: model_id_key(entry.model),
                stage_class: entry.stage_class.clone(),
                input_tokens: entry.counters.input_tokens,
                output_tokens: entry.counters.output_tokens,
                cache_read_tokens: entry.counters.cache_read_tokens,
                cache_creation_tokens: entry.counters.cache_creation_tokens,
                cost_usd: cost,
                elapsed_secs: entry.elapsed_secs,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.task_id.cmp(&b.task_id))
    });
    out
}

/// `SWFC_AGENT_BILLING` at metrics snapshot time. "subscription" is the
/// default; any other value (typically "api") indicates forwarded API
/// key and real per-token charges for agent work. See
/// `scripts/agent-claude.sh` for the complementary write-side logic.
fn read_agent_billing_mode() -> String {
    std::env::var("SWFC_AGENT_BILLING")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "subscription".to_string())
}

/// §4.4 — resolve `SWFC_SESSION_TOKEN_BUDGET` into the optional budget
/// surfaced on `SessionMetrics`. Semantics match `service/tool_loop.rs`'s
/// `check_budget` helper so the UI progress bar agrees with the soft-
/// block the tool loop actually enforces:
/// - unset → default 500_000
/// - "0" → `None` (budget disabled)
/// - n>0 → `Some(n)`
/// - parse error → fall back to the default so an ops typo doesn't
///   silently uncap the session
pub(crate) fn read_session_token_budget() -> Option<u64> {
    const DEFAULT_BUDGET: u64 = 500_000;
    let raw = std::env::var("SWFC_SESSION_TOKEN_BUDGET").ok();
    let parsed = raw
        .as_deref()
        .map(|s| s.parse::<u64>().unwrap_or(DEFAULT_BUDGET))
        .unwrap_or(DEFAULT_BUDGET);
    (parsed > 0).then_some(parsed)
}

/// Map an Anthropic API model string (e.g. "claude-sonnet-4-6") to the
/// ModelId variant so pricing lookup works. Unrecognised strings fall
/// back to Sonnet 4.6 with a stderr warning — that's the current
/// convention for `scripts/agent-claude.sh` and under-reports rather
/// than over-reports when a new model ships before the pricing table
/// is updated.
///
/// Claude Code CLI reports context-window variants with a bracket
/// suffix (e.g. `claude-opus-4-7[1m]`, `claude-sonnet-4-6[200k]`). The
/// `ModelId::api_id()` forms are un-suffixed, so we trim any trailing
/// `[...]` segment before matching. Without this normalization every
/// 1M-context Opus run misreports as Sonnet 4.6.
pub(crate) fn resolve_model_api_id(api_id: &str) -> ModelId {
    let normalized = api_id.split_once('[').map_or(api_id, |(head, _)| head);
    for &m in ModelId::ALL {
        if m.api_id() == normalized {
            return m;
        }
    }
    eprintln!(
        "[metrics] agent reported unknown model '{}'; pricing as Sonnet 4.6 (update metrics.rs::pricing when adding models)",
        api_id
    );
    ModelId::Sonnet46
}

fn percentiles(values: &[u64]) -> (u64, u64, u64, u64, u64) {
    if values.is_empty() {
        return (0, 0, 0, 0, 0);
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let p = |q: f64| -> u64 {
        let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
        sorted[idx]
    };
    let mean = (sorted.iter().sum::<u64>() as f64 / sorted.len() as f64).round() as u64;
    let max = *sorted.last().unwrap();
    (p(0.50), p(0.95), p(0.99), mean, max)
}

/// Helper to produce a zeroed `SessionMetrics` for callers
/// that need a fallback when `MetricsStore::snapshot` returns `None`
/// (a session that emitted before any turn was recorded). Exposes the
/// crate-private `SessionCounters::default().snapshot()` round-trip
/// without leaking the counter struct itself.
pub fn empty_session_metrics() -> SessionMetrics {
    SessionCounters::default().snapshot()
}

/// Thread-safe metrics store keyed by session id.
#[derive(Clone, Default)]
pub struct MetricsStore {
    #[cfg(test)]
    pub(crate) inner: Arc<RwLock<HashMap<SessionId, SessionCounters>>>,
    #[cfg(not(test))]
    inner: Arc<RwLock<HashMap<SessionId, SessionCounters>>>,
    // When set, every record_turn / record_task_* writes a sidecar
    // `<session_id>.metrics.json` alongside the session JSON in this
    // directory. On a snapshot miss, MetricsStore reads the sidecar
    // back into memory. Survives server restart — before this
    // sidecar, metrics were in-memory-only and the UI's Metrics tab
    // returned 404 after any restart mid-run.
    persist_dir: Option<PathBuf>,
    // Serializes the persist step per session so concurrent record_turn
    // / record_task_* callers can't race their async file writes out of
    // order. The in-memory mutation under `inner` already happens under
    // a write lock; this mutex covers only the disk write that follows
    // (the file ordering bug). Allocations are lazy via or_default in
    // persist_one.
    persist_locks: Arc<RwLock<HashMap<SessionId, Arc<Mutex<()>>>>>,
}

impl MetricsStore {
    /// Create a new in-memory `MetricsStore` without persistence configured.
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure the store to persist counters to sidecar files in
    /// `dir`. Best-effort — IO failures are logged to stderr and do
    /// not break the hot path (record_turn still records in-memory).
    pub fn with_persist_dir(mut self, dir: PathBuf) -> Self {
        self.persist_dir = Some(dir);
        self
    }

    /// Persist a single session's counters to sidecar. Call whenever
    /// the in-memory state mutates — record_turn does this. Holds a
    /// per-session async mutex around the file write so concurrent
    /// callers serialize their disk writes in lock-acquisition order
    /// (the in-memory state was already mutated under the inner write
    /// lock by the caller). Without this, two concurrent record_turn
    /// calls could land their files out of order on disk and a future
    /// reload would resurrect a stale snapshot.
    async fn persist_one(&self, id: SessionId, counters: &SessionCounters) {
        let Some(dir) = self.persist_dir.as_ref() else {
            return;
        };
        let bytes = match serde_json::to_vec_pretty(counters) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[metrics] failed to serialize counters: {}", e);
                return;
            }
        };
        // Resolve (or insert) the per-session write-mutex.
        //
        // The read and write lock acquisitions MUST live in separate
        // scopes. Rust extends a `read().await` guard temporary across
        // the entire `if let Some(...) = ... { ... } else { ... }`
        // expression, so a single-expression form would still hold the
        // read guard inside the else arm — deadlocking the write
        // request. The explicit `{ ... }` block on the read path is
        // load-bearing: it forces the read guard to drop before any
        // write attempt.
        let existing = {
            let guard = self.persist_locks.read().await;
            guard.get(&id).cloned()
        };
        let lock = match existing {
            Some(l) => l,
            None => {
                let mut guard = self.persist_locks.write().await;
                guard
                    .entry(id)
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone()
            }
        };
        let _w = lock.lock().await;
        let path = dir.join(format!("{}.metrics.json", id));
        if let Err(e) = tokio::fs::write(&path, bytes).await {
            eprintln!("[metrics] failed to persist {}: {}", path.display(), e);
        }
    }

    /// Load a sidecar into memory. Returns None when the file is
    /// absent or unparseable.
    async fn load_one(&self, id: SessionId) -> Option<SessionCounters> {
        let dir = self.persist_dir.as_ref()?;
        let path = dir.join(format!("{}.metrics.json", id));
        let bytes = tokio::fs::read(&path).await.ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Record metrics for one completed turn.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_turn(
        &self,
        id: SessionId,
        duration: Duration,
        tool_calls: u64,
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_creation: u64,
        model: ModelId,
    ) {
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            counters.turn_count += 1;
            counters.tool_call_count += tool_calls;
            counters
                .turn_durations_ms
                .push(duration.as_millis().min(u64::MAX as u128) as u64);
            let bucket = counters.per_model.entry(model).or_default();
            bucket.count += 1;
            bucket.input_tokens = bucket.input_tokens.saturating_add(input_tokens);
            bucket.output_tokens = bucket.output_tokens.saturating_add(output_tokens);
            bucket.cache_read_tokens = bucket.cache_read_tokens.saturating_add(cache_read);
            bucket.cache_creation_tokens =
                bucket.cache_creation_tokens.saturating_add(cache_creation);
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Record agent-side LLM usage. Called from the server's
    /// `post_progress` handler when a harness event carries an
    /// `agent_usage` block. `model_api_id` is the Anthropic model
    /// string (e.g. "claude-sonnet-4-6") as the agent reported it;
    /// unrecognised strings default to Sonnet 4.6 pricing with a
    /// stderr warning — a conservative choice since that matches
    /// `scripts/agent-claude.sh`'s current pinning. The server passes
    /// the raw string through unchanged so future agent models flow
    /// via a pricing-table update, not a wire-schema change.
    pub async fn record_agent_usage(
        &self,
        id: SessionId,
        model_api_id: &str,
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_creation: u64,
    ) {
        self.record_agent_usage_for_task(
            id,
            None,
            model_api_id,
            input_tokens,
            output_tokens,
            cache_read,
            cache_creation,
        )
        .await
    }

    /// F1 — variant that tags the usage with a `task_id` so the
    /// snapshot can produce per-task breakdowns. The aggregate
    /// `agent_per_model` counters are still updated alongside the
    /// per-task entry, so existing rollups (`agent_cost_usd`,
    /// `per_model_agent_cost_usd`) remain correct. When `task_id` is
    /// `None`, behaves identically to the legacy `record_agent_usage`.
    #[allow(clippy::too_many_arguments)]
    pub async fn record_agent_usage_for_task(
        &self,
        id: SessionId,
        task_id: Option<&str>,
        model_api_id: &str,
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_creation: u64,
    ) {
        let model = resolve_model_api_id(model_api_id);
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            let bucket = counters.agent_per_model.entry(model).or_default();
            bucket.count += 1;
            bucket.input_tokens = bucket.input_tokens.saturating_add(input_tokens);
            bucket.output_tokens = bucket.output_tokens.saturating_add(output_tokens);
            bucket.cache_read_tokens = bucket.cache_read_tokens.saturating_add(cache_read);
            bucket.cache_creation_tokens =
                bucket.cache_creation_tokens.saturating_add(cache_creation);
            // Per-task accounting (F1). One record per task_id; if the
            // same task reports usage twice (idempotent re-emit), the
            // counters accumulate. Stage-class is filled in at snapshot
            // time from the Session's DAG.
            if let Some(tid) = task_id {
                let task_entry = counters
                    .per_task_agent
                    .entry(tid.to_string())
                    .or_insert_with(|| PerTaskAgentCounters {
                        model,
                        counters: ModelCounters::default(),
                        stage_class: None,
                        started_at_ms: None,
                        elapsed_secs: None,
                    });
                task_entry.model = model;
                task_entry.counters.count += 1;
                task_entry.counters.input_tokens = task_entry
                    .counters
                    .input_tokens
                    .saturating_add(input_tokens);
                task_entry.counters.output_tokens = task_entry
                    .counters
                    .output_tokens
                    .saturating_add(output_tokens);
                task_entry.counters.cache_read_tokens = task_entry
                    .counters
                    .cache_read_tokens
                    .saturating_add(cache_read);
                task_entry.counters.cache_creation_tokens = task_entry
                    .counters
                    .cache_creation_tokens
                    .saturating_add(cache_creation);
            }
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Record a rubric-scorer LLM call against this session. Called from
    /// `POST /api/chat/session/:id/score` after `score_transcript` returns.
    /// The scorer is pinned to Sonnet 4.6 at the call site; the `model`
    /// parameter is passed through so the pricing table stays the single
    /// source of truth if that pinning ever changes.
    pub async fn record_scorer_usage(
        &self,
        id: SessionId,
        model: ModelId,
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_creation: u64,
    ) {
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            let bucket = counters.scorer_per_model.entry(model).or_default();
            bucket.count += 1;
            bucket.input_tokens = bucket.input_tokens.saturating_add(input_tokens);
            bucket.output_tokens = bucket.output_tokens.saturating_add(output_tokens);
            bucket.cache_read_tokens = bucket.cache_read_tokens.saturating_add(cache_read);
            bucket.cache_creation_tokens =
                bucket.cache_creation_tokens.saturating_add(cache_creation);
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Record a side-call LLM usage against this session. Side calls
    /// are cheap, read-only LLM hops routed through
    /// `ModelPolicy::for_side_call()` (Haiku 4.5) that do NOT
    /// participate in the main conversation's prompt cache. First
    /// caller is `side_calls::generate_session_title`; future subagents
    /// (narrative TLDRs, error-friendliness passes) flow into the same
    /// `side_call_cost_usd` bucket so the cheap-hop spend stays visibly
    /// distinct from chat / agent / scorer on the UI's Performance
    /// tab. The `model` parameter is passed through rather than
    /// hardcoded so the pricing table remains the single source of
    /// truth for rates if the default for-side-call model ever
    /// changes.
    pub async fn record_side_call_usage(
        &self,
        id: SessionId,
        model: ModelId,
        input_tokens: u64,
        output_tokens: u64,
        cache_read: u64,
        cache_creation: u64,
    ) {
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            let bucket = counters.side_call_per_model.entry(model).or_default();
            bucket.count += 1;
            bucket.input_tokens = bucket.input_tokens.saturating_add(input_tokens);
            bucket.output_tokens = bucket.output_tokens.saturating_add(output_tokens);
            bucket.cache_read_tokens = bucket.cache_read_tokens.saturating_add(cache_read);
            bucket.cache_creation_tokens =
                bucket.cache_creation_tokens.saturating_add(cache_creation);
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Return a `SessionMetrics` snapshot for `id`, or `None` if no data exists.
    pub async fn snapshot(&self, id: SessionId) -> Option<SessionMetrics> {
        // Fast path — in-memory hit.
        if let Some(snap) = {
            let guard = self.inner.read().await;
            guard.get(&id).map(|c| c.snapshot())
        } {
            return Some(snap);
        }
        // Miss — try to rehydrate from sidecar (set when the
        // MetricsStore was built with `with_persist_dir`). Covers
        // server restart mid-session.
        if let Some(counters) = self.load_one(id).await {
            let snap = counters.snapshot();
            let mut guard = self.inner.write().await;
            guard.entry(id).or_insert(counters);
            return Some(snap);
        }
        None
    }

    /// Remove in-memory counters for `id` (called on session expiry).
    pub async fn forget(&self, id: SessionId) {
        let mut guard = self.inner.write().await;
        guard.remove(&id);
    }

    /// Record that a remote task started
    /// at `start_ms` (epoch ms). The matching `record_task_completed`
    /// closes the interval and adds elapsed seconds to the per-instance
    /// counter.
    pub async fn record_task_started(
        &self,
        id: SessionId,
        task_id: &str,
        instance_type: &str,
        start_ms: u64,
    ) {
        // Persist so the start timestamp survives a server restart
        // (without it, record_task_completed can't compute elapsed_secs
        // on the post-restart side).
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            counters
                .running_task_starts
                .insert(task_id.to_string(), (instance_type.to_string(), start_ms));
            // Also stamp the start on the per-task-agent entry (created
            // lazily) so `elapsed_secs` is derivable on completion even when
            // the agent never fires `record_agent_usage_for_task` with a
            // usage block (e.g. non-LLM tasks, or agents that don't emit
            // agent-usage.json).
            counters
                .per_task_agent
                .entry(task_id.to_string())
                .or_default()
                .started_at_ms = Some(start_ms);
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Record a non-remote task start. Used when the harness fires a
    /// task_started event without a `remote` block (local executor).
    /// Populates the per-task start timestamp for wall-clock tracking
    /// but skips the instance-type bucket (no remote compute to bill).
    pub async fn record_task_started_local(&self, id: SessionId, task_id: &str, start_ms: u64) {
        // Persist so the start timestamp survives a server restart.
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            counters
                .per_task_agent
                .entry(task_id.to_string())
                .or_default()
                .started_at_ms = Some(start_ms);
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Pair to `record_task_started`. If no matching start is on file,
    /// silently no-ops (the harness may have started the task before
    /// the session's metrics counter was created).
    pub async fn record_task_completed(&self, id: SessionId, task_id: &str, end_ms: u64) {
        // Persist so instance_seconds + per_task elapsed survive
        // a server restart.
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            if let Some((instance_type, start_ms)) = counters.running_task_starts.remove(task_id) {
                let elapsed_secs = end_ms.saturating_sub(start_ms) / 1000;
                *counters.instance_seconds.entry(instance_type).or_insert(0) += elapsed_secs;
            }
            // Stamp per-task elapsed from the per-task start we captured on
            // task_started (works for both local and remote). `started_at_ms`
            // stays populated so repeat task_completed events (rare, but
            // possible on replay) recompute the same value idempotently.
            if let Some(entry) = counters.per_task_agent.get_mut(task_id) {
                if let Some(start_ms) = entry.started_at_ms {
                    let elapsed = end_ms.saturating_sub(start_ms) as f64 / 1000.0;
                    entry.elapsed_secs = Some(elapsed);
                }
            }
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// counter. Bumped each time the
    /// harness reports that the realized compute exceeded its
    /// high-water baseline.
    /// F2 — record a single tool dispatch by name. Called from the
    /// service-layer tool-use loop (right where `tool_call_count`
    /// already increments) so the per-tool histogram lands without
    /// new wiring through dispatch_one's call sites.
    pub async fn record_tool_call(&self, id: SessionId, tool_name: &str) {
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            *counters
                .tool_calls_by_name
                .entry(tool_name.to_string())
                .or_insert(0) += 1;
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Increment the high-water-exceeded counter for `id` (task exceeded compute baseline).
    pub async fn record_high_water_exceeded(&self, id: SessionId) {
        // Persist so the counter survives a server restart.
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            counters.high_water_exceeded_count += 1;
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// record that the harness batcher dropped `n` events from
    /// this session's in-window queue because it exceeded
    /// HARNESS_BATCH_MAX_EVENTS. Counter is monotonic per session.
    pub async fn record_batch_dropped(&self, id: SessionId, n: u64) {
        if n == 0 {
            return;
        }
        // Persist so the counter survives a server restart.
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            counters.batch_dropped_events += n;
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// §3.2 — record that a tool-loop turn completed after `iterations`
    /// LLM calls. Bucket is exact (no binning) since the cap is small
    /// and the full distribution is useful for re-justifying the cap.
    pub async fn record_tool_loop_iterations(&self, id: SessionId, iterations: usize) {
        // Persist so the histogram survives a server restart.
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            *counters
                .tool_loop_iterations_histogram
                .entry(iterations as u32)
                .or_insert(0) += 1;
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Record one composer run. Aggregates duration +
    /// atom-consideration + backtrack + exclusion-hit counters into the
    /// session's totals. Called from the composer-driven emit path
    /// (the future archetype-fast-path flow); pre-archetype legacy
    /// `intake → build_dag` path doesn't call this. Surfaced via the
    /// Performance tab so operators see when the backward-chain path
    /// is climbing toward the planned p99 < 500ms budget.
    pub async fn record_composer_run(
        &self,
        id: SessionId,
        duration_ms: u64,
        atoms_considered: u64,
        backtracks: u64,
        exclusion_hits: u64,
    ) {
        // Persist so the counter survives a server restart.
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            counters.composer_runs = counters.composer_runs.saturating_add(1);
            counters.composer_total_duration_ms = counters
                .composer_total_duration_ms
                .saturating_add(duration_ms);
            counters.composer_atoms_considered = counters
                .composer_atoms_considered
                .saturating_add(atoms_considered);
            counters.composer_backtracks = counters.composer_backtracks.saturating_add(backtracks);
            counters.composer_exclusion_hits = counters
                .composer_exclusion_hits
                .saturating_add(exclusion_hits);
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Record one completed iteration of an
    /// `IterateUntil` atom. Called by the harness on each
    /// `<id>_iter_N` task completion (whether or not the
    /// convergence rule has fired). `duration_ms` is the iteration's
    /// wallclock span.
    pub async fn record_iteration_run(&self, id: SessionId, duration_ms: u64) {
        // Persist so the counter survives a server restart.
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            counters.iterations_run_total = counters.iterations_run_total.saturating_add(1);
            counters.iteration_total_duration_ms = counters
                .iteration_total_duration_ms
                .saturating_add(duration_ms);
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Record an iterate atom converging at iteration
    /// `iter_count`. Bucketed against
    /// `ITERATIONS_HISTOGRAM_BUCKETS` so the Performance tab can
    /// render a fixed-shape histogram.
    pub async fn record_iteration_converged(&self, id: SessionId, iter_count: u64) {
        // Persist so the histogram survives a server restart.
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            // Lazily seed the histogram with N zero buckets the first
            // time we see any iteration converge.
            if counters.converged_at_iter.is_empty() {
                counters
                    .converged_at_iter
                    .resize(ITERATIONS_HISTOGRAM_BUCKETS.len(), 0);
            }
            // Find the bucket: largest lower-bound ≤ iter_count.
            let mut bucket = 0;
            for (i, edge) in ITERATIONS_HISTOGRAM_BUCKETS.iter().enumerate() {
                if iter_count >= *edge {
                    bucket = i;
                }
            }
            counters.converged_at_iter[bucket] =
                counters.converged_at_iter[bucket].saturating_add(1);
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Record an iterate atom hitting `max_iterations`
    /// without converging. Surfaces the
    /// `BlockerKind::IterationDidNotConverge` count in the Performance
    /// tab so the SME can spot atoms whose convergence rule is
    /// chronically too tight.
    pub async fn record_iteration_max_hit(&self, id: SessionId) {
        // Persist so the counter survives a server restart.
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            counters.max_iterations_hit_count = counters.max_iterations_hit_count.saturating_add(1);
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// §3.3 — record that Opus was selected for a turn and why. Called
    /// from `ConversationService::send_turn` whenever
    /// `ModelPolicy::choose_with_reason` returns `Some(reason)`. Sonnet
    /// turns do not call this.
    pub async fn record_opus_escalation(
        &self,
        id: SessionId,
        reason: crate::model_policy::EscalationReason,
    ) {
        let key = serde_json::to_value(reason)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| format!("{:?}", reason).to_lowercase());
        // Persist so the counter survives a server restart.
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            *counters.opus_escalation_reasons.entry(key).or_insert(0) += 1;
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    // -----------------------------------------------------------------
    // SME-experience counters for Tier 16.2–16.4.
    // -----------------------------------------------------------------

    /// Record one `IntakeFollowup` turn. Called by the service layer at
    /// the end of `ConversationService::send_turn` when the session is
    /// (or was, before the turn advanced the state) in
    /// `SessionState::IntakeFollowup`. Best-effort: an IO failure on
    /// `persist_one` is logged to stderr but never surfaces as an error
    /// to the caller — the turn's conversational content is authoritative
    /// and this counter is only for eval telemetry.
    pub async fn record_intake_followup_turn(&self, id: SessionId) {
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            counters.followup_count = counters.followup_count.saturating_add(1);
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Record an `Emitted → Amending` transition. Called by the service
    /// layer's `/confirm` + `amend_stage_method` dispatch path when
    /// `StateTrigger::AmendStart` fires. Each call represents one
    /// additional amendment round for this session. Saturating increment
    /// so a runaway amend loop can't panic.
    pub async fn record_amendment(&self, id: SessionId) {
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            counters.amendment_count = counters.amendment_count.saturating_add(1);
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Record a `→ Blocked` state transition. Appends an unrecovered
    /// `BlockerEventRecord` with the given `blocker_kind` string (the
    /// serde name of `BlockerKind`, e.g. `"ToolError"`,
    /// `"ValidationFailed"`). Called from the service layer whenever
    /// `try_transition(HarnessTaskBlocked {.. })` or
    /// `try_transition(InfraError {.. })` succeeds.
    pub async fn record_blocker_entered(&self, id: SessionId, blocker_kind: String) {
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            counters.blockers_encountered.push(BlockerEventRecord {
                blocker_kind,
                recovered: false,
                recovery_path: None,
            });
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Mark the most-recently-entered blocker as recovered. Called from
    /// the service layer's `/unblock` handler after the session
    /// successfully leaves `Blocked`. `recovery_path` is the action
    /// the SME took (e.g. `"amend_stage_method"`, `"rerun_task"`,
    /// `"branch_session"`, `"unblock"`).
    ///
    /// Only the last unrecovered entry is mutated — if the session
    /// entered Blocked twice without an intermediate unblock (which
    /// `try_transition` permits for harness re-blockers), the second
    /// entry is the live one; the first stays `recovered = false` as a
    /// faithful audit record.
    pub async fn record_blocker_recovered(&self, id: SessionId, recovery_path: Option<String>) {
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            // Walk from the end to find the first unrecovered entry.
            if let Some(entry) = counters
                .blockers_encountered
                .iter_mut()
                .rev()
                .find(|e| !e.recovered)
            {
                entry.recovered = true;
                entry.recovery_path = recovery_path;
            }
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }

    /// Record the classification confidence for this session. Called from
    /// the `classify_intake` / `append_intake_prose` tool dispatch path
    /// after a `ClassificationResult` is produced.
    ///
    /// `is_ambiguous` semantics: `Some(true)` if `confidence < 0.6`,
    /// `Some(false)` otherwise. Once set to `true` it stays `true`
    /// (sticky-highest-ambiguity semantics — if any classification in
    /// the session was low-confidence, the session is flagged as
    /// ambiguous). If the first call sets `Some(false)` a subsequent
    /// high-confidence reclassification does NOT reset it; that edge
    /// keeps the eval signal conservative.
    pub async fn record_classification_confidence(&self, id: SessionId, confidence: f64) {
        const AMBIGUITY_THRESHOLD: f64 = 0.6;
        let new_ambiguous = confidence < AMBIGUITY_THRESHOLD;
        let counters = {
            let mut guard = self.inner.write().await;
            let counters = guard.entry(id).or_default();
            // Sticky-true: once ambiguous, stays ambiguous.
            counters.is_ambiguous = Some(match counters.is_ambiguous {
                Some(true) => true,
                _ => new_ambiguous,
            });
            counters.clone()
        };
        self.persist_one(id, &counters).await;
    }
}
