//! Per-session telemetry. Tracks turn count, tool-call count, total
//! token usage, and per-turn latency across the lifetime of a session.
//! Exposed via the conversation service so `crates/server` can surface
//! a `/api/chat/session/:id/metrics`
//! endpoint without poking at session internals.
//!
//! Latency percentiles are computed cheaply: turn durations are buffered
//! in a sorted-on-read vector; with a tool-loop cap of 8 LLM calls per
//! user turn and natural session lengths well under a hundred turns, the
//! sort cost is negligible.
//!
//! # Cost-accounting scope
//!
//! Every Anthropic API call made by this server is per-session and bills
//! through `MetricsStore`. There is no opt-out path and no orphan spend
//! log. The three buckets on `SessionMetrics` sum to `total_cost_usd`:
//!
//! - **`chat_cost_usd`** — LLM turns driven by
//!   `ConversationService::send_turn` / `send_turn_streaming`, recorded
//!   via `MetricsStore::record_turn` after the whole tool-use loop
//!   finishes. A single user turn may fire up to 16 LLM calls; their
//!   usage is accumulated and recorded once.
//! - **`agent_cost_usd`** — LLM usage from harness progress events
//!   carrying an `agent_usage` block, recorded via
//!   `MetricsStore::record_agent_usage`. The agent script
//!   (`scripts/agent-claude.sh`) parses the Claude Code CLI's
//!   `--output-format=json` final line into
//!   `runtime/outputs/<task_id>/agent-usage.json`; the harness reads
//!   that file on `task_completed` and forwards the usage to the server.
//!   Zero when the agent script isn't instrumented.
//! - **`scorer_cost_usd`** — rubric-scorer calls from
//!   `scorer::score_transcript`, recorded via
//!   `MetricsStore::record_scorer_usage`. Fires only when an operator
//!   triggers `POST /api/chat/session/:id/score` against a live session;
//!   there are no CI-driven rubric runs.
//!
//! Harness-progress heartbeat records written by `harness_batch` are
//! zero-token rows that advance the turn counter without inflating cost.
//!
//! # Module layout
//!
//! Pure organizational split — every public path that existed pre-split
//! is preserved by re-exports here:
//!
//! - [`counters`] — small data-shape types shared across the module
//!   (`ModelCounters`, `TokenBucket`, `PerSurfaceTokens`,
//!   `BlockerEventRecord`, `ITERATIONS_HISTOGRAM_BUCKETS`, etc.).
//! - [`pricing`] — Anthropic list pricing per `ModelId` plus the
//!   `cost_usd` arithmetic.
//! - [`session_metrics`] — public `SessionMetrics` snapshot type.
//! - [`store`] — runtime storage layer (`SessionCounters` +
//!   `MetricsStore` + the snapshot routine + `empty_session_metrics`).
//! - [`io`] — sync emit-time writers
//!   (`write_cost_ledger_row`, `write_session_metrics_row`).

pub mod counters;
pub mod io;
pub mod pricing;
pub mod session_metrics;
pub mod store;

pub use counters::{
    AffordanceFallbackSummary, BlockerEventRecord, ModelCounters, PerSurfaceTokens,
    PerTaskAgentSnapshot, TokenBucket, TurnSource, ITERATIONS_HISTOGRAM_BUCKETS,
};
pub use io::{write_cost_ledger_row, write_session_metrics_row};
pub use session_metrics::SessionMetrics;
pub use store::{empty_session_metrics, MetricsStore};

// Re-export private store helpers at the parent-module path so the
// existing `metrics::tests` and `metrics::tests::budget` references
// (which use `super::*` and `super::super::read_session_token_budget`)
// resolve without churn. Test-only — the helpers carry `pub(crate)`
// in store.rs so this re-export is reachable; non-test builds don't
// need it and would warn if it were unconditional.
#[cfg(test)]
pub(crate) use store::{read_session_token_budget, resolve_model_api_id, SessionCounters};

#[cfg(test)]
mod tests;
