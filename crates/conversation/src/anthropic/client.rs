//! The `AnthropicClient` HTTP + SSE client, the `TurnRequest` /
//! `TurnResponse` message shapes, and the shared `StopReason` / `Usage`
//! primitives they carry. Also hosts the Anthropic-specific JSON parsing
//! (`parse_response`, `ApiResponse` family) and the `build_messages_payload`
//! helper used by both the non-streaming and streaming POST paths.

use super::accumulator::StreamAccumulator;
use super::backend::LlmBackend;
use super::delta_sink::DeltaSink;
use super::stream::parse_stream_chunk;
use crate::session::Turn;
use crate::tools::Tool;
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use ecaa_workflow_core::resilient_client::{ResilientClient, ResilientClientConfig};
use serde::Deserialize;
use uuid::Uuid;

/// hard cap on the unparsed SSE buffer. Exceeded means either
/// Anthropic is streaming multi-MB tool-call arguments (pathological)
/// or the stream is malformed. Either way, fail-fast beats OOM. The
/// 50 MB budget is ~100× a typical verbose response so normal traffic
/// never trips it.
const SSE_BUFFER_MAX_BYTES: usize = 50 * 1024 * 1024;

/// Anthropic beta header value that enables the server-side context
/// editing feature (`clear_tool_uses_20250919`).
///
/// Exposed as `pub` so the `documented_constants` integration test can
/// assert the header value matches the CLAUDE.md claim.
pub const CONTEXT_MANAGEMENT_BETA: &str = "context-management-2025-06-27";

/// Default HTTP timeout for the Anthropic Messages API client. Plan
/// D-R10 / S2.7 — 180s gives Sonnet/Opus + tool loop enough headroom
/// without the previous 120s ceiling that masked stalls as timeouts on
/// long generations. Override via `ECAA_ANTHROPIC_TIMEOUT_SECS=<n>`.
pub const DEFAULT_ANTHROPIC_TIMEOUT_SECS: u64 = 180;

