//! Cheap, read-only side-call LLM hops routed through
//! `ModelPolicy::for_side_call()` (Haiku 4.5).
//!
//! `for_side_call` is a policy pin: a guardrail that ensures future
//! subagents default to Haiku rather than re-using the main
//! conversation's Sonnet budget. This module is the first caller of
//! that helper, validating the pin against a production feature
//! (session auto-titling) and giving later side-call use cases
//! (narrative TLDRs, error-friendliness rewrites, attachment
//! classifiers) a concrete pattern to copy.
//!
//! # Design invariants
//!
//! - **No cache marker interaction with the main conversation.** Side
//!   calls build their own `TurnRequest` from scratch. They never share
//!   a system prompt with `service::send_turn`, so no change here can
//!   invalidate the Sonnet-side cache prefix.
//! - **Billed through `MetricsStore::record_side_call_usage`.** A
//!   separate cost bucket (`side_call_cost_usd`) keeps the cheap-hop
//!   spend visibly distinct from chat / agent / scorer in the UI's
//!   Performance tab.
//! - **Prompt caching opt-in (R-28).** Haiku 4.5's minimum cacheable
//!   prefix is 4096 tokens. The static rubric is ~80 tokens; baking
//!   the 6-turn dialogue render into the same system block extends
//!   the cacheable prefix enough that detailed intake sessions cross
//!   the threshold. Below 4096 tokens Anthropic ignores
//!   `cache_control` and we don't pay the 1.25× write premium; above
//!   it, the next retry on the same session (transient HTTP failure,
//!   429 backoff) cache-reads at 0.1× input rate. Per-call user
//!   message stays a thin "Generate the title now." stub so the
//!   cacheable suffix isn't perturbed.
//! - **Every side call is idempotent at the caller-site level.** The
//!   `POST /api/chat/session/:id/auto-title` route short-circuits when
//!   `session.title.is_some()` rather than re-invoking this helper, so
//!   a retry loop can't accidentally double-bill.

pub mod explain;
pub mod remediation_proposer;
pub mod renderer_drafter;
pub mod summary;

use crate::anthropic::{LlmBackend, StopReason, TurnRequest};
use crate::metrics::MetricsStore;
use crate::model_policy::ModelPolicy;
use crate::prompt::SystemPromptBlock;
use crate::session::{SessionId, Turn, TurnRole};
use anyhow::{anyhow, Context, Result};
use std::sync::Arc;

/// Minimum number of non-system turns required before `generate_session_title`
/// will fire. Below this, the dialogue is too thin to produce a
/// meaningful label — the route's 400 response steers callers to wait.
///
// AUTO_TITLE_MIN_TURNS — auto-title fires after >= N non-system turns.
// Increased from initial 3 → 6 to reduce title churn on quick clarifications.
// At 3 turns the conversation is mostly greeting + first prose + initial
// classifier response, so titles ended up generic
// ("Bioinformatics Analysis Setup"). At 6+ the SME has had at least one
// back-and-forth on study design and the model has a real signal to
// anchor the label on. If CLAUDE.md or docs/api-reference.md still say
// "≥3", treat the constant here as authoritative and update the docs.
pub const AUTO_TITLE_MIN_TURNS: usize = 6;

/// Absolute cap on the characters we keep from the LLM's output. Haiku
/// occasionally returns a long sentence despite the 3–6-word instruction;
/// truncating here prevents runaway titles in the UI's SessionTree.
pub const AUTO_TITLE_MAX_CHARS: usize = 60;

/// `max_tokens` for the one-shot Haiku call. 30 comfortably fits a 6-word
/// title; the model is instructed to emit only the title so we expect
/// `stop_reason: end_turn` well inside the cap. Setting it tight is a
/// belt-and-braces defense against a prompt regression that would try
/// to generate prose.
const AUTO_TITLE_MAX_OUTPUT_TOKENS: u32 = 30;

