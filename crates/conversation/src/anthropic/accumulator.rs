//! `StreamAccumulator` — reassembles a `TurnResponse` from a sequence of
//! `StreamEvent`s. Used by the live streaming path in
//! `AnthropicClient::send_turn_streaming` and covered by unit tests below
//! so the streaming path is verifiable without hitting the real API.

use super::client::{tool_from_api, StopReason, TurnResponse, Usage};
use super::stream::StreamEvent;
use crate::tools::Tool;
use anyhow::{Context, Result};
use uuid::Uuid;

/// Accumulates `StreamEvent`s from a streaming Anthropic response into
/// a final `TurnResponse`. Passed one event at a time by the SSE parser.
#[derive(Default)]
pub struct StreamAccumulator {
    text: String,
    tool_blocks: Vec<ToolBlock>,
    stop_reason: Option<StopReason>,
    usage: Usage,
}

#[derive(Debug)]
struct ToolBlock {
    name: String,
    /// Block index from `content_block_start.index` so streamed
    /// `input_json_delta` events with the same index can be appended in
    /// order even if Anthropic interleaves them with text deltas.
    index: usize,
    partial_input: String,
}

impl StreamAccumulator {
    /// Process one `StreamEvent`. `on_delta` is called synchronously
    /// with each text fragment so the SSE broadcaster can forward it
    /// to UI clients.
    pub fn handle(&mut self, event: StreamEvent, on_delta: &(dyn Fn(&str) + Send + Sync)) {
        // The accumulator's `on_delta` is a plain reference because it's
        // synchronous; the trait method takes the `Arc<DeltaSink>` and
        // dereferences once per chunk.
        match event {
            StreamEvent::ContentBlockStart { index, name } => {
                // Two index-collision cases to handle defensively:
                //   1) A second ContentBlockStart arrives for an index that
                //      already has a tool_block. Overwrite the name (if the
                //      new event carries a non-empty name) and reset the
                //      partial_input so the second block's deltas don't
                //      append onto stale bytes.
                //   2) A tool block initially arrives with empty name and
                //      a later event populates it. Register the block so
                //      subsequent ToolUseInput deltas can attach.
                if let Some(existing) = self.tool_blocks.iter_mut().find(|b| b.index == index) {
                    if !name.is_empty() {
                        existing.name = name;
                    }
                    existing.partial_input.clear();
                } else if !name.is_empty() {
                    self.tool_blocks.push(ToolBlock {
                        name,
                        index,
                        partial_input: String::new(),
                    });
                }
                // Empty-name first occurrence at an unseen index is a plain
                // text content block — its bytes arrive via ContentBlockDelta
                // and route to self.text; nothing to track here.
            }
            StreamEvent::ContentBlockDelta { text } => {
                on_delta(&text);
                self.text.push_str(&text);
            }
            StreamEvent::ToolUseInput { index, input_delta } => {
                if let Some(block) = self.tool_blocks.iter_mut().find(|b| b.index == index) {
                    block.partial_input.push_str(&input_delta);
                }
            }
            StreamEvent::ContentBlockStop { .. } => {
                // Block fully delivered; finalisation happens at MessageStop.
            }
            StreamEvent::MessageStop { stop_reason, usage } => {
                self.stop_reason = Some(stop_reason);
                // Real API: input/cache counts arrive on message_start, output_tokens
                // arrives on message_delta. Accumulate all four the same way so a
                // mock (or a future API change) that also emits input/cache on
                // message_delta adds correctly instead of overriding.
                self.usage.input_tokens =
                    self.usage.input_tokens.saturating_add(usage.input_tokens);
                self.usage.output_tokens =
                    self.usage.output_tokens.saturating_add(usage.output_tokens);
                self.usage.cache_creation_input_tokens = self
                    .usage
                    .cache_creation_input_tokens
                    .saturating_add(usage.cache_creation_input_tokens);
                self.usage.cache_read_input_tokens = self
                    .usage
                    .cache_read_input_tokens
                    .saturating_add(usage.cache_read_input_tokens);
            }
            StreamEvent::MessageStart { initial_usage } => {
                // Symmetric with MessageStop: saturating_add across all four
                // counters so a stray duplicate message_start (or a mock that
                // emits split start events) accumulates rather than discarding
                // prior input/cache totals.
                self.usage.input_tokens = self
                    .usage
                    .input_tokens
                    .saturating_add(initial_usage.input_tokens);
                self.usage.output_tokens = self
                    .usage
                    .output_tokens
                    .saturating_add(initial_usage.output_tokens);
                self.usage.cache_creation_input_tokens = self
                    .usage
                    .cache_creation_input_tokens
                    .saturating_add(initial_usage.cache_creation_input_tokens);
                self.usage.cache_read_input_tokens = self
                    .usage
                    .cache_read_input_tokens
                    .saturating_add(initial_usage.cache_read_input_tokens);
            }
            StreamEvent::ToolUseStart { .. } | StreamEvent::ToolUseStop { .. } => {}
        }
    }