fn anthropic_client_timeout() -> std::time::Duration {
    let secs = std::env::var("ECAA_ANTHROPIC_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_ANTHROPIC_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

/// Magic substring planted in the error message when the reqwest
/// request-body timeout fires. Lets `service::retry::classify_retriable`
/// distinguish a clean Anthropic-side stall (terminal: surface to user
/// instead of burning two retry attempts on a likely-still-hung backend)
/// from generic transient connection blips (retriable). The token is a
/// hyphenated literal so an unrelated body containing the words "request"
/// and "timeout" can't accidentally match.
pub const REQUEST_BODY_TIMEOUT_MARKER: &str = "anthropic-request-body-timeout";

/// Wrap a `reqwest::Error` from `.send().await` so a body-timeout failure
/// carries the marker the retry classifier reads. `is_timeout()` is true
/// for both connect-phase and request-body-phase timeouts; either way we
/// classify as terminal here — connect-phase timeouts to api.anthropic.com
/// from a healthy box are vanishingly rare and almost always indicate the
/// op is broader than a turn-level retry can fix.
fn wrap_send_error(err: reqwest::Error, url: &str, streaming: bool) -> anyhow::Error {
    if err.is_timeout() {
        let secs = anthropic_client_timeout().as_secs();
        let suffix = if streaming { " (streaming)" } else { "" };
        anyhow!(
            "{} after {}s on POST {}{}: {}",
            REQUEST_BODY_TIMEOUT_MARKER,
            secs,
            url,
            suffix,
            err
        )
    } else {
        let suffix = if streaming { " (streaming)" } else { "" };
        anyhow::Error::new(err).context(format!("POST {}{}", url, suffix))
    }
}

/// Context-management trigger threshold: once this many tool-use exchanges
/// accumulate, Anthropic's server-side editor clears the oldest tool_result
/// blocks. Exposed for the documented-constants test.
pub const CONTEXT_MGMT_TRIGGER_TOOL_USES: u32 = 8;

/// How many tool-use exchanges the editor preserves (the most recent N).
/// Exposed for the documented-constants test.
pub const CONTEXT_MGMT_KEEP_TOOL_USES: u32 = 4;

/// True if context editing is enabled (the default). The
/// `ECAA_DISABLE_CONTEXT_EDITING=1` escape hatch turns it off for A/B
/// comparison or if the beta ever regresses.
pub(crate) fn context_editing_enabled() -> bool {
    std::env::var("ECAA_DISABLE_CONTEXT_EDITING")
        .ok()
        .as_deref()
        != Some("1")
}

/// Guarded payload dumper.
///
/// `ECAA_DUMP_ANTHROPIC_PAYLOAD` writes the request body the chat is
/// about to send to Anthropic to a local file. Useful for debugging
/// "schema too complex" / cache-prefix mismatches, but the file
/// contains the system prompt, tool catalog, and any tool-result
/// payloads already in the conversation — so:
///
/// 1. The dump is gated on `ECAA_DEBUG=1` so an operator can't dump
///    accidentally / surreptitiously. A warning is logged when the
///    dump path is set but `ECAA_DEBUG` isn't.
/// 2. The file is created with mode 0600 so a co-tenant on the same
///    Unix host can't read it. (Default mode is 0644 on most distros
///    via umask 022.)
fn dump_anthropic_payload(dump_path: &str, body: &serde_json::Value) {
    let debug_enabled = std::env::var("ECAA_DEBUG")
        .map(|v| v == "1" || v == "true")
        .unwrap_or(false);
    if !debug_enabled {
        tracing::warn!(
            "ECAA_DUMP_ANTHROPIC_PAYLOAD={dump_path} set but ECAA_DEBUG=1 not set; ignoring \
             (the dump may contain prompt + tool-result payloads, only enabled under explicit debug)"
        );
        return;
    }
    let path = std::path::PathBuf::from(dump_path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let serialized = serde_json::to_string_pretty(body).unwrap_or_default();
    // 0600 mode on unix; on windows there's no equivalent and we
    // fall back to the default mode silently.
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(&path) {
        Ok(mut f) => {
            use std::io::Write;
            if let Err(e) = f.write_all(serialized.as_bytes()) {
                tracing::warn!(
                    "ECAA_DUMP_ANTHROPIC_PAYLOAD: write to {} failed: {e}",
                    path.display()
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                "ECAA_DUMP_ANTHROPIC_PAYLOAD: opening {} (mode 0600) failed: {e}",
                path.display()
            );
        }
    }
}

/// Inject the `context_management` request field that Anthropic's beta
/// expects. `trigger.value = 8` keeps up to 8 tool exchanges verbatim
/// before the server clears the oldest; `keep.value = 4` pins the four
/// most recent. Sized for a tool loop where typical turns settle under
/// 8 iterations (so the editor rarely fires) but pathological turns
/// still cap.
fn apply_context_management(body: &mut serde_json::Value) {
    if !context_editing_enabled() {
        return;
    }
    if let Some(obj) = body.as_object_mut() {
        obj.insert(
            "context_management".into(),
            serde_json::json!({
                "edits": [{
                    "type": "clear_tool_uses_20250919",
                    "trigger": { "type": "tool_uses", "value": CONTEXT_MGMT_TRIGGER_TOOL_USES },
                    "keep": { "type": "tool_uses", "value": CONTEXT_MGMT_KEEP_TOOL_USES },
                    "clear_tool_inputs": true,
                }],
            }),
        );
    }
}

/// All inputs needed to send a single Anthropic Messages API request.
#[derive(Debug, Clone)]
pub struct TurnRequest {
    /// Ordered system-prompt blocks (with cache-control metadata).
    pub system_prompt: Vec<crate::prompt::SystemPromptBlock>,
    /// `Arc<Vec<Turn>>` so the tool-use loop can share the
    /// entire prior transcript across iterations without cloning the
    /// Vec each time. Session mutations still happen on a plain Vec
    /// on the Session type; this Arc wraps the *request-time snapshot*
    /// that gets re-sent to Anthropic for every iteration of one
    /// user turn.
    pub conversation: std::sync::Arc<Vec<Turn>>,
    /// JSON schema objects for all tools the model may call.
    pub tool_schemas: Vec<serde_json::Value>,
    /// Model to use for this request.
    pub model: crate::model_policy::ModelId,
    /// Sampling temperature (usually 0 for deterministic tool routing).
    pub temperature: f32,
    /// Maximum tokens allowed in the response.
    pub max_tokens: u32,
    /// Raw tool_use / tool_result message pairs accumulated during the
    /// current tool loop. Appended after the base conversation messages
    /// so the model sees its own tool calls and their results in the
    /// correct Anthropic Messages API format.
    #[allow(clippy::type_complexity)]
    pub tool_exchange: Vec<serde_json::Value>,
    /// Anthropic Messages `tool_choice` override. `None` leaves the
    /// default (`{"type":"auto"}` — model is free to call any tool or
    /// none). `Some(ToolChoice::Tool(name))` forces the model to emit
    /// exactly one `tool_use` block for the named tool before composing
    /// its final response. Used by the tool-loop on the first Intake
    /// turn to mandate `classify_intake` — without this guardrail Sonnet
    /// drifts to conversational acknowledgment + fabricates "backend
    /// classifier issue" excuses when long technical prompts arrive.
    pub tool_choice: Option<ToolChoice>,
}

/// Anthropic Messages `tool_choice` override. Currently we only need
/// the named-tool form (`{"type":"tool","name":"<name>"}`); the
/// `{"type":"any"}` and `{"type":"none"}` variants are not exercised
/// by the tool-loop today.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolChoice {
    /// Force the model to call exactly one named tool before responding.
    Tool(String),
}

impl ToolChoice {
    /// Render as the JSON payload Anthropic Messages expects.
    pub fn to_anthropic_payload(&self) -> serde_json::Value {
        match self {
            ToolChoice::Tool(name) => serde_json::json!({
                "type": "tool",
                "name": name,
            }),
        }
    }
}

/// Per-request observability captured off Anthropic response headers.
/// `request_id` is the `anthropic-request-id` (essential for support
/// tickets); the `ratelimit_*` triple is what AWS-side capacity
/// planning + the per-session UI Performance tab consume so an
/// operator can spot a session drifting toward the per-minute cap
/// before it 429s. All optional — older Anthropic responses + the
/// MockLlmBackend leave these unset.
#[derive(Debug, Clone, Default)]
pub struct RequestMetadata {
    /// `anthropic-request-id` response header; required for Anthropic support tickets.
    pub request_id: Option<String>,
    /// Remaining requests in the current rate-limit window.
    pub ratelimit_requests_remaining: Option<u32>,
    /// Remaining input tokens in the current rate-limit window.
    pub ratelimit_input_tokens_remaining: Option<u32>,
    /// Remaining output tokens in the current rate-limit window.
    pub ratelimit_output_tokens_remaining: Option<u32>,
}

/// Parsed response from a single Anthropic Messages API call.
#[derive(Debug, Clone)]
pub struct TurnResponse {
    /// Text content from the assistant's response (may be empty when the
    /// response was purely tool-use blocks).
    pub assistant_content: String,
    /// Tool calls parsed out of the response, as `(call_id, Tool)` pairs.
    pub tool_uses: Vec<(Uuid, Tool)>,
    /// Why the model stopped generating.
    pub stop_reason: StopReason,
    /// Token counts for this request/response pair.
    pub usage: Usage,
    /// Captured once per `send_turn`; present on the `AnthropicClient`
    /// path, default-empty on `MockLlmBackend`. Plan S2.8 — surfaces
    /// `anthropic-request-id` for Anthropic-side debugging and the
    /// `x-ratelimit-remaining-*` triple for capacity planning.
    pub request_metadata: RequestMetadata,
}

/// Reason the model stopped generating tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The model reached a natural stopping point.
    EndTurn,
    /// The model emitted a `tool_use` block; the tool loop should
    /// dispatch the call and re-prompt.
    ToolUse,
    /// The response was truncated at `max_tokens`; the turn may be
    /// incomplete.
    MaxTokens,
    /// Any stop reason not covered by the above variants.
    Other,
}

/// Token usage breakdown from an Anthropic Messages API response.
#[derive(Debug, Clone, Copy, Default)]
pub struct Usage {
    /// Number of input tokens billed for this request.
    pub input_tokens: u32,
    /// Number of output tokens generated.
    pub output_tokens: u32,
    /// Input tokens written into the prompt cache (billed at a higher write rate).
    pub cache_creation_input_tokens: u32,
    /// Input tokens read from the prompt cache (billed at a lower read rate).
    pub cache_read_input_tokens: u32,
}

/// HTTP client for the Anthropic Messages API. Wraps `ResilientClient`
/// for HTTPS-scheme enforcement and timeout handling.
pub struct AnthropicClient {
    api_key: String,
    /// Wrapped via `ResilientClient` to enforce the HTTPS-scheme guard:
    /// any misconfigured `ANTHROPIC_BASE_URL=http://...` on a non-loopback
    /// host is rejected at construction time rather than silently sending
    /// credentials over an insecure channel.
    resilient: ResilientClient,
}

impl AnthropicClient {
    /// Construct from `ECAA_ANTHROPIC_API_KEY` (or legacy `ANTHROPIC_API_KEY`)
    /// and the optional `ANTHROPIC_BASE_URL` override.
    pub fn new() -> Result<Self> {
        let api_key = crate::anthropic_api_key()
            .context("ECAA_ANTHROPIC_API_KEY required for AnthropicClient::new — set the env var or use MockLlmBackend (legacy ANTHROPIC_API_KEY is also accepted with a deprecation warning)")?;
        let raw_base = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".into());
        let base_url = url::Url::parse(&raw_base)
            .with_context(|| format!("parsing ANTHROPIC_BASE_URL: {raw_base:?}"))?;
        let rc_cfg = ResilientClientConfig {
            base_url,
            timeout: anthropic_client_timeout(),
            user_agent: format!("ecaa-workflow/{}", env!("CARGO_PKG_VERSION")),
        };
        let resilient =
            ResilientClient::new(rc_cfg).map_err(|e| anyhow!("building ResilientClient: {e}"))?;
        Ok(Self { api_key, resilient })
    }

    /// Override the Anthropic API base URL. Used in tests to point
    /// at a mock server or a local proxy.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Result<Self> {
        let raw = base_url.into();
        let parsed = url::Url::parse(&raw).with_context(|| format!("parsing base_url: {raw:?}"))?;
        let rc_cfg = ResilientClientConfig {
            base_url: parsed,
            timeout: anthropic_client_timeout(),
            user_agent: format!("ecaa-workflow/{}", env!("CARGO_PKG_VERSION")),
        };
        self.resilient =
            ResilientClient::new(rc_cfg).map_err(|e| anyhow!("building ResilientClient: {e}"))?;
        Ok(self)
    }
}

#[async_trait]
impl LlmBackend for AnthropicClient {
    #[tracing::instrument(
        skip(self, req),
        fields(
            model = ?req.model,
            msg_count = req.conversation.len(),
            max_tokens = req.max_tokens,
        )
    )]
    async fn send_turn(&self, req: TurnRequest) -> Result<TurnResponse> {
        let mut body = build_messages_payload(&req);
        apply_context_management(&mut body);
        // `Url` Display always emits a trailing slash for the root path
        // (`https://api.anthropic.com/`), so `format!("{}/v1/messages", ...)`
        // produces `https://api.anthropic.com//v1/messages` which
        // Cloudflare 404s. Use `.join()` to compose paths safely.
        let url = self
            .resilient
            .base_url()
            .join("v1/messages")
            .map(|u| u.to_string())
            .unwrap_or_else(|_| format!("{}v1/messages", self.resilient.base_url()));
        let mut post = self
            .resilient
            .inner()
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");
        if context_editing_enabled() {
            post = post.header("anthropic-beta", CONTEXT_MANAGEMENT_BETA);
        }
        let resp = post
            .json(&body)
            .send()
            .await
            .map_err(|e| wrap_send_error(e, &url, false))?;

        let status = resp.status();
        // Pull observability headers BEFORE consuming the body. Per
        // S2.8: anthropic-request-id is needed for support tickets;
        // the rate-limit triple feeds Performance-tab capacity
        // planning so the SME can spot exhaustion before a 429.
        let req_meta = capture_request_metadata(resp.headers());
        let text = resp.text().await.context("reading response body")?;

        if !status.is_success() {
            // Translate 429 into a backoff signal the caller can reason about.
            if status.as_u16() == 429 {
                return Err(anyhow!("anthropic API rate-limited (HTTP 429): {}", text));
            }
            return Err(anyhow!(
                "anthropic API error (HTTP {}): {}",
                status.as_u16(),
                text
            ));
        }

        let mut parsed = parse_response(&text)?;
        parsed.request_metadata = req_meta;
        Ok(parsed)
    }

    /// POSTs with `stream: true`, reads the SSE
    /// response body as a chunked byte stream, parses each `data: {...}`
    /// line via `parse_stream_chunk`, drives a `StreamAccumulator` that
    /// reassembles the equivalent `TurnResponse`, and fires `on_delta` once
    /// per `ContentBlockDelta` so the caller can surface tokens live.
    #[tracing::instrument(
        skip(self, req, on_delta),
        fields(
            model = ?req.model,
            msg_count = req.conversation.len(),
            max_tokens = req.max_tokens,
        )
    )]
    async fn send_turn_streaming(
        &self,
        req: TurnRequest,
        on_delta: DeltaSink,
    ) -> Result<TurnResponse> {
        use futures::StreamExt;

        let mut body = build_messages_payload(&req);
        apply_context_management(&mut body);
        if let Some(obj) = body.as_object_mut() {
            obj.insert("stream".into(), serde_json::Value::Bool(true));
        }
        // Temporary diagnostic — dump the request payload to
        // a debug path so the schema-too-complex failure can be reproduced
        // offline.
        //
        // The dumped payload contains the
        // full system prompt + tool definitions + any tool-result bodies
        // already in the conversation, and is reachable through any LLM
        // turn with the env var set. Gate on ECAA_DEBUG=1 so the dump is
        // off unless the operator explicitly opted in, and force mode
        // 0600 so other users on a shared host can't read the file.
        if let Ok(dump_path) = std::env::var("ECAA_DUMP_ANTHROPIC_PAYLOAD") {
            dump_anthropic_payload(&dump_path, &body);
        }

        let url = self
            .resilient
            .base_url()
            .join("v1/messages")
            .map(|u| u.to_string())
            .unwrap_or_else(|_| format!("{}v1/messages", self.resilient.base_url()));
        let mut post = self
            .resilient
            .inner()
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .header("accept", "text/event-stream");
        if context_editing_enabled() {
            post = post.header("anthropic-beta", CONTEXT_MANAGEMENT_BETA);
        }
        let resp = post
            .json(&body)
            .send()
            .await
            .map_err(|e| wrap_send_error(e, &url, true))?;

        let status = resp.status();
        let req_meta = capture_request_metadata(resp.headers());
        if !status.is_success() {
            let text = resp.text().await.context("reading streaming error body")?;
            if status.as_u16() == 429 {
                return Err(anyhow!("anthropic API rate-limited (HTTP 429): {}", text));
            }
            return Err(anyhow!(
                "anthropic API error (HTTP {}): {}",
                status.as_u16(),
                text
            ));
        }

        let mut stream = resp.bytes_stream();
        // R4-LLM-3: byte-level buffering. Anthropic streams arbitrary
        // UTF-8 (delta text fields, tool-use payloads, multilingual
        // intake responses). `String::from_utf8_lossy` on each TCP
        // chunk corrupts any multi-byte codepoint split across a
        // chunk boundary (e.g. a Chinese character whose 3-byte UTF-8
        // sequence lands with byte 0 in chunk N and bytes 1-2 in
        // chunk N+1). Accumulate raw bytes until we see the
        // `\n\n` event separator, then decode the complete event
        // slice so codepoint boundaries are always preserved.
        let mut buffer: Vec<u8> = Vec::new();
        let mut accumulator = StreamAccumulator::default();

        while let Some(chunk) = stream.next().await {
            let bytes = chunk.context("reading SSE chunk")?;
            buffer.extend_from_slice(bytes.as_ref());
            // hard cap on the unparsed buffer. Normal Anthropic
            // responses top out around 500 kB; anything approaching
            // SSE_BUFFER_MAX_BYTES is pathological (runaway
            // tool-call generation, malformed stream) and would
            // otherwise OOM. Fail loud instead.
            if buffer.len() > SSE_BUFFER_MAX_BYTES {
                return Err(anyhow!(
                    "anthropic SSE buffer exceeded {} bytes without a \
                     complete event; aborting to avoid OOM",
                    SSE_BUFFER_MAX_BYTES
                ));
            }
            // Anthropic emits one event per blank-line-separated block.
            // Find each `\n\n` boundary by scanning bytes; the
            // decode happens AFTER the slice is known to end on an
            // event boundary, which is guaranteed to be a codepoint
            // boundary (LF is a single ASCII byte).
            let mut scan_from: usize = 0;
            while let Some(end_rel) = buffer[scan_from..].windows(2).position(|w| w == b"\n\n") {
                let end = scan_from + end_rel;
                // Decode just the event slice — guaranteed to be a
                // complete UTF-8 sequence because the `\n\n`
                // boundary is on an ASCII byte that can't appear in
                // the middle of a multi-byte UTF-8 codepoint.
                let event_text = String::from_utf8_lossy(&buffer[..end]).into_owned();
                buffer.drain(..end + 2);
                scan_from = 0;
                for line in event_text.lines() {
                    let line = line.trim_start();
                    if let Some(json) = line.strip_prefix("data:") {
                        let json = json.trim_start();
                        if json == "[DONE]" || json.is_empty() {
                            continue;
                        }
                        let events = parse_stream_chunk(json)?;
                        for event in events {
                            accumulator.handle(event, on_delta.as_ref());
                        }
                    }
                }
            }
        }

        let mut parsed = accumulator.finalize()?;
        parsed.request_metadata = req_meta;
        Ok(parsed)
    }

    /// POST /v1/messages/count_tokens preflight. Calls
    /// the same Messages API host with the same payload shape as
    /// `send_turn`, but the count_tokens endpoint returns just the
    /// would-be input-token charge without invoking the model.
    ///
    /// The endpoint is free (no token charge); its own RPM limit is
    /// separate from the Messages limit so a budget-tight session
    /// can preflight without consuming its rate budget.
    async fn count_tokens(&self, request: &TurnRequest) -> Result<Option<u32>> {
        // Build the standard payload. count_tokens accepts the same
        // shape as messages create, minus stream/temperature/max_tokens.
        let mut body = build_messages_payload(request);
        if let Some(obj) = body.as_object_mut() {
            obj.remove("stream");
            obj.remove("temperature");
            obj.remove("max_tokens");
        }
        let url = self
            .resilient
            .base_url()
            .join("v1/messages/count_tokens")
            .map(|u| u.to_string())
            .unwrap_or_else(|_| format!("{}v1/messages/count_tokens", self.resilient.base_url()));
        let resp = self
            .resilient
            .inner()
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .with_context(|| format!("POST {}", url))?;
        let status = resp.status();
        let text = resp.text().await.context("reading count_tokens body")?;
        if !status.is_success() {
            return Err(anyhow!(
                "count_tokens API error (HTTP {}): {}",
                status.as_u16(),
                text
            ));
        }
        // Response shape: `{"input_tokens": <u32>}`.
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .with_context(|| format!("parsing count_tokens response: {}", text))?;
        let input = parsed
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .map(|n| n as u32);
        Ok(input)
    }
}

