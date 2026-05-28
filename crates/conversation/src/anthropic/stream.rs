//! SSE parsing domain — the `StreamEvent` enum the accumulator consumes,
//! the `parse_stream_chunk` line parser that translates Anthropic SSE
//! events into it, and the `synthesize_stream` helper used by the offline
//! / mock path so the server can still emit tool-call boundaries even
//! when the underlying backend is non-streaming.

use super::client::{StopReason, TurnResponse, Usage};
use anyhow::{Context, Result};

/// Streaming events surfaced to the server. The real Anthropic SSE
/// feed is parsed through `parse_stream_chunk`; the offline mock and
/// non-streaming path can synthesize these from a complete response.
///
/// Anthropic correlates streamed blocks by `index` (an integer per content
/// block within the response), so the index-based variants are what the
/// `StreamAccumulator` actually consumes. The legacy id-based variants
/// (`ToolUseStart` / `ToolUseStop`) are kept so existing call sites and
/// tests that expect tool-id strings still build.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// Anthropic's `message_start` event carries the initial `usage` block:
    /// `input_tokens`, `cache_creation_input_tokens`, `cache_read_input_tokens`.
    /// Output tokens arrive later on `message_delta`, so the accumulator
    /// stashes `initial_usage` here and merges it with the final output
    /// count at `MessageStop`.
    MessageStart {
        /// Initial token usage from the `message_start` event.
        initial_usage: Usage,
    },
    /// New `content_block_start` event surfaced with the block index and
    /// (when the block is a tool_use) the tool name. Used by the
    /// `StreamAccumulator` to start a new tool block.
    ContentBlockStart {
        /// Zero-based index of the content block.
        index: usize,
        /// Tool name for `tool_use` blocks; empty string for text blocks.
        name: String,
    },
    /// Text delta for a content block. The accumulator appends this to the
    /// running assistant text and the live `on_delta` callback emits it to
    /// the SSE broadcaster.
    ContentBlockDelta {
        /// Incremental text fragment to append.
        text: String,
    },
    /// Streaming partial JSON for a tool_use input field. Correlated by
    /// block `index` so multiple interleaved blocks get appended to the
    /// right buffer.
    ToolUseInput {
        /// Zero-based index of the tool_use block.
        index: usize,
        /// Incremental JSON fragment for the tool input.
        input_delta: String,
    },
    /// `content_block_stop` event with the block index. The accumulator
    /// uses this as a terminal marker per block; legacy server code that
    /// only cared about a generic stop signal still sees `ToolUseStop`.
    ContentBlockStop {
        /// Zero-based index of the completed content block.
        index: usize,
    },
    /// Legacy id-based variants — kept for source compat with the
    /// pre-streaming wiring and existing tests. The accumulator ignores
    /// them.
    ToolUseStart {
        /// Tool name from the streaming event.
        tool_name: String,
        /// Tool call UUID from the streaming event.
        tool_id: String,
    },
    /// Legacy id-based tool stop event.
    ToolUseStop {
        /// Tool call UUID that stopped.
        tool_id: String,
    },
    /// Final event: carries stop reason and final token usage.
    MessageStop {
        /// Why the model stopped generating.
        stop_reason: StopReason,
        /// Final usage including output tokens.
        usage: Usage,
    },
}