    /// Consume the accumulator and produce the final `TurnResponse`.
    /// Fails when a tool block has unparseable JSON input.
    pub fn finalize(self) -> Result<TurnResponse> {
        let mut tool_uses: Vec<(Uuid, Tool)> = Vec::new();
        for block in self.tool_blocks {
            // An empty tool input is a valid no-arg tool call (e.g. get_session_state).
            let input: serde_json::Value = if block.partial_input.trim().is_empty() {
                serde_json::Value::Object(serde_json::Map::new())
            } else {
                serde_json::from_str(&block.partial_input).with_context(|| {
                    format!(
                        "parsing streamed tool input for '{}': {}",
                        block.name, block.partial_input
                    )
                })?
            };
            let tool = tool_from_api(&block.name, input)?;
            tool_uses.push((Uuid::new_v4(), tool));
        }
        Ok(TurnResponse {
            assistant_content: self.text,
            tool_uses,
            stop_reason: self.stop_reason.unwrap_or(StopReason::Other),
            usage: self.usage,
            request_metadata: super::client::RequestMetadata::default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::backend::LlmBackend;
    use super::super::client::TurnRequest;
    use super::super::delta_sink::DeltaSink;
    use super::*;
    use crate::tools::BatchableTool;
    use std::sync::Arc;

    fn collect_deltas() -> (DeltaSink, Arc<std::sync::Mutex<String>>) {
        let buf = Arc::new(std::sync::Mutex::new(String::new()));
        let buf_clone = buf.clone();
        let sink: DeltaSink = Arc::new(move |chunk: &str| {
            buf_clone.lock().unwrap().push_str(chunk);
        });
        (sink, buf)
    }

    #[tokio::test]
    async fn accumulator_assembles_text_only_response() {
        let mut acc = StreamAccumulator::default();
        let (sink, captured) = collect_deltas();
        let sink_ref: &(dyn Fn(&str) + Send + Sync) = sink.as_ref();
        acc.handle(
            StreamEvent::MessageStart {
                initial_usage: Usage::default(),
            },
            sink_ref,
        );
        acc.handle(
            StreamEvent::ContentBlockDelta {
                text: "Hello, ".into(),
            },
            sink_ref,
        );
        acc.handle(
            StreamEvent::ContentBlockDelta {
                text: "world!".into(),
            },
            sink_ref,
        );
        acc.handle(
            StreamEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
            sink_ref,
        );

        let resp = acc.finalize().unwrap();
        assert_eq!(resp.assistant_content, "Hello, world!");
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert!(resp.tool_uses.is_empty());
        // The on_delta callback fired for each chunk in order.
        assert_eq!(captured.lock().unwrap().clone(), "Hello, world!");
    }

    #[tokio::test]
    async fn accumulator_assembles_tool_use_with_streamed_input() {
        let mut acc = StreamAccumulator::default();
        let (sink, captured) = collect_deltas();
        let sink_ref: &(dyn Fn(&str) + Send + Sync) = sink.as_ref();

        acc.handle(
            StreamEvent::MessageStart {
                initial_usage: Usage::default(),
            },
            sink_ref,
        );
        acc.handle(
            StreamEvent::ContentBlockStart {
                index: 0,
                name: "classify_intake".into(),
            },
            sink_ref,
        );
        acc.handle(
            StreamEvent::ToolUseInput {
                index: 0,
                input_delta: "{\"prose\":".into(),
            },
            sink_ref,
        );
        acc.handle(
            StreamEvent::ToolUseInput {
                index: 0,
                input_delta: "\"single cell scRNA-seq\"}".into(),
            },
            sink_ref,
        );
        acc.handle(StreamEvent::ContentBlockStop { index: 0 }, sink_ref);
        acc.handle(
            StreamEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
            },
            sink_ref,
        );

        let resp = acc.finalize().unwrap();
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        assert_eq!(resp.tool_uses.len(), 1);
        match &resp.tool_uses[0].1 {
            Tool::Batchable(BatchableTool::ClassifyIntake { prose }) => {
                assert_eq!(prose, "single cell scRNA-seq");
            }
            other => panic!("expected ClassifyIntake, got {:?}", other),
        }
        // Tool input bytes should NOT have been streamed to the on_delta
        // callback (deltas are text-only).
        assert!(captured.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn accumulator_handles_empty_tool_input() {
        let mut acc = StreamAccumulator::default();
        let (sink, _) = collect_deltas();
        let sink_ref: &(dyn Fn(&str) + Send + Sync) = sink.as_ref();
        acc.handle(
            StreamEvent::ContentBlockStart {
                index: 0,
                name: "get_session_state".into(),
            },
            sink_ref,
        );
        acc.handle(StreamEvent::ContentBlockStop { index: 0 }, sink_ref);
        acc.handle(
            StreamEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
            },
            sink_ref,
        );
        let resp = acc.finalize().unwrap();
        // Zero-arg tools (get_session_state) must round-trip cleanly even
        // when the streamed input was empty.
        assert_eq!(resp.tool_uses.len(), 1);
        match &resp.tool_uses[0].1 {
            Tool::Batchable(BatchableTool::GetSessionState) => {}
            other => panic!("expected GetSessionState, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn accumulator_interleaved_text_and_tool_input() {
        // Anthropic may stream a text block followed by a tool block; both
        // share the same accumulator. Verify the text reaches the delta
        // sink and the tool input stays scoped to its own block.
        let mut acc = StreamAccumulator::default();
        let (sink, captured) = collect_deltas();
        let sink_ref: &(dyn Fn(&str) + Send + Sync) = sink.as_ref();

        acc.handle(
            StreamEvent::MessageStart {
                initial_usage: Usage::default(),
            },
            sink_ref,
        );
        // text content block at index 0
        acc.handle(
            StreamEvent::ContentBlockStart {
                index: 0,
                name: String::new(),
            },
            sink_ref,
        );
        acc.handle(
            StreamEvent::ContentBlockDelta {
                text: "Looking up classification…\n".into(),
            },
            sink_ref,
        );
        acc.handle(StreamEvent::ContentBlockStop { index: 0 }, sink_ref);
        // tool_use content block at index 1
        acc.handle(
            StreamEvent::ContentBlockStart {
                index: 1,
                name: "classify_intake".into(),
            },
            sink_ref,
        );
        acc.handle(
            StreamEvent::ToolUseInput {
                index: 1,
                input_delta: "{\"prose\":\"x\"}".into(),
            },
            sink_ref,
        );
        acc.handle(StreamEvent::ContentBlockStop { index: 1 }, sink_ref);
        acc.handle(
            StreamEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
            },
            sink_ref,
        );

        let resp = acc.finalize().unwrap();
        assert_eq!(resp.assistant_content, "Looking up classification…\n");
        assert_eq!(resp.tool_uses.len(), 1);
        assert_eq!(
            captured.lock().unwrap().clone(),
            "Looking up classification…\n"
        );
    }

    #[tokio::test]
    async fn accumulator_merges_message_start_usage_with_message_delta_output() {
        // Regression: on a real Anthropic stream, input_tokens + cache
        // counts arrive on message_start; output_tokens arrives on
        // message_delta. The accumulator must merge the two so the
        // /metrics endpoint surfaces non-zero input tokens.
        let mut acc = StreamAccumulator::default();
        let (sink, _) = collect_deltas();
        let sink_ref: &(dyn Fn(&str) + Send + Sync) = sink.as_ref();

        acc.handle(
            StreamEvent::MessageStart {
                initial_usage: Usage {
                    input_tokens: 1234,
                    output_tokens: 0,
                    cache_creation_input_tokens: 800,
                    cache_read_input_tokens: 400,
                },
            },
            sink_ref,
        );
        acc.handle(
            StreamEvent::ContentBlockDelta {
                text: "hello".into(),
            },
            sink_ref,
        );
        acc.handle(
            StreamEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 0,
                    output_tokens: 47,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                },
            },
            sink_ref,
        );

        let resp = acc.finalize().unwrap();
        assert_eq!(resp.usage.input_tokens, 1234);
        assert_eq!(resp.usage.output_tokens, 47);
        assert_eq!(resp.usage.cache_creation_input_tokens, 800);
        assert_eq!(resp.usage.cache_read_input_tokens, 400);
    }

    #[tokio::test]
    async fn accumulator_adds_cache_tokens_across_events_not_overrides() {
        // Regression: previously the accumulator used `if x > 0 { self.x = x }`
        // on cache_creation_input_tokens and cache_read_input_tokens, which
        // overrode message_start counts if any downstream event (mock or
        // future API change) also emitted cache tokens. Now it saturating_adds
        // across events, matching output_tokens handling.
        let mut acc = StreamAccumulator::default();
        let (sink, _) = collect_deltas();
        let sink_ref: &(dyn Fn(&str) + Send + Sync) = sink.as_ref();

        acc.handle(
            StreamEvent::MessageStart {
                initial_usage: Usage {
                    input_tokens: 100,
                    output_tokens: 0,
                    cache_creation_input_tokens: 200,
                    cache_read_input_tokens: 50,
                },
            },
            sink_ref,
        );
        acc.handle(
            StreamEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 5,
                    output_tokens: 42,
                    cache_creation_input_tokens: 10,
                    cache_read_input_tokens: 7,
                },
            },
            sink_ref,
        );

        let resp = acc.finalize().unwrap();
        assert_eq!(resp.usage.input_tokens, 105);
        assert_eq!(resp.usage.output_tokens, 42);
        assert_eq!(resp.usage.cache_creation_input_tokens, 210);
        assert_eq!(resp.usage.cache_read_input_tokens, 57);
    }

    #[tokio::test]
    async fn default_streaming_impl_falls_back_to_send_turn() {
        // MockLlmBackend overrides `send_turn_streaming` rather than
        // inheriting the default trait impl, so the streaming path
        // doesn't drop tool_uses on mock-driven turns. For text-only
        // responses the behaviour is unchanged: the assistant_content
        // arrives as a single delta on the sink.
        let mock = crate::mock::MockLlmBackend::new(vec![TurnResponse {
            assistant_content: "default-impl text".into(),
            tool_uses: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),

            request_metadata: Default::default(),
        }]);

        let (sink, captured) = collect_deltas();
        let req = TurnRequest {
            system_prompt: vec![],
            conversation: std::sync::Arc::new(vec![]),
            tool_schemas: vec![],
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.0,
            max_tokens: 100,
            tool_exchange: vec![],
            tool_choice: None,
        };
        let resp = mock.send_turn_streaming(req, sink).await.unwrap();
        assert_eq!(resp.assistant_content, "default-impl text");
        assert_eq!(captured.lock().unwrap().clone(), "default-impl text");
    }
}