/// Read the observability headers Anthropic returns on every Messages
/// API response. All fields optional — older API versions, the mock
/// backend, and connection-error responses leave them unset. Plan S2.8.
fn capture_request_metadata(headers: &reqwest::header::HeaderMap) -> RequestMetadata {
    fn header_str<'a>(headers: &'a reqwest::header::HeaderMap, key: &str) -> Option<&'a str> {
        headers.get(key).and_then(|v| v.to_str().ok())
    }
    fn header_u32(headers: &reqwest::header::HeaderMap, key: &str) -> Option<u32> {
        header_str(headers, key).and_then(|v| v.parse::<u32>().ok())
    }
    RequestMetadata {
        request_id: header_str(headers, "anthropic-request-id").map(str::to_string),
        ratelimit_requests_remaining: header_u32(headers, "anthropic-ratelimit-requests-remaining"),
        ratelimit_input_tokens_remaining: header_u32(
            headers,
            "anthropic-ratelimit-input-tokens-remaining",
        ),
        ratelimit_output_tokens_remaining: header_u32(
            headers,
            "anthropic-ratelimit-output-tokens-remaining",
        ),
    }
}

// ── Request payload assembly ─────────────────────────────────────────────────

/// Maximum `cache_control` breakpoints Anthropic allows in a single
/// Messages API request. Exceeding this returns HTTP 400 at send time.
/// `count_cache_markers` walks the assembled payload and asserts the
/// invariant in debug builds; `build_messages_payload` is the only
/// producer of these markers, so any regression is caught at test time.
pub(super) const MAX_CACHE_BREAKPOINTS: usize = 4;

/// Build the Anthropic Messages `POST /v1/messages` payload from a
/// `TurnRequest`. Promoted from `pub(super)` to `pub` so the
/// cross-crate regression test
/// Helper for [`build_messages_payload`]. Walks every
/// message's `content` array; for any `{"type":"text","text":""}` block
/// (or whitespace-only text), substitutes a single ASCII space so
/// Anthropic accepts the request, and emits a tracing::warn with the
/// message index + role for forensic follow-up.
///
/// Rationale: Anthropic's Messages API rejects empty text content
/// blocks with HTTP 400 "messages: text content blocks must be
/// non-empty". Three blinded-corpus scenarios hit this on iteration 1
/// of a fresh session; the root cause is still under investigation
/// (suspect: stale assistant turn with `content=""` from a prior
/// streaming response that stopped on tool_use only). A blanket
/// failure here costs the whole user turn; a placeholder character
/// keeps the conversation forward-moving while surfacing the bug.
fn sanitize_empty_text_blocks(messages: &mut [serde_json::Value]) {
    for (idx, msg) in messages.iter_mut().enumerate() {
        let role = msg
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("unknown")
            .to_string();
        let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        for (block_idx, block) in content.iter_mut().enumerate() {
            let Some(obj) = block.as_object_mut() else {
                continue;
            };
            let is_text_block = obj.get("type").and_then(|t| t.as_str()) == Some("text");
            if !is_text_block {
                continue;
            }
            let text_is_empty = obj
                .get("text")
                .and_then(|t| t.as_str())
                .map(|s| s.trim().is_empty())
                .unwrap_or(true);
            if text_is_empty {
                tracing::warn!(
                    message_index = idx,
                    block_index = block_idx,
                    role = %role,
                    "sanitizing empty text content block before Anthropic POST \
                     — RCA-2026-05-20"
                );
                obj.insert("text".into(), serde_json::json!(" "));
            }
        }
    }
}

