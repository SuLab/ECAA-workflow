//! Mock LlmBackend for unit tests and the offline kill-switch.
//!
//! Drives a session through a scripted sequence of `TurnResponse` values.

use crate::anthropic::{LlmBackend, TurnRequest, TurnResponse};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Mutex;

/// Combine the scripted-vec and cursor under one Mutex so callers can't
/// invent a future lock-acquisition order that deadlocks against
/// `send_turn` or `remaining`. The two fields are always read together,
/// so unifying them costs nothing and forecloses the ordering footgun.
struct MockState {
    scripted: Vec<TurnResponse>,
    cursor: usize,
}

/// Scripted `LlmBackend` for deterministic fixture-based tests.
/// Plays back a pre-recorded sequence of `TurnResponse`s one by one.
pub struct MockLlmBackend {
    state: Mutex<MockState>,
}

impl MockLlmBackend {
    /// Construct from a pre-recorded sequence of `TurnResponse`s.
    pub fn new(scripted: Vec<TurnResponse>) -> Self {
        Self {
            state: Mutex::new(MockState {
                scripted,
                cursor: 0,
            }),
        }
    }

    /// Return the number of scripted responses not yet consumed.
    pub fn remaining(&self) -> usize {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.scripted.len().saturating_sub(state.cursor)
    }
}

