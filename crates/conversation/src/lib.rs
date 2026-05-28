//! LLM-mediated conversation layer over the deterministic `crates/core`
//! compiler. Tools are thin wrappers — no new business logic lives here.

use std::sync::OnceLock;

/// Read the Anthropic API key for the chat-side UX shim (server,
/// chat-llm CLI, scorer, verify-cache-prefix test). The canonical
/// env var is `SWFC_ANTHROPIC_API_KEY`. Legacy `ANTHROPIC_API_KEY` is
/// accepted as a fallback with a one-time stderr deprecation note, so
/// existing `.env` files keep working during the transition.
///
/// The rename exists because `scripts/agent-claude.sh` invokes
/// `npx @anthropic-ai/claude-code`, and Claude Code CLI reads
/// `ANTHROPIC_API_KEY` when deciding between API billing and the
/// subscription at `~/.claude/.credentials.json`. The old var name
/// leaked into every subprocess and forced API billing unconditionally.
/// `SWFC_ANTHROPIC_API_KEY` is invisible to Claude Code, so the server
/// can keep a chat-side key set without affecting the per-task agent's
/// billing path.
pub fn anthropic_api_key() -> Option<String> {
    if let Ok(v) = std::env::var("SWFC_ANTHROPIC_API_KEY") {
        if !v.is_empty() {
            return Some(v);
        }
    }
    if let Ok(v) = std::env::var("ANTHROPIC_API_KEY") {
        if !v.is_empty() {
            warn_legacy_api_key_once();
            return Some(v);
        }
    }
    None
}

fn warn_legacy_api_key_once() {
    static WARNED: OnceLock<()> = OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!(
            "[scripps-workflow] WARNING: ANTHROPIC_API_KEY is set but \
             SWFC_ANTHROPIC_API_KEY is not. Using ANTHROPIC_API_KEY as a \
             legacy fallback. Please rename in your .env: \
             `ANTHROPIC_API_KEY=…` -> `SWFC_ANTHROPIC_API_KEY=…`. \
             The old name conflicts with `npx @anthropic-ai/claude-code`'s \
             subscription-vs-API auth selection; leaving it set forces \
             per-task agent runs to bill the API instead of the subscription."
        );
    });
}

pub mod anthropic;
/// Audit actor for high-leverage session mutations (mirrors the
/// server's `RequestPrincipal::audit_actor()` output). Used by
/// [`session::ConfirmationToken::granted_by`].
pub mod audit_actor;
pub mod batcher;
// R2-N19 — provider-neutral LLM abstraction. The canonical `LlmBackend`
// trait lives at `crate::llm::backend`; `crate::anthropic::backend`
// stays as a thin re-export shim so existing imports keep working.
// Neutralising the trait's Anthropic-shaped fields (`TurnRequest`,
// `TurnResponse`, `DeltaSink`) is a separate PR.
pub mod emit;
pub mod errors;
pub mod harness_batch;
// Path-hint extractor (e2e #13): scans SME intake prose for
// filesystem-shaped tokens that resolve under SWFC_INPUT_ROOTS, so a
// SME who types "the CSV is at /data/foo.csv" doesn't have to also
// open the Inputs tab to register it manually.
pub mod heuristic_batch;
pub mod heuristic_confidence;
pub mod heuristic_cross_omics;
pub mod heuristic_failure;
pub mod heuristic_mock;
pub mod heuristic_refusal;
pub mod intake_path_hints;
pub mod llm;
pub mod metrics;
pub mod mock;
pub mod model_policy;
pub mod persistence;
pub mod prompt;
// Promotion-pipeline
// gate runner for hypothesized-node proposals. Pure sync, no tokio;
// reuses the core's obligation registry + sandbox
// check against a transient TaskNode synthesized from the proposal.
pub mod proposal_gate;
pub mod scorer;
pub mod service;
pub mod session;
pub mod side_calls;
pub mod sme_text;
pub mod tool_schemas;
pub mod tools;

pub use anthropic::{AnthropicClient, LlmBackend, StopReason, TurnRequest, TurnResponse, Usage};
// StreamEvent, StreamAccumulator, and DeltaSink stay module-
// local in `crate::anthropic`. In-crate consumers (service.rs) use the
// direct submodule path; no crate-level re-export is needed, which
// keeps the public API narrow.
pub use emit::emit_with_conversation_log;
pub use errors::{ConversationError, ToolError, ToolResult};
pub use harness_batch::{BatcherConfig, HarnessBatcher};
pub use heuristic_batch::{build_batched_response, BatchStrategy};
pub use heuristic_mock::HeuristicMockBackend;
pub use metrics::{MetricsStore, SessionMetrics};
pub use mock::MockLlmBackend;
pub use model_policy::{ModelId, ModelPolicy};
pub use persistence::{LineageSummary, SessionMetadata, SessionStore};
pub use prompt::{build_system_prompt, SystemPromptBlock};
pub use scorer::{parse_score, score_transcript, score_transcript_with_model, RubricScore};
pub use service::{AutoEmitOutcome, ConversationService, ServiceError, ServiceEventSink};
pub use session::state::{UserInput, UserInputFile, UserInputKind};
pub use session::{
    session_lineage_schema_version, ConfirmationCard, HarnessEvent, Session, SessionId,
    SessionLineage, SessionState, ShareToken, StateTrigger, ToolCallRecord, TransitionError, Turn,
    TurnRole,
};
pub use tool_schemas::{tool_schemas, tool_status_line};
pub use tools::{dispatch_batch, dispatch_one, BatchableTool, HighImpactTool, Tool, ToolContext};

// Re-export the v4 dispatch types so downstream callers
// (CLI, eval-adapters) can consume them without reaching directly
// into `ecaa_workflow_core::composer`.
pub use ecaa_workflow_core::composer::{ComposerOutput, PolicyDecisionRecord};
pub use ecaa_workflow_core::composer_v4::RankedAlternative;