/// `tests/force_classify_on_first_intake.rs` can assert payload
/// shape without instantiating an `AnthropicClient`.
pub fn build_messages_payload(req: &TurnRequest) -> serde_json::Value {
    use serde_json::json;

    let mut system_blocks: Vec<serde_json::Value> = Vec::new();
    for block in &req.system_prompt {
        let mut obj = serde_json::Map::new();
        obj.insert("type".into(), json!("text"));
        obj.insert("text".into(), json!(block.text));
        if block.cache {
            obj.insert("cache_control".into(), json!({"type": "ephemeral"}));
        }
        system_blocks.push(serde_json::Value::Object(obj));
    }

    let mut messages: Vec<serde_json::Value> = req
        .conversation
        .iter()
        .filter(|t| t.role != crate::session::TurnRole::System)
        .map(|t| {
            let role = match t.role {
                crate::session::TurnRole::User => "user",
                crate::session::TurnRole::Assistant => "assistant",
                crate::session::TurnRole::System => "user",
            };
            json!({
                "role": role,
                "content": [{"type": "text", "text": t.content}],
            })
        })
        .collect();

    // Marker-budget accounting: Anthropic caps cache_control breakpoints
    // per request at `MAX_CACHE_BREAKPOINTS` (4). We must pick which
    // optional markers to emit when the fixed markers already consume
    // the budget.
    //
    // Fixed markers (when conditions met):
    // - one per cached system-prompt block (role, class, taxonomy…)
    // - the last tool schema (§3.9, when tools non-empty)
    // - the tool_exchange tail (§3.14, when exchange non-empty)
    //
    // Discretionary marker:
    // - the conversation-tail marker (§3.8)
    //
    // The conversation-tail marker was the first §3.8 optimization, but
    // it's the cheapest to skip: cross-turn it never hits anyway (the
    // uncached `format_session_state` block between system and conversation
    // invalidates the prefix every turn), and within-turn its only job is
    // to anchor the iter-1→iter-2 cache chain. When tool_exchange exists
    // (iter 2+), the tool_exchange tail marker serves the same role for
    // iter 2→iter N. So suppressing convtail when the budget is tight
    // costs at most an iter-2 anchor re-write, which Anthropic's 5-min
    // TTL will absorb within the same turn anyway.
    let cached_system_count = req.system_prompt.iter().filter(|b| b.cache).count();
    let tools_marker = if req.tool_schemas.is_empty() { 0 } else { 1 };
    let toolex_tail_marker = if req.tool_exchange.is_empty() { 0 } else { 1 };
    let fixed_markers = cached_system_count + tools_marker + toolex_tail_marker;
    let convtail_fits = fixed_markers < MAX_CACHE_BREAKPOINTS;

    // §3.8 — attach a cache_control marker to the last content block of
    // the last historical message so Anthropic caches the whole
    // conversation prefix along with the system prompt. On subsequent
    // calls within the 5-min TTL window, everything before this marker
    // bills at cache-read rate (0.1× input). Suppressed when the fixed
    // marker count already fills the budget (see comment above).
    if convtail_fits {
        if let Some(last_msg) = messages.last_mut() {
            if let Some(content) = last_msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                if let Some(last_block) = content.last_mut().and_then(|b| b.as_object_mut()) {
                    last_block.insert("cache_control".into(), json!({"type": "ephemeral"}));
                }
            }
        }
    }

    // Append tool_use / tool_result messages from the current tool loop.
    // These carry the proper Anthropic Messages API content blocks so the
    // model sees its own tool calls and their results.
    let tool_exchange_start = messages.len();
    messages.extend(req.tool_exchange.iter().cloned());

    // §3.14 — attach a cache_control marker to the last content block of
    // the final tool_exchange entry. Within one user turn, tool_exchange
    // grows monotonically (each iteration appends one assistant + one
    // user message), so the previous iteration's prefix is a proper
    // prefix of the current one. Anthropic cache-reads the earlier
    // iterations and cache-writes only the newest tool_result delta,
    // which flips the 16-iteration worst case from O(n²) full-rate
    // replay to O(n) writes + O(n) cached reads.
    if messages.len() > tool_exchange_start {
        if let Some(last_msg) = messages.last_mut() {
            if let Some(content) = last_msg.get_mut("content").and_then(|c| c.as_array_mut()) {
                if let Some(last_block) = content.last_mut().and_then(|b| b.as_object_mut()) {
                    last_block.insert("cache_control".into(), json!({"type": "ephemeral"}));
                }
            }
        }
    }

    // §3.9 — clone tool schemas and attach a cache_control marker to the
    // last entry so Anthropic caches the full tool vocabulary as part of
    // the prefix. On subsequent calls within the 5-min TTL window the
    // ~5–6 KB of tool JSON bills at cache-read rate instead of full
    // input. Uses one of the 4 available breakpoints.
    //
    // S2.15 — set `strict: true` on every tool definition. Per
    // Anthropic's 2026 Messages API guidance, strict mode enforces
    // schema compliance on tool_use inputs so a missing/malformed
    // parameter returns a parse error rather than a silently truncated
    // call we'd then try to dispatch. Cuts a class of "model invented
    // a field that isn't in the schema" failures the closed 16-tool
    // vocabulary already implicitly relies on.
    let mut tools: Vec<serde_json::Value> = req
        .tool_schemas
        .iter()
        .cloned()
        .map(|mut t| {
            if let Some(obj) = t.as_object_mut() {
                obj.insert("strict".into(), json!(true));
            }
            t
        })
        .collect();
    if let Some(last_tool) = tools.last_mut().and_then(|v| v.as_object_mut()) {
        last_tool.insert("cache_control".into(), json!({"type": "ephemeral"}));
    }

    // Anthropic rejects any text content block whose `text` field is empty
    // ("messages: text content blocks must be non-empty", HTTP 400). Empty
    // blocks can slip in from a stale assistant turn that stopped on
    // tool_use only, a tool_exchange entry with whitespace-only text, or a
    // race with concurrent writers replacing content. Swap any empty-text
    // block for a placeholder space and log the offending message index so
    // the origin is observable post-hoc rather than crashing the whole
    // turn with a 500.
    sanitize_empty_text_blocks(&mut messages);

    let mut payload = json!({
        "model": req.model.api_id(),
        "max_tokens": req.max_tokens,
        "system": system_blocks,
        "messages": messages,
        "tools": tools,
    });
    if !matches!(req.model, crate::model_policy::ModelId::Opus47) {
        payload["temperature"] = json!(req.temperature);
    }
    if let Some(choice) = &req.tool_choice {
        payload["tool_choice"] = choice.to_anthropic_payload();
    }

    // Defensive cache-invariant guard. A bug that silently generates >4
    // markers would be rejected by the Anthropic API at send time with
    // HTTP 400; catching it locally surfaces the offending call-site
    // instead of a runtime failure. Also asserts TTL is the 5-minute
    // default unless explicitly allowed via ECAA_ALLOW_1H_CACHE=1.
    debug_assert!(
        count_cache_markers(&payload) <= MAX_CACHE_BREAKPOINTS,
        "cache_control marker count exceeds {} — Anthropic rejects these",
        MAX_CACHE_BREAKPOINTS
    );
    if std::env::var("ECAA_ALLOW_1H_CACHE").ok().as_deref() != Some("1") {
        debug_assert!(
            !has_non_default_ttl(&payload),
            "cache_control ttl must be the default (5m ephemeral); set ECAA_ALLOW_1H_CACHE=1 to override"
        );
    }

    payload
}

/// Walk a built Messages API payload and return the total number of
/// `cache_control` markers across `system`, `messages`, `tools`, and
/// any nested `tool_result` blocks. Used by the debug-assert guard in
/// `build_messages_payload` and by unit tests that lock the invariant.
pub(super) fn count_cache_markers(payload: &serde_json::Value) -> usize {
    fn walk(v: &serde_json::Value, count: &mut usize) {
        match v {
            serde_json::Value::Object(map) => {
                if map.contains_key("cache_control") {
                    *count += 1;
                }
                for (_k, child) in map {
                    walk(child, count);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    walk(item, count);
                }
            }
            _ => {}
        }
    }
    let mut count = 0;
    walk(payload, &mut count);
    count
}