#[async_trait]
impl LlmBackend for MockLlmBackend {
    async fn send_turn(&self, _request: TurnRequest) -> Result<TurnResponse> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.cursor >= state.scripted.len() {
            return Err(anyhow::anyhow!(
                "MockLlmBackend exhausted at cursor {}",
                state.cursor
            ));
        }
        let resp = state.scripted[state.cursor].clone();
        state.cursor += 1;
        Ok(resp)
    }

    /// Explicit streaming impl. The default trait fallback emits
    /// one post-hoc delta of the assistant text, but does not synthesize
    /// tool-call boundaries — a mock-driven turn carrying a tool call
    /// reached the service layer with an empty `tool_uses` Vec when
    /// routed through the streaming path. Calling `send_turn` directly
    /// and firing the assistant text through the delta sink preserves
    /// the structured `TurnResponse` (tool_uses, stop_reason, usage)
    /// that the tool loop reads.
    async fn send_turn_streaming(
        &self,
        request: TurnRequest,
        on_delta: crate::anthropic::delta_sink::DeltaSink,
    ) -> Result<TurnResponse> {
        let response = self.send_turn(request).await?;
        if !response.assistant_content.is_empty() {
            on_delta(&response.assistant_content);
        }
        Ok(response)
    }

    /// Deterministic token estimate. The default trait impl returns
    /// `None`, which forces the tool loop's `count_tokens` preflight to
    /// skip the budget check on every mock-driven turn. Returning a
    /// rough chars/4 estimate (~English text approximation) covers the
    /// `Ok(Some(n))` branch without a real Anthropic round trip.
    async fn count_tokens(&self, request: &TurnRequest) -> Result<Option<u32>> {
        let mut total_chars: usize = serde_json::to_string(&request.tool_schemas)
            .unwrap_or_default()
            .len();
        for block in &request.system_prompt {
            total_chars = total_chars.saturating_add(block.text.len());
        }
        for turn in request.conversation.iter() {
            total_chars = total_chars.saturating_add(turn.content.len());
        }
        for exchange in &request.tool_exchange {
            total_chars = total_chars
                .saturating_add(serde_json::to_string(exchange).unwrap_or_default().len());
        }
        // chars/4 heuristic — same order of magnitude as Anthropic
        // tokenisation for English prose + JSON; test paths only assert
        // the value is non-zero.
        let estimate = (total_chars / 4) as u32;
        Ok(Some(estimate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::{StopReason, Usage};
    use crate::tools::{BatchableTool, Tool};
    use uuid::Uuid;

    fn assistant_text(s: &str) -> TurnResponse {
        TurnResponse {
            assistant_content: s.into(),
            tool_uses: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
            request_metadata: Default::default(),
        }
    }

    fn tool_use(tool: Tool) -> TurnResponse {
        TurnResponse {
            assistant_content: String::new(),
            tool_uses: vec![(Uuid::new_v4(), tool)],
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            request_metadata: Default::default(),
        }
    }

    fn empty_request() -> TurnRequest {
        TurnRequest {
            system_prompt: vec![],
            conversation: std::sync::Arc::new(vec![]),
            tool_schemas: vec![],
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_exchange: vec![],
            tool_choice: None,
        }
    }

    #[tokio::test]
    async fn mock_returns_scripted_in_order() {
        let mock = MockLlmBackend::new(vec![assistant_text("first"), assistant_text("second")]);
        let r1 = mock.send_turn(empty_request()).await.unwrap();
        let r2 = mock.send_turn(empty_request()).await.unwrap();
        assert_eq!(r1.assistant_content, "first");
        assert_eq!(r2.assistant_content, "second");
    }

    #[tokio::test]
    async fn mock_exhaustion_errors() {
        let mock = MockLlmBackend::new(vec![assistant_text("only")]);
        mock.send_turn(empty_request()).await.unwrap();
        let r = mock.send_turn(empty_request()).await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn mock_drives_tool_use_sequence() {
        let mock = MockLlmBackend::new(vec![
            tool_use(Tool::Batchable(BatchableTool::ClassifyIntake {
                prose: "rnaseq".into(),
            })),
            assistant_text("done"),
        ]);
        assert_eq!(mock.remaining(), 2);
        let _ = mock.send_turn(empty_request()).await.unwrap();
        let _ = mock.send_turn(empty_request()).await.unwrap();
        assert_eq!(mock.remaining(), 0);
    }

    /// Streaming impl preserves tool_uses (the default trait
    /// fallback used to lose them). Regression guard.
    #[tokio::test]
    async fn mock_streaming_preserves_tool_uses() {
        let mock = MockLlmBackend::new(vec![tool_use(Tool::Batchable(
            BatchableTool::ClassifyIntake {
                prose: "rnaseq".into(),
            },
        ))]);
        let captured = std::sync::Arc::new(Mutex::new(String::new()));
        let cap = captured.clone();
        let sink: crate::anthropic::delta_sink::DeltaSink = std::sync::Arc::new(move |s: &str| {
            cap.lock().unwrap().push_str(s);
        });
        let resp = mock
            .send_turn_streaming(empty_request(), sink)
            .await
            .unwrap();
        assert_eq!(resp.tool_uses.len(), 1);
    }

    /// Streaming impl streams the assistant_content through the
    /// delta sink (same shape as the live path).
    #[tokio::test]
    async fn mock_streaming_fires_delta_for_assistant_text() {
        let mock = MockLlmBackend::new(vec![assistant_text("hello world")]);
        let captured = std::sync::Arc::new(Mutex::new(String::new()));
        let cap = captured.clone();
        let sink: crate::anthropic::delta_sink::DeltaSink = std::sync::Arc::new(move |s: &str| {
            cap.lock().unwrap().push_str(s);
        });
        let _ = mock
            .send_turn_streaming(empty_request(), sink)
            .await
            .unwrap();
        assert_eq!(captured.lock().unwrap().as_str(), "hello world");
    }

    /// Deterministic count_tokens stub returns Some(n>0) so the
    /// tool loop's preflight branch is exercised in mock-driven tests.
    #[tokio::test]
    async fn mock_count_tokens_returns_deterministic_estimate() {
        let mock = MockLlmBackend::new(vec![]);
        let req = TurnRequest {
            system_prompt: vec![crate::prompt::SystemPromptBlock {
                text: "system prompt text that should contribute to the estimate".into(),
                cache: false,
            }],
            conversation: std::sync::Arc::new(vec![crate::session::Turn::user(
                "user message text",
            )]),
            tool_schemas: vec![],
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_exchange: vec![],
            tool_choice: None,
        };
        let n = mock.count_tokens(&req).await.unwrap();
        assert!(n.is_some());
        assert!(n.unwrap() > 0);
    }
}
