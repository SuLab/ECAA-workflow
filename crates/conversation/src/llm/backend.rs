//! The `LlmBackend` trait — the single abstraction through which the
//! conversation service talks to an LLM. Both the live `AnthropicClient`
//! and the `MockLlmBackend` implement this trait. The streaming variant has
//! a default fallback that calls `send_turn` and fires a single post-hoc
//! delta so every backend works through the streaming path without
//! modification.
//!
//! R2-N19 — the trait lives here under `crate::llm::backend` so the
//! canonical import path is provider-neutral. The request/response shapes
//! (`TurnRequest` / `TurnResponse` / `DeltaSink`) still come from the
//! Anthropic submodule for now; neutralising those is a follow-up PR.
//! The old `crate::anthropic::backend` path remains as a thin re-export
//! so existing callers keep compiling unchanged.

use crate::anthropic::client::{TurnRequest, TurnResponse};
use crate::anthropic::delta_sink::DeltaSink;
use anyhow::Result;
use async_trait::async_trait;

/// Abstraction over the Anthropic Messages API. Implemented by
/// `AnthropicClient` (production) and `MockLlmBackend` / `HeuristicMockBackend`
/// (tests).
#[async_trait]
pub trait LlmBackend: Send + Sync {
    /// Send a single turn to the LLM and return the complete response.
    async fn send_turn(&self, request: TurnRequest) -> Result<TurnResponse>;

    /// Streaming variant. Calls
    /// `on_delta` once per text chunk as it arrives so the UI can render
    /// tokens incrementally. Returns the same `TurnResponse` shape as
    /// `send_turn` so the existing service loop can stay structurally the
    /// same. The default impl falls back to non-streaming + a single
    /// post-hoc delta callback so any backend (including `MockLlmBackend`)
    /// works through the streaming path without modification.
    async fn send_turn_streaming(
        &self,
        request: TurnRequest,
        on_delta: DeltaSink,
    ) -> Result<TurnResponse> {
        let resp = self.send_turn(request).await?;
        if !resp.assistant_content.is_empty() {
            on_delta(&resp.assistant_content);
        }
        Ok(resp)
    }

    /// POST /v1/messages/count_tokens preflight. Returns
    /// the input-token count Anthropic would charge for the supplied
    /// `TurnRequest` if it were sent. Used by the tool loop to detect
    /// runaway turns before they incur the full cost of a send_turn.
    ///
    /// Default returns `None` so backends opt in. `MockLlmBackend`
    /// stays None (offline tests don't have token-counting needs).
    /// `AnthropicClient` overrides this to hit the real endpoint.
    /// The endpoint is free (no charge) but has its own RPM limits;
    /// callers that need throttling should respect 429 responses.
    async fn count_tokens(&self, _request: &TurnRequest) -> Result<Option<u32>> {
        Ok(None)
    }
}