/// Parse a single Anthropic SSE event line into zero or more `StreamEvent`s.
///
/// Anthropic's SSE stream emits one event per line as `data: {json}` separated
/// by `event: <type>` headers. Callers buffer lines and pass each `data:` JSON
/// payload to this function as they arrive.
pub fn parse_stream_chunk(json: &str) -> Result<Vec<StreamEvent>> {
    let v: serde_json::Value =
        serde_json::from_str(json).with_context(|| format!("parsing SSE chunk: {}", json))?;
    let kind = v["type"].as_str().unwrap_or("");
    let mut out = Vec::new();
    match kind {
        "message_start" => {
            let u = &v["message"]["usage"];
            let initial_usage = Usage {
                input_tokens: u["input_tokens"].as_u64().unwrap_or(0) as u32,
                output_tokens: u["output_tokens"].as_u64().unwrap_or(0) as u32,
                cache_creation_input_tokens: u["cache_creation_input_tokens"].as_u64().unwrap_or(0)
                    as u32,
                cache_read_input_tokens: u["cache_read_input_tokens"].as_u64().unwrap_or(0) as u32,
            };
            out.push(StreamEvent::MessageStart { initial_usage });
        }
        "content_block_start" => {
            let index = v["index"].as_u64().unwrap_or(0) as usize;
            if let Some(block) = v.get("content_block") {
                let block_type = block["type"].as_str().unwrap_or("");
                if block_type == "tool_use" {
                    let tool_name = block["name"].as_str().unwrap_or("").to_string();
                    let tool_id = block["id"].as_str().unwrap_or("").to_string();
                    // Surface BOTH the new index-based and the legacy
                    // id-based variants so old consumers (tests, server
                    // tool-pill wiring) still work while the accumulator
                    // gets what it needs.
                    out.push(StreamEvent::ContentBlockStart {
                        index,
                        name: tool_name.clone(),
                    });
                    out.push(StreamEvent::ToolUseStart { tool_name, tool_id });
                } else {
                    // Plain text content block — the accumulator doesn't
                    // need a start marker for it but emit it for completeness.
                    out.push(StreamEvent::ContentBlockStart {
                        index,
                        name: String::new(),
                    });
                }
            }
        }
        "content_block_delta" => {
            let index = v["index"].as_u64().unwrap_or(0) as usize;
            if let Some(delta) = v.get("delta") {
                let dtype = delta["type"].as_str().unwrap_or("");
                match dtype {
                    "text_delta" => {
                        if let Some(text) = delta["text"].as_str() {
                            out.push(StreamEvent::ContentBlockDelta { text: text.into() });
                        }
                    }
                    "input_json_delta" => {
                        // Per-tool partial JSON correlated by content block
                        // index. The accumulator appends each delta to the
                        // matching ToolBlock buffer.
                        if let Some(s) = delta["partial_json"].as_str() {
                            out.push(StreamEvent::ToolUseInput {
                                index,
                                input_delta: s.into(),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        "content_block_stop" => {
            let index = v["index"].as_u64().unwrap_or(0) as usize;
            out.push(StreamEvent::ContentBlockStop { index });
            // Keep emitting the legacy ToolUseStop too so any consumer that
            // listens for it still fires.
            out.push(StreamEvent::ToolUseStop {
                tool_id: String::new(),
            });
        }
        "message_delta" => {
            if let Some(delta) = v.get("delta") {
                let stop_reason = match delta["stop_reason"].as_str() {
                    Some("end_turn") => StopReason::EndTurn,
                    Some("tool_use") => StopReason::ToolUse,
                    Some("max_tokens") => StopReason::MaxTokens,
                    _ => StopReason::Other,
                };
                let usage = Usage {
                    input_tokens: 0,
                    output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0) as u32,
                    cache_creation_input_tokens: 0,
                    cache_read_input_tokens: 0,
                };
                out.push(StreamEvent::MessageStop { stop_reason, usage });
            }
        }
        "message_stop" | "ping" | "" => {}
        _ => {}
    }
    Ok(out)
}

/// Helper: synthesize a stream of `StreamEvent`s from a fully-baked
/// `TurnResponse`. Used by the offline / mock path so the server can still
/// emit tool-call boundaries even when the underlying backend is non-streaming.
///
/// Emits a `ContentBlockStart { index, name }` event for each tool_use
/// block. The `StreamAccumulator` only registers a tool block on
/// `ContentBlockStart`; an id-based-only `ToolUseStart` (which the
/// accumulator ignores) leaves an empty `tool_uses` Vec on
/// `finalize()`. Emitting the index-based event matches the real
/// `parse_stream_chunk` shape and round-trips through the accumulator.
pub fn synthesize_stream(resp: &TurnResponse) -> Vec<StreamEvent> {
    // Synthesized streams carry the already-baked usage at MessageStop;
    // MessageStart's initial_usage is left at Default because the
    // non-streaming response block doesn't split usage across events.
    let mut events = vec![StreamEvent::MessageStart {
        initial_usage: Usage::default(),
    }];
    let mut next_index: usize = 0;
    if !resp.assistant_content.is_empty() {
        // Text content block at index 0. Emit ContentBlockStart with an
        // empty name (the accumulator's text path doesn't need a tool
        // name) so any consumer that tracks block ordering by index
        // observes the same sequence as the real SSE stream.
        events.push(StreamEvent::ContentBlockStart {
            index: next_index,
            name: String::new(),
        });
        events.push(StreamEvent::ContentBlockDelta {
            text: resp.assistant_content.clone(),
        });
        events.push(StreamEvent::ContentBlockStop { index: next_index });
        next_index += 1;
    }
    for (i, (_id, tool)) in resp.tool_uses.iter().enumerate() {
        let tool_id = format!("synth-{}", i);
        let block_index = next_index;
        // Index-based ContentBlockStart so the `StreamAccumulator`
        // registers the tool block. The legacy id-based variants are
        // emitted alongside for any consumer that still listens on them.
        events.push(StreamEvent::ContentBlockStart {
            index: block_index,
            name: tool.name().to_string(),
        });
        // Serialize the tool input through the closed `Tool` vocabulary
        // so the accumulator's `finalize()` can parse it back into a
        // `Tool` variant. An empty object is valid for zero-arg tools
        // (e.g. `get_session_state`); anything richer round-trips via
        // serde's standard derivation.
        let input_json = serde_json::to_string(tool).unwrap_or_else(|_| "{}".to_string());
        events.push(StreamEvent::ToolUseInput {
            index: block_index,
            input_delta: input_json,
        });
        events.push(StreamEvent::ToolUseStart {
            tool_name: tool.name().to_string(),
            tool_id: tool_id.clone(),
        });
        events.push(StreamEvent::ContentBlockStop { index: block_index });
        events.push(StreamEvent::ToolUseStop { tool_id });
        next_index += 1;
    }
    events.push(StreamEvent::MessageStop {
        stop_reason: resp.stop_reason,
        usage: resp.usage,
    });
    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{BatchableTool, Tool};
    use uuid::Uuid;

    #[test]
    fn parse_text_delta_chunk() {
        let chunk = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let events = parse_stream_chunk(chunk).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::ContentBlockDelta { text } => assert_eq!(text, "Hello"),
            _ => panic!("expected ContentBlockDelta"),
        }
    }

    #[test]
    fn parse_tool_use_start_chunk() {
        let chunk = r#"{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_abc","name":"classify_intake","input":{}}}"#;
        let events = parse_stream_chunk(chunk).unwrap();
        // parse_stream_chunk now emits BOTH the new
        // index-based ContentBlockStart AND the legacy id-based
        // ToolUseStart so old consumers and the new accumulator both work.
        assert_eq!(events.len(), 2);
        match &events[0] {
            StreamEvent::ContentBlockStart { index, name } => {
                assert_eq!(*index, 1);
                assert_eq!(name, "classify_intake");
            }
            other => panic!("expected ContentBlockStart, got {:?}", other),
        }
        match &events[1] {
            StreamEvent::ToolUseStart { tool_name, tool_id } => {
                assert_eq!(tool_name, "classify_intake");
                assert_eq!(tool_id, "toolu_abc");
            }
            other => panic!("expected ToolUseStart, got {:?}", other),
        }
    }

    #[test]
    fn parse_message_start_captures_initial_usage() {
        // Anthropic surfaces input + cache tokens on `message_start`,
        // not `message_delta`. Verify the parser captures them so the
        // Metrics tab shows non-zero input_tokens on live sessions.
        let chunk = r#"{"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"claude","content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":1234,"output_tokens":1,"cache_creation_input_tokens":800,"cache_read_input_tokens":400}}}"#;
        let events = parse_stream_chunk(chunk).unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            StreamEvent::MessageStart { initial_usage } => {
                assert_eq!(initial_usage.input_tokens, 1234);
                assert_eq!(initial_usage.cache_creation_input_tokens, 800);
                assert_eq!(initial_usage.cache_read_input_tokens, 400);
            }
            other => panic!("expected MessageStart, got {:?}", other),
        }
    }

    #[test]
    fn parse_message_delta_with_stop_reason() {
        let chunk = r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":12}}"#;
        let events = parse_stream_chunk(chunk).unwrap();
        match &events[0] {
            StreamEvent::MessageStop { stop_reason, .. } => {
                assert_eq!(*stop_reason, StopReason::EndTurn);
            }
            _ => panic!("expected MessageStop"),
        }
    }

    #[test]
    fn parse_ping_is_silent() {
        let events = parse_stream_chunk(r#"{"type":"ping"}"#).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn parse_malformed_chunk_errors() {
        let r = parse_stream_chunk("{not json");
        assert!(r.is_err());
    }

    #[test]
    fn synthesize_stream_from_text_only_response() {
        let resp = TurnResponse {
            assistant_content: "Hi there".into(),
            tool_uses: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),

            request_metadata: Default::default(),
        };
        let events = synthesize_stream(&resp);
        assert!(matches!(events[0], StreamEvent::MessageStart { .. }));
        // Text content opens with ContentBlockStart so the
        // accumulator observes the same boundary it would see in real SSE.
        assert!(matches!(
            events[1],
            StreamEvent::ContentBlockStart { index: 0, ref name } if name.is_empty()
        ));
        assert!(events
            .iter()
            .any(|e| matches!(e, StreamEvent::ContentBlockDelta { text } if text == "Hi there")));
        assert!(matches!(
            events.last(),
            Some(StreamEvent::MessageStop { .. })
        ));
    }

    #[test]
    fn synthesize_stream_from_tool_use_response() {
        let resp = TurnResponse {
            assistant_content: String::new(),
            tool_uses: vec![(
                Uuid::new_v4(),
                Tool::Batchable(BatchableTool::ClassifyIntake { prose: "x".into() }),
            )],
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),

            request_metadata: Default::default(),
        };
        let events = synthesize_stream(&resp);
        let starts = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::ToolUseStart { .. }))
            .count();
        let stops = events
            .iter()
            .filter(|e| matches!(e, StreamEvent::ToolUseStop { .. }))
            .count();
        assert_eq!(starts, 1);
        assert_eq!(stops, 1);
        // ContentBlockStart with the tool name is what the
        // accumulator needs to register the tool block. Without this
        // event the mock-streaming path produced an empty tool_uses Vec
        // even when the underlying TurnResponse carried a tool call.
        let block_starts: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                StreamEvent::ContentBlockStart { index, name } if !name.is_empty() => {
                    Some((*index, name.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(block_starts.len(), 1);
        assert_eq!(block_starts[0].1, "classify_intake");
    }

    /// End-to-end — feed a synthesized stream through the
    /// accumulator and verify the tool call survives. Regression
    /// guard for the synthesized stream's `ContentBlockStart`
    /// emission: without it the accumulator never registers the
    /// tool block and `finalize()` returns an empty `tool_uses`
    /// Vec even though the source `TurnResponse` carried one.
    #[test]
    fn synthesized_stream_round_trips_tool_use_through_accumulator() {
        use crate::anthropic::accumulator::StreamAccumulator;
        let resp = TurnResponse {
            assistant_content: String::new(),
            tool_uses: vec![(
                Uuid::new_v4(),
                Tool::Batchable(BatchableTool::ClassifyIntake {
                    prose: "single-cell variant analysis".into(),
                }),
            )],
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
            request_metadata: Default::default(),
        };
        let events = synthesize_stream(&resp);
        let mut acc = StreamAccumulator::default();
        let sink: std::sync::Arc<dyn Fn(&str) + Send + Sync> = std::sync::Arc::new(|_| {});
        for ev in events {
            acc.handle(ev, sink.as_ref());
        }
        let out = acc.finalize().expect("accumulator finalize");
        assert_eq!(out.tool_uses.len(), 1);
        match &out.tool_uses[0].1 {
            Tool::Batchable(BatchableTool::ClassifyIntake { prose }) => {
                assert_eq!(prose, "single-cell variant analysis");
            }
            other => panic!("expected ClassifyIntake, got {:?}", other),
        }
    }
}