/// True if any `cache_control` object declares a non-default `ttl`
/// (e.g. `"ttl": "1h"`). The 5-minute ephemeral TTL is the default and
/// cheaper for interactive chat — 1-hour writes cost 2× base input vs
/// 1.25× for 5-minute. Guarded at payload-build time (see §4.6).
fn has_non_default_ttl(payload: &serde_json::Value) -> bool {
    fn walk(v: &serde_json::Value) -> bool {
        match v {
            serde_json::Value::Object(map) => {
                if let Some(cc) = map.get("cache_control") {
                    if let Some(cc_obj) = cc.as_object() {
                        if cc_obj.contains_key("ttl") {
                            return true;
                        }
                    }
                }
                map.values().any(walk)
            }
            serde_json::Value::Array(items) => items.iter().any(walk),
            _ => false,
        }
    }
    walk(payload)
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ApiResponse {
    #[allow(dead_code)] // reserved-for-serde: deserialised but unused; preserves on-wire shape
    id: Option<String>,
    #[allow(dead_code)] // reserved-for-serde: deserialised but unused; preserves on-wire shape
    role: Option<String>,
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    #[serde(default)]
    usage: ApiUsage,
}

#[derive(Debug, Deserialize, Default, schemars::JsonSchema)]
struct ApiUsage {
    #[serde(default)]
    input_tokens: u32,
    #[serde(default)]
    output_tokens: u32,
    /// Legacy flat field — Anthropic still emits the sum across all
    /// TTL tiers here. Required for `MockLlmBackend`-style fixtures
    /// that pre-date the per-TTL split. New code should consult
    /// `cache_creation` for the tier-level breakdown.
    #[serde(default)]
    cache_creation_input_tokens: u32,
    #[serde(default)]
    cache_read_input_tokens: u32,
    /// R-38: per-TTL breakdown of cache writes. Present on responses
    /// after Anthropic introduced the `ephemeral_5m_input_tokens` /
    /// `ephemeral_1h_input_tokens` split. The 1h tier costs 2× base
    /// input vs 1.25× for the 5-minute tier (the `has_non_default_ttl`
    /// guard at this file's L687 already exists to flip
    /// `ECAA_ALLOW_1H_CACHE=1`), so per-tier accounting is necessary
    /// for accurate cost attribution. Defaults to `CacheCreation::default()`
    /// when absent — older fixtures + the mock backend leave it unset.
    #[serde(default)]
    cache_creation: CacheCreation,
}

/// R-38: per-TTL breakdown of Anthropic cache-creation token writes.
/// Each field is the count of cache-creation input tokens written into
/// the matching TTL tier. Sums across tiers equal the legacy
/// `cache_creation_input_tokens` flat field (when both are present).
#[derive(Debug, Deserialize, Default, Clone, Copy, serde::Serialize, schemars::JsonSchema)]
pub struct CacheCreation {
    /// Cache-creation input tokens written into the 5-minute TTL tier.
    #[serde(default)]
    pub ephemeral_5m_input_tokens: u64,
    /// Cache-creation input tokens written into the 1-hour TTL tier.
    #[serde(default)]
    pub ephemeral_1h_input_tokens: u64,
}

impl CacheCreation {
    /// Sum of all per-TTL tiers. Used to derive the flat
    /// `cache_creation_input_tokens` total when the new nested shape
    /// is present but the legacy flat field is zero.
    pub fn total(&self) -> u64 {
        self.ephemeral_5m_input_tokens
            .saturating_add(self.ephemeral_1h_input_tokens)
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

fn parse_response(body: &str) -> Result<TurnResponse> {
    let parsed: ApiResponse = serde_json::from_str(body)
        .with_context(|| format!("parsing anthropic response: {}", body))?;

    let mut text_chunks = String::new();
    let mut tool_uses: Vec<(Uuid, Tool)> = Vec::new();

    for block in parsed.content {
        match block {
            ContentBlock::Text { text } => {
                if !text_chunks.is_empty() {
                    text_chunks.push('\n');
                }
                text_chunks.push_str(&text);
            }
            ContentBlock::ToolUse { id, name, input } => {
                let tool = tool_from_api(&name, input)?;
                // R-34: Anthropic emits `toolu_*` strings that aren't
                // UUIDs (`Uuid::parse_str` fails) — previously we minted
                // a fresh `Uuid::new_v4()`, so two parses of the same
                // response body produced DIFFERENT UUIDs and the
                // `tool_result.tool_use_id` we sent back bore no
                // relation to Anthropic-side state.
                //
                // Derive the UUID deterministically from the original
                // `toolu_*` string (SHA-256 truncated to 16 bytes,
                // tagged version=4 / variant=RFC4122 per RFC 9562 §5.4).
                // Re-parsing the same body now yields the same UUID, so
                // the audit log + Anthropic-side tool_use_id remain in a
                // stable functional relationship. Also tracing-logs the
                // original `toolu_*` so downstream audit tooling can
                // recover the literal Anthropic id even though it
                // doesn't ride in the `(Uuid, Tool)` tuple. The
                // tuple-shape preservation keeps this an additive fix
                // — no churn at the ~10 downstream call sites that
                // pattern-match `(uid, tool)`.
                let uid = Uuid::parse_str(&id).unwrap_or_else(|_| {
                    let original = id.clone();
                    tracing::debug!(
                        anthropic_tool_use_id = %original,
                        "deriving deterministic UUID from non-UUID tool_use id"
                    );
                    derive_uuid_from_opaque_id(&original)
                });
                tool_uses.push((uid, tool));
            }
        }
    }

    let stop_reason = match parsed.stop_reason.as_deref() {
        Some("end_turn") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        _ => StopReason::Other,
    };

    // R-38: derive the flat `cache_creation_input_tokens` total from
    // the per-TTL breakdown when the response carries it. Anthropic
    // currently emits BOTH the legacy flat field AND the nested
    // shape, so we prefer the explicit flat value when present and
    // fall back to the per-tier sum (which equals the flat value
    // anyway when both arrive — but a future cleanup that drops the
    // legacy field on the API side won't silently zero the metric).
    let nested_total = parsed.usage.cache_creation.total();
    let cache_creation_input_tokens = if parsed.usage.cache_creation_input_tokens > 0 {
        parsed.usage.cache_creation_input_tokens
    } else {
        // Truncate to u32 to match the existing Usage shape; the
        // nested struct uses u64 because Anthropic's docs leave the
        // upper bound implementation-defined. A 4B-token cache write
        // is unrealistic but a saturate-on-overflow keeps the metric
        // honest if it ever happens.
        nested_total.min(u32::MAX as u64) as u32
    };

    Ok(TurnResponse {
        assistant_content: text_chunks,
        tool_uses,
        stop_reason,
        usage: Usage {
            input_tokens: parsed.usage.input_tokens,
            output_tokens: parsed.usage.output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens: parsed.usage.cache_read_input_tokens,
        },
        request_metadata: RequestMetadata::default(),
    })
}

/// R-34: derive a deterministic UUID from an opaque tool-use identifier
/// (typically Anthropic's `toolu_*` string). Two calls with the same
/// input always produce the same UUID, so audit logs that key by the
/// derived UUID stay stable across re-deserialization of the same
/// response body. Uses SHA-256 truncated to 16 bytes, tagged
/// version=4 / variant=RFC4122 per RFC 9562 §5.4 layout.
///
/// Lives next to `parse_response` so the call site comment + helper
/// stay co-located. Not exposed publicly — internal correctness
/// invariant, no caller benefits from reaching past `TurnResponse`.
fn derive_uuid_from_opaque_id(original: &str) -> Uuid {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(original.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // Set version (high nibble of byte 6) to 4 and variant (top two
    // bits of byte 8) to 10xx — matches `Uuid::new_v4`'s layout so
    // downstream code that inspects the version field doesn't choke.
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;
    Uuid::from_bytes(bytes)
}

/// Reuse the same enum the dispatcher uses by re-serializing under the
/// tag the enum expects. Shared with the `StreamAccumulator` so streaming
/// and non-streaming paths converge on the same Tool construction.
pub(super) fn tool_from_api(name: &str, input: serde_json::Value) -> Result<Tool> {
    let mut obj = match input {
        serde_json::Value::Object(m) => m,
        other => return Err(anyhow!("tool_use input was not an object: {:?}", other)),
    };
    obj.insert("tool_name".into(), serde_json::Value::String(name.into()));
    let v = serde_json::Value::Object(obj);
    serde_json::from_value(v)
        .with_context(|| format!("deserializing tool_use payload for '{}'", name))
}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup/teardown; the bounded waiver is
    // scoped to this `mod tests` block.
    #![allow(unsafe_code)]
    use super::*;
    use crate::tools::BatchableTool;

    /// Regression guard: sanitize_empty_text_blocks must
    /// replace any empty / whitespace-only text content block with a
    /// single space, preserving every other field on the block (type,
    /// cache_control, etc.) and every other message untouched. Without
    /// this guard the Anthropic Messages API rejects the request with
    /// HTTP 400 "messages: text content blocks must be non-empty".
    #[test]
    fn sanitize_empty_text_blocks_replaces_empty_text_with_space() {
        let mut messages = vec![
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": ""}
                ],
            }),
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "We have weekly counts ..."}
                ],
            }),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "   "},
                    {"type": "tool_use", "id": "tu_1", "name": "x", "input": {}}
                ],
            }),
        ];
        sanitize_empty_text_blocks(&mut messages);
        assert_eq!(
            messages[0]["content"][0]["text"], " ",
            "empty assistant text block must be replaced with a space"
        );
        assert_eq!(
            messages[1]["content"][0]["text"], "We have weekly counts ...",
            "non-empty user message must be untouched"
        );
        assert_eq!(
            messages[2]["content"][0]["text"], " ",
            "whitespace-only assistant text block must be replaced with a space"
        );
        assert_eq!(
            messages[2]["content"][1]["type"], "tool_use",
            "non-text blocks must be untouched (tool_use survives unchanged)"
        );
    }

    #[test]
    fn parse_text_only_response() {
        let body = serde_json::json!({
            "content": [
                {"type": "text", "text": "Hello, how can I help?"}
            ],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 8}
        })
        .to_string();
        let resp = parse_response(&body).unwrap();
        assert_eq!(resp.assistant_content, "Hello, how can I help?");
        assert_eq!(resp.stop_reason, StopReason::EndTurn);
        assert!(resp.tool_uses.is_empty());
        assert_eq!(resp.usage.input_tokens, 10);
    }

    #[test]
    fn parse_tool_use_response() {
        let body = serde_json::json!({
            "content": [
                {"type": "text", "text": "Looking that up..."},
                {"type": "tool_use", "id": "tu_1", "name": "classify_intake",
                 "input": {"prose": "scRNA-seq from human IVD"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 12, "output_tokens": 4}
        })
        .to_string();
        let resp = parse_response(&body).unwrap();
        assert_eq!(resp.stop_reason, StopReason::ToolUse);
        assert_eq!(resp.tool_uses.len(), 1);
        match &resp.tool_uses[0].1 {
            Tool::Batchable(BatchableTool::ClassifyIntake { prose }) => {
                assert!(prose.contains("scRNA-seq"))
            }
            _ => panic!("expected ClassifyIntake variant"),
        }
    }

    #[test]
    fn parse_malformed_json_errors() {
        let r = parse_response("{not json");
        assert!(r.is_err());
    }

    // Derived UUIDs for non-UUID tool_use ids must be stable across
    // re-parses of the same response body, so the audit log preserves
    // the relationship to the Anthropic-side `toolu_*` id.
    #[test]
    fn parse_toolu_id_yields_deterministic_uuid() {
        let body = serde_json::json!({
            "content": [
                {"type": "tool_use", "id": "toolu_01ABCDEFG", "name": "classify_intake",
                 "input": {"prose": "x"}}
            ],
            "stop_reason": "tool_use",
            "usage": {}
        })
        .to_string();
        let a = parse_response(&body).unwrap();
        let b = parse_response(&body).unwrap();
        assert_eq!(a.tool_uses[0].0, b.tool_uses[0].0);
        // The derived UUID has version=4 / variant=RFC4122 per RFC
        // 9562 §5.4 — `Uuid::get_version_num` returns 4.
        assert_eq!(a.tool_uses[0].0.get_version_num(), 4);
    }

    #[test]
    fn parse_distinct_toolu_ids_yield_distinct_uuids() {
        let body = |id: &str| {
            serde_json::json!({
                "content": [
                    {"type": "tool_use", "id": id, "name": "classify_intake",
                     "input": {"prose": "x"}}
                ],
                "stop_reason": "tool_use",
                "usage": {}
            })
            .to_string()
        };
        let a = parse_response(&body("toolu_01A")).unwrap();
        let b = parse_response(&body("toolu_01B")).unwrap();
        assert_ne!(a.tool_uses[0].0, b.tool_uses[0].0);
    }

    #[test]
    fn derive_uuid_from_opaque_id_is_pure() {
        let a = derive_uuid_from_opaque_id("toolu_01XYZ");
        let b = derive_uuid_from_opaque_id("toolu_01XYZ");
        assert_eq!(a, b);
        assert_eq!(a.get_version_num(), 4);
        let c = derive_uuid_from_opaque_id("");
        assert_eq!(c.get_version_num(), 4);
    }

    // R-38: legacy flat `cache_creation_input_tokens` continues to
    // round-trip when present (matches the historical fixture shape +
    // every existing test that asserts on this field).
    #[test]
    fn parse_response_preserves_legacy_cache_creation_flat() {
        let body = serde_json::json!({
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1,
                "cache_creation_input_tokens": 1500
            }
        })
        .to_string();
        let r = parse_response(&body).unwrap();
        assert_eq!(r.usage.cache_creation_input_tokens, 1500);
    }

    // R-38: per-TTL nested shape sums into the flat field when the
    // legacy flat field is absent / zero. Validates the
    // `ECAA_ALLOW_1H_CACHE=1` future-flip scenario flagged in the
    // remediation plan.
    #[test]
    fn parse_response_derives_flat_from_nested_per_ttl() {
        let body = serde_json::json!({
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 800,
                    "ephemeral_1h_input_tokens": 200
                }
            }
        })
        .to_string();
        let r = parse_response(&body).unwrap();
        assert_eq!(r.usage.cache_creation_input_tokens, 1000);
    }

    // R-38: when BOTH shapes arrive, the explicit flat field wins
    // (matches Anthropic's current transition behaviour where the
    // legacy total ships alongside the new per-TTL breakdown).
    #[test]
    fn parse_response_prefers_flat_when_both_present() {
        let body = serde_json::json!({
            "content": [{"type": "text", "text": "ok"}],
            "stop_reason": "end_turn",
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1,
                "cache_creation_input_tokens": 1234,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 9999,
                    "ephemeral_1h_input_tokens": 9999
                }
            }
        })
        .to_string();
        let r = parse_response(&body).unwrap();
        assert_eq!(r.usage.cache_creation_input_tokens, 1234);
    }

    #[test]
    fn cache_creation_total_sums_tiers() {
        let cc = CacheCreation {
            ephemeral_5m_input_tokens: 7,
            ephemeral_1h_input_tokens: 11,
        };
        assert_eq!(cc.total(), 18);
    }

    #[test]
    fn missing_api_key_errors_at_new() {
        let prior_swfc = std::env::var("ECAA_ANTHROPIC_API_KEY").ok();
        let prior_legacy = std::env::var("ANTHROPIC_API_KEY").ok();
        // SAFETY: tests in this module run single-threaded by virtue of
        // accessing the same env var; we restore on drop below.
        unsafe {
            std::env::remove_var("ECAA_ANTHROPIC_API_KEY");
            std::env::remove_var("ANTHROPIC_API_KEY");
        };
        let r = AnthropicClient::new();
        if let Some(k) = prior_swfc {
            unsafe { std::env::set_var("ECAA_ANTHROPIC_API_KEY", k) };
        }
        if let Some(k) = prior_legacy {
            unsafe { std::env::set_var("ANTHROPIC_API_KEY", k) };
        }
        assert!(r.is_err());
    }

    #[test]
    fn build_payload_serializes_cached_blocks() {
        let req = TurnRequest {
            system_prompt: vec![crate::prompt::SystemPromptBlock {
                text: "system rules".into(),
                cache: true,
            }],
            conversation: std::sync::Arc::new(vec![crate::session::Turn::user("hello")]),
            tool_schemas: vec![serde_json::json!({"name": "x"})],
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_exchange: vec![],
            tool_choice: None,
        };
        let body = build_messages_payload(&req);
        let system = body["system"].as_array().unwrap();
        assert_eq!(system[0]["text"], "system rules");
        assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["model"], "claude-sonnet-4-6");
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn cache_markers_stay_within_anthropic_limit() {
        // Anthropic rejects requests with >4 cache_control markers. This
        // test locks the invariant against future regressions (e.g. a
        // new cacheable block added to build_system_prompt without
        // adjusting other breakpoints). System-prompt blocks match the
        // current real shape from build_system_prompt (role+tools cached,
        // taxonomy+state not), plus the §3.8 conversation-tail marker
        // and §3.9 last-tool marker added to every payload — total must
        // be ≤ 4.
        let req = TurnRequest {
            system_prompt: vec![
                crate::prompt::SystemPromptBlock {
                    text: "role".into(),
                    cache: true,
                },
                crate::prompt::SystemPromptBlock {
                    text: "tools".into(),
                    cache: true,
                },
                crate::prompt::SystemPromptBlock {
                    text: "taxonomy".into(),
                    cache: false,
                },
                crate::prompt::SystemPromptBlock {
                    text: "state".into(),
                    cache: false,
                },
            ],
            conversation: std::sync::Arc::new(vec![
                crate::session::Turn::user("hello"),
                crate::session::Turn::assistant("hi"),
                crate::session::Turn::user("tell me more"),
            ]),
            tool_schemas: vec![
                serde_json::json!({"name": "a"}),
                serde_json::json!({"name": "b"}),
                serde_json::json!({"name": "c"}),
            ],
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_exchange: vec![],
            tool_choice: None,
        };
        let body = build_messages_payload(&req);
        let markers = count_cache_markers(&body);
        assert!(
            markers <= MAX_CACHE_BREAKPOINTS,
            "marker count {} exceeds Anthropic limit {}",
            markers,
            MAX_CACHE_BREAKPOINTS
        );
        assert_eq!(
            markers, 4,
            "expected exactly 4 markers today (role + tools-system + conversation-tail + last-tool), got {}",
            markers
        );
    }

    #[test]
    fn bio_taxonomy_session_payload_respects_marker_budget() {
        // Bio sessions with a loaded taxonomy cache the taxonomy
        // block. This test exercises the worst-case real-world shape
        // — bio session, non-empty conversation, non-empty
        // tool_exchange — and asserts the resulting payload still fits
        // in Anthropic's 4-breakpoint cap.
        //
        // Marker census for this shape:
        // - role (cached) = 1
        // - taxonomy (cached, new) = 2
        // - last tool schema (§3.9) = 3
        // - conversation tail (§3.8) = 4 ← at cap
        // - tool_exchange tail (§3.14) — WOULD OVERFLOW
        //
        // See `bio_taxonomy_session_drops_convtail_marker` below for
        // the mitigation: when system has multiple cached blocks, the
        // conversation-tail marker is suppressed so the tool_exchange
        // tail marker stays available within the cap.
        // Phase B4 — synthesize a minimal StageTaxonomy metadata holder
        // for the cache-marker budget regression. Pre-B4 this loaded
        // `config/stage-taxonomies/single-cell.yaml`; the YAMLs are gone.
        let tax = ecaa_workflow_core::taxonomy::StageTaxonomy {
            id: "single_cell".into(),
            domain: "computational biology".into(),
            description: "single-cell RNA-seq composition (synthesized for client tests)".into(),
            ..Default::default()
        };
        let mut session = crate::session::Session::new(false);
        session.taxonomy = Some(tax);

        let system_prompt = crate::prompt::build_system_prompt(&session);
        let tool_schemas = crate::tool_schemas::tool_schemas_for_state(&session.state);

        let req = TurnRequest {
            system_prompt,
            conversation: std::sync::Arc::new(vec![
                crate::session::Turn::user("please help me analyse this"),
                crate::session::Turn::assistant("happy to help"),
                crate::session::Turn::user("bulk rnaseq, de genes"),
            ]),
            tool_schemas,
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_choice: None,
            tool_exchange: vec![
                serde_json::json!({
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "tu_1", "name": "x", "input": {}}],
                }),
                serde_json::json!({
                    "role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "ok"}],
                }),
            ],
        };
        let body = build_messages_payload(&req);
        let markers = count_cache_markers(&body);
        assert!(
            markers <= MAX_CACHE_BREAKPOINTS,
            "bio+tax+conversation+tool_exchange payload produced {} markers; \
             must stay within {} or Anthropic returns HTTP 400",
            markers,
            MAX_CACHE_BREAKPOINTS
        );
    }

    #[test]
    fn non_bio_taxonomy_session_payload_respects_marker_budget() {
        // Non-bio sessions spend a marker on the class block, leaving
        // no room for a taxonomy marker. prompt.rs enforces that
        // non-bio taxonomy stays cache:false; the end-to-end payload
        // must stay ≤ 4 markers in the mid-tool-loop shape.
        // Phase B4 — synthesize a clinical-trial StageTaxonomy metadata
        // holder for the cache-marker budget regression.
        let tax = ecaa_workflow_core::taxonomy::StageTaxonomy {
            id: "clinical_trial".into(),
            domain: "clinical research".into(),
            description: "clinical trial analysis (synthesized for client tests)".into(),
            project_class: Some(ecaa_workflow_core::project_class::ProjectClass::ClinicalTrial),
            ..Default::default()
        };
        let mut session = crate::session::Session::new(false);
        session.project_class = ecaa_workflow_core::project_class::ProjectClass::ClinicalTrial;
        session.taxonomy = Some(tax);

        let system_prompt = crate::prompt::build_system_prompt(&session);
        let tool_schemas = crate::tool_schemas::tool_schemas_for_state(&session.state);

        let req = TurnRequest {
            system_prompt,
            conversation: std::sync::Arc::new(vec![
                crate::session::Turn::user("clinical trial review"),
                crate::session::Turn::assistant("sure"),
                crate::session::Turn::user("randomized, 200 patients"),
            ]),
            tool_schemas,
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_choice: None,
            tool_exchange: vec![
                serde_json::json!({
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "tu_1", "name": "x", "input": {}}],
                }),
                serde_json::json!({
                    "role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "ok"}],
                }),
            ],
        };
        let body = build_messages_payload(&req);
        let markers = count_cache_markers(&body);
        assert!(
            markers <= MAX_CACHE_BREAKPOINTS,
            "non-bio+tax+conversation+tool_exchange payload produced {} markers; \
             the taxonomy block must stay uncached for non-bio to fit the budget",
            markers
        );
    }

    #[test]
    fn conversation_tail_carries_cache_marker() {
        // §3.8 regression guard: the last message of the historical
        // conversation must carry a cache_control marker so Anthropic
        // caches the whole conversation prefix. If a refactor drops this
        // wire-up, long-session replay silently bills at full input rate.
        let req = TurnRequest {
            system_prompt: vec![],
            conversation: std::sync::Arc::new(vec![
                crate::session::Turn::user("one"),
                crate::session::Turn::assistant("two"),
                crate::session::Turn::user("three"),
            ]),
            tool_schemas: vec![],
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_choice: None,
            tool_exchange: vec![],
        };
        let body = build_messages_payload(&req);
        let messages = body["messages"].as_array().unwrap();
        let last = messages.last().unwrap();
        let last_block = last["content"].as_array().unwrap().last().unwrap();
        assert_eq!(last_block["cache_control"]["type"], "ephemeral");
        // Non-tail messages must NOT carry markers (otherwise we'd
        // fragment the cache).
        for msg in &messages[..messages.len() - 1] {
            for block in msg["content"].as_array().unwrap() {
                assert!(
                    block.get("cache_control").is_none(),
                    "non-tail message carries a cache_control marker"
                );
            }
        }
    }

    #[test]
    fn system_and_tools_are_byte_identical_across_turns() {
        // The cache hit ratio surfaced in the UI Performance tab only
        // matters if the cacheable prefix stays
        // byte-for-byte identical across turns. A regression that
        // introduces a timestamp, a randomized ID, or a non-
        // deterministic HashMap serialization into the prompt/tool
        // blocks would silently drop cache hits on every turn without
        // failing any existing test.
        //
        // Anthropic's prefix-matching cache serves reads when the byte
        // sequence up to a cache_control marker matches a prior
        // request's — bytes must match or the cache key diverges.
        let base_system = vec![
            crate::prompt::SystemPromptBlock {
                text: "role: you are the SME assistant".into(),
                cache: true,
            },
            crate::prompt::SystemPromptBlock {
                text: "state: turn 1 context".into(),
                cache: false,
            },
        ];
        let tools = vec![
            serde_json::json!({"name": "a", "description": "first"}),
            serde_json::json!({"name": "b", "description": "second"}),
        ];
        let req1 = TurnRequest {
            system_prompt: base_system.clone(),
            conversation: std::sync::Arc::new(vec![crate::session::Turn::user("turn 1")]),
            tool_schemas: tools.clone(),
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_exchange: vec![],
            tool_choice: None,
        };
        let req2 = TurnRequest {
            system_prompt: base_system.clone(),
            conversation: std::sync::Arc::new(vec![
                crate::session::Turn::user("turn 1"),
                crate::session::Turn::assistant("response 1"),
                crate::session::Turn::user("turn 2"),
            ]),
            tool_schemas: tools.clone(),
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_exchange: vec![],
            tool_choice: None,
        };
        let b1 = build_messages_payload(&req1);
        let b2 = build_messages_payload(&req2);
        assert_eq!(
            b1["system"][0].to_string(),
            b2["system"][0].to_string(),
            "role block bytes diverged between turns — cache would miss"
        );
        assert_eq!(
            b1["tools"].to_string(),
            b2["tools"].to_string(),
            "tools array bytes diverged between turns — cache would miss"
        );
    }

    #[test]
    fn tool_exchange_tail_carries_cache_marker() {
        // §3.14 regression guard: when tool_exchange is non-empty, the
        // last content block of the final tool_exchange message must
        // carry a cache_control marker so iteration-level replay bills
        // at cache-read rate instead of full input rate. Empty
        // tool_exchange must NOT add a marker.
        let empty_req = TurnRequest {
            system_prompt: vec![],
            conversation: std::sync::Arc::new(vec![crate::session::Turn::user("hi")]),
            tool_schemas: vec![],
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_exchange: vec![],
            tool_choice: None,
        };
        let body = build_messages_payload(&empty_req);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1, "no tool_exchange should mean 1 message");

        let with_exchange = TurnRequest {
            system_prompt: vec![],
            conversation: std::sync::Arc::new(vec![crate::session::Turn::user("hi")]),
            tool_schemas: vec![],
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_choice: None,
            tool_exchange: vec![
                serde_json::json!({
                    "role": "assistant",
                    "content": [{"type": "tool_use", "id": "tu_1", "name": "x", "input": {}}],
                }),
                serde_json::json!({
                    "role": "user",
                    "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "ok"}],
                }),
            ],
        };
        let body = build_messages_payload(&with_exchange);
        let messages = body["messages"].as_array().unwrap();
        let last_msg = messages.last().unwrap();
        let last_block = last_msg["content"].as_array().unwrap().last().unwrap();
        assert_eq!(
            last_block["cache_control"]["type"], "ephemeral",
            "tool_exchange tail must carry a cache_control marker"
        );
    }

    #[test]
    fn last_tool_schema_carries_cache_marker() {
        // §3.9 regression guard: the final tool in the `tools` array
        // must carry a cache_control marker so Anthropic caches the
        // tool vocabulary. Non-final tools must not, or we waste
        // breakpoints.
        let req = TurnRequest {
            system_prompt: vec![],
            conversation: std::sync::Arc::new(vec![]),
            tool_schemas: vec![
                serde_json::json!({"name": "a"}),
                serde_json::json!({"name": "b"}),
                serde_json::json!({"name": "c"}),
            ],
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_exchange: vec![],
            tool_choice: None,
        };
        let body = build_messages_payload(&req);
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 3);
        assert!(tools[0].get("cache_control").is_none());
        assert!(tools[1].get("cache_control").is_none());
        assert_eq!(tools[2]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn uncached_blocks_never_get_markers() {
        // Regression guard: if a future refactor of build_messages_payload
        // accidentally flips cache=false into a cache_control marker (e.g.
        // by iterating over the wrong flag), the number of markers on the
        // built payload would exceed the number of cache:true blocks.
        let req = TurnRequest {
            system_prompt: vec![
                crate::prompt::SystemPromptBlock {
                    text: "cached".into(),
                    cache: true,
                },
                crate::prompt::SystemPromptBlock {
                    text: "not cached".into(),
                    cache: false,
                },
            ],
            conversation: std::sync::Arc::new(vec![]),
            tool_schemas: vec![],
            model: crate::model_policy::ModelId::Sonnet46,
            temperature: 0.4,
            max_tokens: 1024,
            tool_exchange: vec![],
            tool_choice: None,
        };
        let body = build_messages_payload(&req);
        let system = body["system"].as_array().unwrap();
        assert!(system[0].get("cache_control").is_some());
        assert!(system[1].get("cache_control").is_none());
        assert_eq!(count_cache_markers(&body), 1);
    }

    #[test]
    fn context_management_applied_by_default_and_disabled_by_env() {
        // §3.13: the beta feature that auto-clears stale tool_result
        // blocks is on by default. `ECAA_DISABLE_CONTEXT_EDITING=1`
        // turns it off so an operator can A/B or roll back without a
        // redeploy.
        let mut body = serde_json::json!({"model": "claude-sonnet-4-6"});
        // Scope env mutation — tests in this module are serial because
        // they share the process-wide env.
        unsafe { std::env::remove_var("ECAA_DISABLE_CONTEXT_EDITING") };
        apply_context_management(&mut body);
        let edits = body["context_management"]["edits"].as_array().unwrap();
        assert_eq!(edits[0]["type"], "clear_tool_uses_20250919");
        assert_eq!(edits[0]["trigger"]["value"], 8);
        assert_eq!(edits[0]["keep"]["value"], 4);
        assert_eq!(edits[0]["clear_tool_inputs"], true);

        // Escape hatch: env flag disables the injection entirely.
        let mut body2 = serde_json::json!({"model": "claude-sonnet-4-6"});
        unsafe { std::env::set_var("ECAA_DISABLE_CONTEXT_EDITING", "1") };
        apply_context_management(&mut body2);
        assert!(body2.get("context_management").is_none());
        unsafe { std::env::remove_var("ECAA_DISABLE_CONTEXT_EDITING") };
    }

    #[test]
    fn ttl_guard_rejects_non_default_ttl() {
        // 5-minute ephemeral is the default and what interactive chat
        // wants (writes cost 1.25× base input; 1-hour writes cost 2×).
        // Any future change that sets `ttl: "1h"` on a cache_control
        // block must first flip ECAA_ALLOW_1H_CACHE=1. This test pins
        // the guard. See §4.6.
        let mut payload = serde_json::json!({
            "system": [
                {"type": "text", "text": "x", "cache_control": {"type": "ephemeral", "ttl": "1h"}}
            ]
        });
        assert!(has_non_default_ttl(&payload));
        payload = serde_json::json!({
            "system": [
                {"type": "text", "text": "x", "cache_control": {"type": "ephemeral"}}
            ]
        });
        assert!(!has_non_default_ttl(&payload));
    }

    // ── dump_anthropic_payload ───────

    /// Serialise the `dump_anthropic_payload` tests so concurrent
    /// `cargo test` threads don't observe each other's `ECAA_DEBUG`
    /// flipping. The env table is process-global; tests in this
    /// module already use `unsafe { set_var/remove_var }` under the
    /// crate-level `#![allow(unsafe_code)]` waiver.
    static DUMP_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn dump_anthropic_payload_skips_when_debug_unset() {
        let _g = DUMP_TEST_LOCK.lock().unwrap();
        unsafe { std::env::remove_var("ECAA_DEBUG") };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("dump.json");
        dump_anthropic_payload(
            path.to_str().unwrap(),
            &serde_json::json!({"test": "payload"}),
        );
        assert!(
            !path.exists(),
            "dump file MUST NOT be written when ECAA_DEBUG is unset"
        );
    }

    #[test]
    fn dump_anthropic_payload_writes_when_debug_set() {
        let _g = DUMP_TEST_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ECAA_DEBUG", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("dump.json");
        dump_anthropic_payload(
            path.to_str().unwrap(),
            &serde_json::json!({"test": "payload"}),
        );
        assert!(path.exists(), "dump file should exist when ECAA_DEBUG=1");
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"test\""));
        assert!(contents.contains("\"payload\""));
        unsafe { std::env::remove_var("ECAA_DEBUG") };
    }

    #[test]
    #[cfg(unix)]
    fn dump_anthropic_payload_forces_0600_mode_under_debug() {
        let _g = DUMP_TEST_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ECAA_DEBUG", "1") };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("dump.json");
        dump_anthropic_payload(path.to_str().unwrap(), &serde_json::json!({"k": "v"}));
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        // Compare the low 9 bits (the rwxrwxrwx subset of the mode).
        assert_eq!(
            mode & 0o777,
            0o600,
            "dump file mode must be 0600 (was {:o})",
            mode & 0o777
        );
        unsafe { std::env::remove_var("ECAA_DEBUG") };
    }

    #[test]
    fn dump_anthropic_payload_debug_true_string_also_enables() {
        // The guard accepts "1" or "true" as truthy.
        let _g = DUMP_TEST_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ECAA_DEBUG", "true") };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("dump.json");
        dump_anthropic_payload(path.to_str().unwrap(), &serde_json::json!({"k": "v"}));
        assert!(path.exists(), "ECAA_DEBUG=true must also enable the dump");
        unsafe { std::env::remove_var("ECAA_DEBUG") };
    }

    #[test]
    fn dump_anthropic_payload_skips_for_other_values() {
        let _g = DUMP_TEST_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ECAA_DEBUG", "0") };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("dump.json");
        dump_anthropic_payload(path.to_str().unwrap(), &serde_json::json!({"k": "v"}));
        assert!(!path.exists(), "ECAA_DEBUG=0 must NOT enable the dump");
        unsafe { std::env::remove_var("ECAA_DEBUG") };
    }

    // ── ResilientClient wiring (E28) ────────────────────────────────────────

    /// `AnthropicClient` internally wraps a `ResilientClient` whose scheme
    /// guard rejects non-https base URLs. This test confirms the guard is
    /// active: setting `ANTHROPIC_BASE_URL` to a non-loopback http:// URL
    /// must cause `new()` to fail rather than send credentials over plain
    /// HTTP.
    ///
    /// Because `ANTHROPIC_BASE_URL` is read inside `new()`, we temporarily
    /// set it in this test. The test is serialized by the shared env lock
    /// used by the dump tests above.
    #[test]
    fn resilient_client_rejects_non_loopback_http_base_url() {
        let _g = DUMP_TEST_LOCK.lock().unwrap();
        // Inject a non-https, non-loopback base URL.
        let prior_url = std::env::var("ANTHROPIC_BASE_URL").ok();
        let prior_swfc = std::env::var("ECAA_ANTHROPIC_API_KEY").ok();
        unsafe {
            std::env::set_var("ANTHROPIC_BASE_URL", "http://api.anthropic.com");
            // Provide a dummy key so the key-check doesn't fire first.
            std::env::set_var("ECAA_ANTHROPIC_API_KEY", "sk-ant-test-dummy");
        }
        let result = AnthropicClient::new();
        // Restore env.
        unsafe {
            match prior_url {
                Some(v) => std::env::set_var("ANTHROPIC_BASE_URL", v),
                None => std::env::remove_var("ANTHROPIC_BASE_URL"),
            }
            match prior_swfc {
                Some(v) => std::env::set_var("ECAA_ANTHROPIC_API_KEY", v),
                None => std::env::remove_var("ECAA_ANTHROPIC_API_KEY"),
            }
        }
        assert!(
            result.is_err(),
            "AnthropicClient::new() must fail when ANTHROPIC_BASE_URL is non-https non-loopback"
        );
        let err_msg = result.err().expect("already asserted is_err").to_string();
        assert!(
            err_msg.contains("ResilientClient") || err_msg.contains("https"),
            "error must mention https scheme guard: {err_msg}"
        );
    }

    /// Loopback http:// is the documented exception for development /
    /// mock-server scenarios. `AnthropicClient::new()` must succeed when
    /// `ANTHROPIC_BASE_URL=http://127.0.0.1:8080`.
    #[test]
    fn resilient_client_accepts_loopback_http_base_url() {
        let _g = DUMP_TEST_LOCK.lock().unwrap();
        let prior_url = std::env::var("ANTHROPIC_BASE_URL").ok();
        let prior_swfc = std::env::var("ECAA_ANTHROPIC_API_KEY").ok();
        unsafe {
            std::env::set_var("ANTHROPIC_BASE_URL", "http://127.0.0.1:8080");
            std::env::set_var("ECAA_ANTHROPIC_API_KEY", "sk-ant-test-dummy");
        }
        let result = AnthropicClient::new();
        unsafe {
            match prior_url {
                Some(v) => std::env::set_var("ANTHROPIC_BASE_URL", v),
                None => std::env::remove_var("ANTHROPIC_BASE_URL"),
            }
            match prior_swfc {
                Some(v) => std::env::set_var("ECAA_ANTHROPIC_API_KEY", v),
                None => std::env::remove_var("ECAA_ANTHROPIC_API_KEY"),
            }
        }
        assert!(
            result.is_ok(),
            "AnthropicClient::new() must accept loopback http:// for dev/test"
        );
    }
}