/// Haiku output is deterministic at temperature 0; reproducible titles
/// help the UI test the feature against a mock backend that pins a
/// specific response.
const AUTO_TITLE_TEMPERATURE: f32 = 0.0;

/// System prompt lives in its own.txt alongside `prompt_role.txt` and
/// `scorer_prompt.txt` so prompt edits don't require a Rust recompile
/// beyond what `include_str!` already forces.
const AUTO_TITLE_PROMPT: &str = include_str!("auto_title_prompt.txt");

/// Render the first `AUTO_TITLE_MIN_TURNS` non-system turns as plain
/// dialogue the LLM can read. Quick-reply rows / confirmation cards are
/// intentionally skipped — they're UI concerns that don't help labeling.
fn render_dialogue(turns: &[Turn]) -> String {
    let mut out = String::new();
    let mut shown = 0usize;
    for t in turns {
        if t.role == TurnRole::System {
            continue;
        }
        if shown >= AUTO_TITLE_MIN_TURNS {
            break;
        }
        let role = match t.role {
            TurnRole::User => "USER",
            TurnRole::Assistant => "ASSISTANT",
            TurnRole::System => "SYSTEM",
        };
        out.push_str(&format!("{}: {}\n\n", role, t.content));
        shown += 1;
    }
    out
}

/// Trim and cap the LLM's raw output. Strips surrounding whitespace,
/// surrounding quotes (Haiku occasionally wraps the title despite the
/// instruction), and any trailing punctuation. Enforces the
/// `AUTO_TITLE_MAX_CHARS` cap on a UTF-8 char-boundary so we never split
/// a multi-byte grapheme.
fn sanitize_title(raw: &str) -> String {
    let stripped = raw
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`')
        .trim_end_matches(|c: char| c.is_ascii_punctuation())
        .trim()
        .to_string();
    if stripped.chars().count() <= AUTO_TITLE_MAX_CHARS {
        return stripped;
    }
    let mut end = 0usize;
    for (i, _) in stripped.char_indices().take(AUTO_TITLE_MAX_CHARS) {
        end = i;
    }
    // `end` currently points at the START of the last kept char; advance
    // past it so the slice includes it.
    end += stripped[end..]
        .chars()
        .next()
        .map(|c| c.len_utf8())
        .unwrap_or(0);
    stripped[..end].to_string()
}

/// Ask Haiku 4.5 (via `ModelPolicy::for_side_call()`) for a short title
/// summarizing what this session is about.
///
/// # Errors
///
/// - `ShortConversation` — fewer than `AUTO_TITLE_MIN_TURNS` non-system
///   turns. Caller should wait for more dialogue before retrying.
/// - LLM transport / parsing errors propagate as `anyhow::Error` via
///   `context(...)` so the HTTP route can surface them at 5xx status.
///
/// Callers MUST guard against `session.title.is_some()` before invoking
/// this helper; the helper itself is stateless and will happily
/// re-invoke the LLM on every call.
pub async fn generate_session_title(
    backend: Arc<dyn LlmBackend>,
    metrics: &MetricsStore,
    session_id: SessionId,
    conversation: &[Turn],
    archetype_id: Option<&str>,
) -> Result<String> {
    // Non-system turn count gate. Matches the 400 the HTTP route
    // returns; raised here as a distinct error type so tests can
    // assert on it without parsing error strings.
    let non_system_turns = conversation
        .iter()
        .filter(|t| t.role != TurnRole::System)
        .count();
    if non_system_turns < AUTO_TITLE_MIN_TURNS {
        return Err(anyhow!(
            "session has only {} non-system turn(s); auto-title requires at least {}",
            non_system_turns,
            AUTO_TITLE_MIN_TURNS
        ));
    }

    let dialogue = render_dialogue(conversation);
    // When the composer pinned an archetype on this
    // session, surface its id to the title prompt so the model can
    // suffix `" — <archetype_id>"` for SessionTree readability. None
    // for legacy / backward-chain sessions; the prompt template
    // handles both branches.
    let archetype_hint = archetype_id
        .map(|id| format!("ARCHETYPE: {}\n\n", id))
        .unwrap_or_default();
    // R-28 — system block carries the rubric + dialogue + archetype
    // hint so the cacheable prefix is as long as possible (crosses
    // Haiku's 4096-token cache-min on detailed intakes). Per-call
    // user message stays a stub so the cacheable suffix doesn't
    // shift between retries.
    let system_text = format!(
        "{}\n\n{}DIALOGUE:\n\n{}",
        AUTO_TITLE_PROMPT, archetype_hint, dialogue
    );
    let model = ModelPolicy::for_side_call();
    let req = TurnRequest {
        // Side-call system prompt is its OWN block; it never overlaps
        // with the main conversation's `build_system_prompt` output, so
        // no cache key cross-talk is possible.
        system_prompt: vec![SystemPromptBlock {
            text: system_text,
            // R-28 — opt in so the marker is sent. Anthropic ignores
            // `cache_control` below the 4096-token minimum so we pay
            // the 1.25× write premium ONLY when the prefix is long
            // enough to actually cache (detailed intake dialogues);
            // shorter sessions fall back to standard billing.
            cache: true,
        }],
        conversation: Arc::new(vec![Turn::user("Generate the title now.".to_string())]),
        tool_schemas: vec![],
        model,
        temperature: AUTO_TITLE_TEMPERATURE,
        max_tokens: AUTO_TITLE_MAX_OUTPUT_TOKENS,
        tool_exchange: vec![],
        tool_choice: None,
    };

    let resp = backend
        .send_turn(req)
        .await
        .context("auto-title LLM call failed")?;
    if resp.stop_reason != StopReason::EndTurn {
        return Err(anyhow!(
            "auto-title expected end_turn stop reason, got {:?} (model returned {:?})",
            resp.stop_reason,
            resp.assistant_content
        ));
    }

    metrics
        .record_side_call_usage(
            session_id,
            model,
            resp.usage.input_tokens as u64,
            resp.usage.output_tokens as u64,
            resp.usage.cache_read_input_tokens as u64,
            resp.usage.cache_creation_input_tokens as u64,
        )
        .await;

    let title = sanitize_title(&resp.assistant_content);
    if title.is_empty() {
        return Err(anyhow!(
            "auto-title produced an empty title after sanitization (raw output: {:?})",
            resp.assistant_content
        ));
    }
    Ok(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::{TurnResponse, Usage};
    use crate::model_policy::ModelId;
    use crate::session::Turn;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Mock backend that records every request and returns a canned
    /// response. Unlike `MockLlmBackend` in the crate, this one exposes
    /// the captured requests so tests can assert the side-call path
    /// invokes Haiku (not Sonnet).
    struct CapturingMock {
        requests: Mutex<Vec<TurnRequest>>,
        response: Mutex<TurnResponse>,
    }

    impl CapturingMock {
        fn new(response_text: &str) -> Arc<Self> {
            Arc::new(Self {
                requests: Mutex::new(Vec::new()),
                response: Mutex::new(TurnResponse {
                    assistant_content: response_text.to_string(),
                    tool_uses: Vec::new(),
                    stop_reason: StopReason::EndTurn,
                    usage: Usage {
                        input_tokens: 100,
                        output_tokens: 5,
                        cache_read_input_tokens: 0,
                        cache_creation_input_tokens: 0,
                    },

                    request_metadata: Default::default(),
                }),
            })
        }
    }

    #[async_trait]
    impl LlmBackend for CapturingMock {
        async fn send_turn(&self, req: TurnRequest) -> Result<TurnResponse> {
            self.requests.lock().unwrap().push(req);
            Ok(self.response.lock().unwrap().clone())
        }
        async fn send_turn_streaming(
            &self,
            req: TurnRequest,
            _on_delta: crate::anthropic::delta_sink::DeltaSink,
        ) -> Result<TurnResponse> {
            self.send_turn(req).await
        }
    }

    fn six_turns() -> Vec<Turn> {
        // Plan S5.4 / D-R4: AUTO_TITLE_MIN_TURNS bumped from 3 to 6.
        // The fixture carries enough
        // dialogue to satisfy the new gate.
        vec![
            Turn::user("I need to run DE analysis on bulk rnaseq from 6 liver samples"),
            Turn::assistant("Got it — 6 samples, bulk RNA-seq, differential expression."),
            Turn::user("yes, compare treated vs control"),
            Turn::assistant("Are these paired samples or independent groups?"),
            Turn::user("independent groups, both arms randomly assigned"),
            Turn::assistant("Understood — running an unpaired contrast on 6 samples."),
        ]
    }

    #[tokio::test]
    async fn generates_title_via_haiku() {
        let backend = CapturingMock::new("Bulk RNA-seq DE liver");
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();

        let title = generate_session_title(backend.clone(), &metrics, id, &six_turns(), None)
            .await
            .unwrap();
        assert_eq!(title, "Bulk RNA-seq DE liver");

        // Exactly one request was issued — auto-title is one-shot,
        // never a tool loop.
        let reqs = backend.requests.lock().unwrap();
        assert_eq!(reqs.len(), 1);
    }

    #[tokio::test]
    async fn routes_through_for_side_call_model() {
        // Regression guard for the §3.15 policy pin. If a future refactor
        // "simplifies" generate_session_title to use the main chat
        // model, the side-call cost bucket would evaporate and we'd
        // start paying ~3× for what was supposed to be cheap.
        let backend = CapturingMock::new("Weekly ARIMA forecast");
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let _ = generate_session_title(backend.clone(), &metrics, id, &six_turns(), None)
            .await
            .unwrap();
        let reqs = backend.requests.lock().unwrap();
        assert_eq!(reqs[0].model, ModelId::Haiku45);
        assert_eq!(reqs[0].model, ModelPolicy::for_side_call());
    }

    #[tokio::test]
    async fn records_usage_in_side_call_bucket_not_chat() {
        // The whole point of a separate bucket is that auto-title spend
        // shows up in its own row in the Performance tab. Regression
        // guard: if someone accidentally calls record_chat_usage from
        // here, this test fires.
        let backend = CapturingMock::new("interim analysis trial plan");
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let _ = generate_session_title(backend, &metrics, id, &six_turns(), None)
            .await
            .unwrap();
        let snap = metrics.snapshot(id).await.unwrap();
        assert!(snap.side_call_cost_usd > 0.0, "side-call bucket empty");
        assert!(
            (snap.chat_cost_usd - 0.0).abs() < 1e-9,
            "chat bucket polluted by side-call: {}",
            snap.chat_cost_usd
        );
        // The haiku model key should be the only entry in the side-call
        // per-model map.
        let keys: Vec<_> = snap.per_model_side_call_cost_usd.keys().cloned().collect();
        assert_eq!(keys, vec!["haiku_4_5"]);
    }

    #[tokio::test]
    async fn rejects_short_conversations() {
        // Two turns is below AUTO_TITLE_MIN_TURNS (6 post-S5.4). The route surfaces
        // this as 400; the helper surfaces it as an error a test can
        // detect without string-parsing.
        let backend = CapturingMock::new("anything");
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let err = generate_session_title(
            backend.clone(),
            &metrics,
            id,
            &[Turn::user("hi"), Turn::assistant("hello")],
            None,
        )
        .await
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("auto-title requires at least"),
            "expected short-conversation error, got: {}",
            err
        );
        // No LLM call should have been made.
        assert!(backend.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn sanitizes_output_trims_quotes_and_trailing_punctuation() {
        // Haiku occasionally wraps the title in quotes or ends with a
        // period despite the instructions. The sanitizer strips these
        // so the SessionTree renders cleanly.
        let backend = CapturingMock::new("  \"Clinical trial IA plan.\"  ");
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let title = generate_session_title(backend, &metrics, id, &six_turns(), None)
            .await
            .unwrap();
        assert_eq!(title, "Clinical trial IA plan");
    }

    #[tokio::test]
    async fn sanitizes_output_caps_length() {
        // A runaway Haiku response shouldn't overflow the SessionTree
        // row. Cap at AUTO_TITLE_MAX_CHARS on a UTF-8 boundary.
        let long = "x".repeat(200);
        let backend = CapturingMock::new(&long);
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let title = generate_session_title(backend, &metrics, id, &six_turns(), None)
            .await
            .unwrap();
        assert!(
            title.chars().count() <= AUTO_TITLE_MAX_CHARS,
            "title exceeded cap: {} chars",
            title.chars().count()
        );
    }

    /// When an archetype id is supplied, the prompt
    /// surfaces it as `ARCHETYPE: <id>` so the labeler can append
    /// ` — <id>` to the summary. R-28 moved the archetype hint and
    /// dialogue into the system block so the cacheable prefix is
    /// extended; the test now asserts on the system_prompt content.
    #[tokio::test]
    async fn auto_title_includes_archetype_in_prompt_when_supplied() {
        let backend = CapturingMock::new("IVD scRNA-seq baseline — single_cell_de");
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let _ = generate_session_title(
            backend.clone(),
            &metrics,
            id,
            &six_turns(),
            Some("single_cell_de"),
        )
        .await
        .unwrap();
        let captured = backend.requests.lock().unwrap();
        let req = captured.first().expect("captured one request");
        let system_text: String = req
            .system_prompt
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            system_text.contains("ARCHETYPE: single_cell_de"),
            "system prompt missing archetype hint: {system_text}"
        );
    }

    /// When no archetype is supplied (legacy or
    /// backward-chain sessions), the `ARCHETYPE:` line is absent
    /// from the prompt so the labeler falls back to the summary-only
    /// shape.
    #[tokio::test]
    async fn auto_title_omits_archetype_line_when_none() {
        let backend = CapturingMock::new("Bulk RNA-seq DE analysis");
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let _ = generate_session_title(backend.clone(), &metrics, id, &six_turns(), None)
            .await
            .unwrap();
        let captured = backend.requests.lock().unwrap();
        let req = captured.first().expect("captured one request");
        let system_text: String = req
            .system_prompt
            .iter()
            .map(|b| b.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        // The prompt rules text contains a backtick-quoted `ARCHETYPE:` reference,
        // so we check for the actual injected hint form `"\n\nARCHETYPE: "` which
        // only appears when an archetype_id is supplied.
        assert!(
            !system_text.contains("\n\nARCHETYPE: "),
            "ARCHETYPE: hint line should be absent when archetype_id is None"
        );
    }

    #[tokio::test]
    async fn errors_on_non_end_turn_stop_reason() {
        // If Haiku returns tool_use or max_tokens (the max_tokens case
        // would happen if a future prompt regression asked for prose
        // despite the 30-token cap), surface a clean error rather than
        // store a half-finished title.
        let backend = Arc::new(CapturingMock {
            requests: Mutex::new(Vec::new()),
            response: Mutex::new(TurnResponse {
                assistant_content: "truncated midwa".to_string(),
                tool_uses: Vec::new(),
                stop_reason: StopReason::MaxTokens,
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 30,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },

                request_metadata: Default::default(),
            }),
        });
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let err = generate_session_title(backend, &metrics, id, &six_turns(), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("end_turn"),
            "expected stop-reason error: {}",
            err
        );
    }

    #[test]
    fn sanitize_is_a_pure_function_level_noop_on_clean_input() {
        // A clean Haiku output round-trips unchanged.
        assert_eq!(
            sanitize_title("Bulk RNA-seq DE liver"),
            "Bulk RNA-seq DE liver"
        );
        assert_eq!(sanitize_title(""), "");
    }
}
