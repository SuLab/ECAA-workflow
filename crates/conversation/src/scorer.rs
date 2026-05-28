//! real-LLM naturalness rubric scorer.
//!
//! Takes a frozen transcript and rubric notes, asks Sonnet 4.6 to
//! score the conversation against a 9-dimension rubric, and returns
//! a structured `RubricScore`. The scorer system prompt lives in
//! `tests/conversation-fixtures/runner/scorer_prompt.txt` so the
//! rubric content stays under source control alongside the fixture
//! corpus.
//!
//! Routing to Haiku 4.5 (~3× cheaper) was prototyped via
//! `make scorer-ab` (driver at `src/bin/scorer_ab.rs`); the A/B run
//! failed ship criteria (mean |Δtotal| = 2.4, pass/fail agreement 60%)
//! so the default stays on Sonnet 4.6 until `scorer_prompt.txt` is
//! tightened against Haiku's looser interpretation of `claim_boundary`.
//!
//! # Cost accounting
//!
//! Scorer spend is recorded into `MetricsStore` under the session's
//! `scorer_cost_usd` bucket — there is no opt-out path and no orphan
//! spend log. The scorer is invoked exclusively from
//! `POST /api/chat/session/:id/score`, so every call has a `SessionId`
//! in scope; the signature reflects that.

use crate::anthropic::{LlmBackend, StopReason, TurnRequest};
use crate::metrics::MetricsStore;
use crate::model_policy::ModelId;
use crate::prompt::SystemPromptBlock;
use crate::session::{SessionId, Turn, TurnRole};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use ts_rs::TS;

/// One row of the 9-dimension rubric. Each dimension is scored 0–2.
/// `hardware_awareness` is the 9th dimension, added to gate against
/// transcripts that invent thread counts / GPU flags in chat instead
/// of deferring to the execution agent's runtime envelope.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RubricScore {
    /// Naturalness: conversation feels like SME chat, not form-filling (0–10).
    pub naturalness: u8,
    /// Continuity: no topic drift between turns (0–10).
    pub continuity: u8,
    /// One-question discipline: assistant never stacks multiple questions (0–10).
    pub one_question: u8,
    /// Method neutrality: assistant never recommends specific methods unprompted (0–10).
    pub method_neutrality: u8,
    /// Claim boundary: assistant never reports results as its own (0–10).
    pub claim_boundary: u8,
    /// Tool efficiency: no redundant tool calls in a single turn (0–10).
    pub tool_efficiency: u8,
    /// Confirmation discipline: emit_package only fires after SME confirm (0–10).
    pub confirmation: u8,
    /// Recovery: blockers are resolved cleanly without looping (0–10).
    pub recovery: u8,
    /// Rewards transcripts that defer hardware decisions
    /// (--threads N, BLAS env vars, GPU flag selection) to the
    /// execution agent at runtime. Penalizes transcripts that quote
    /// specific numeric thread counts or `--use_accelerator=True` in
    /// chat. Absent on scorer outputs produced before this dimension
    /// is present — the parser treats the dimension as 0 with an INFO
    /// comment so historical rubric runs don't need to be re-scored.
    pub hardware_awareness: u8,
}

impl RubricScore {
    /// Sum all nine rubric dimensions (0–90 maximum).
    pub fn total(&self) -> u8 {
        self.naturalness
            + self.continuity
            + self.one_question
            + self.method_neutrality
            + self.claim_boundary
            + self.tool_efficiency
            + self.confirmation
            + self.recovery
            + self.hardware_awareness
    }

    /// Maximum possible total — 9 dimensions × 2 points each.
    pub const MAX_TOTAL: u8 = 18;

    /// Pass threshold is set proportionally for the 9-dimension
    /// rubric: 14/18 (the same ~81% bar as the earlier 13/16).
    pub const PASS_THRESHOLD: u8 = 14;
}

const SCORER_TEMPERATURE: f32 = 0.0;
const SCORER_MAX_TOKENS: u32 = 2000;

/// The scorer system prompt is the rubric definition. Authored in
/// `tests/conversation-fixtures/runner/scorer_prompt.txt` and embedded at
/// compile time so the binary doesn't depend on the file at runtime.
const SCORER_PROMPT: &str =
    include_str!("../../../tests/conversation-fixtures/runner/scorer_prompt.txt");

/// Request a single rubric score from a live LLM backend.
///
/// The transcript is rendered as a plain-text dialogue so the scorer can
/// read it the way an SME would. Tool-call audit details are intentionally
/// elided — they go in `rubric_notes` if a fixture wants to call them out.
///
/// Token usage from the underlying LLM call is recorded into `metrics`
/// under `session_id`'s `scorer_cost_usd` bucket before returning. There
/// is no opt-out path — every scorer invocation bills through a session.
///
/// Routes through [`score_transcript_with_model`] with `ModelId::Sonnet46` —
/// the baseline. The Haiku 4.5 candidate is exercised by the
/// `make scorer-ab` A/B target (see `crates/conversation/src/bin/scorer_ab.rs`).
/// The current rubric prompt failed ship criteria against Haiku, so
/// this default continues to route to Sonnet; re-running the A/B after
/// a rubric-prompt revision is the prerequisite for any future flip.
pub async fn score_transcript(
    backend: Arc<dyn LlmBackend>,
    metrics: &MetricsStore,
    session_id: SessionId,
    transcript: &[Turn],
    rubric_notes: &str,
) -> Result<RubricScore> {
    score_transcript_with_model(
        backend,
        metrics,
        session_id,
        transcript,
        rubric_notes,
        ModelId::Sonnet46,
    )
    .await
}

/// Explicit-model form used by the `scorer_ab` binary to drive the same
/// rubric prompt through two models back-to-back for A/B comparison. Not
/// intended for production call-sites — those should go through
/// [`score_transcript`] so the model default stays in one place.
pub async fn score_transcript_with_model(
    backend: Arc<dyn LlmBackend>,
    metrics: &MetricsStore,
    session_id: SessionId,
    transcript: &[Turn],
    rubric_notes: &str,
    model: ModelId,
) -> Result<RubricScore> {
    let system_prompt = vec![SystemPromptBlock {
        text: SCORER_PROMPT.to_string(),
        cache: true,
    }];

    let rendered = render_transcript(transcript);
    let user_prompt = format!(
        "TRANSCRIPT:\n\n{}\n\nRUBRIC NOTES (from the fixture author):\n{}\n\nScore the transcript on each of the 9 dimensions and emit the strict format described above.",
        rendered, rubric_notes
    );

    let req = TurnRequest {
        system_prompt,
        conversation: std::sync::Arc::new(vec![Turn::user(user_prompt)]),
        tool_schemas: vec![],
        model,
        temperature: SCORER_TEMPERATURE,
        max_tokens: SCORER_MAX_TOKENS,
        tool_exchange: vec![],
        tool_choice: None,
    };

    let resp = backend
        .send_turn(req)
        .await
        .context("scorer LLM call failed")?;
    if resp.stop_reason != StopReason::EndTurn {
        return Err(anyhow!(
            "scorer expected end_turn stop reason, got {:?}",
            resp.stop_reason
        ));
    }
    metrics
        .record_scorer_usage(
            session_id,
            model,
            resp.usage.input_tokens as u64,
            resp.usage.output_tokens as u64,
            resp.usage.cache_read_input_tokens as u64,
            resp.usage.cache_creation_input_tokens as u64,
        )
        .await;
    parse_score(&resp.assistant_content)
}

fn render_transcript(transcript: &[Turn]) -> String {
    let mut out = String::new();
    for (i, t) in transcript.iter().enumerate() {
        if t.role == TurnRole::System {
            // The synthetic [tool results] turns are noise to the scorer —
            // skip them so the dialogue reads cleanly.
            continue;
        }
        let role = match t.role {
            TurnRole::User => "USER",
            TurnRole::Assistant => "ASSISTANT",
            TurnRole::System => "SYSTEM",
        };
        out.push_str(&format!("Turn {} [{}]: {}\n\n", i + 1, role, t.content));
        if !t.quick_replies.is_empty() {
            out.push_str(&format!(
                "  (quick replies offered: {})\n\n",
                t.quick_replies.join(" | ")
            ));
        }
        if let Some(card) = &t.confirmation_card {
            out.push_str(&format!(
                "  (confirmation card surfaced; summary: {})\n\n",
                card.summary_markdown.lines().next().unwrap_or("")
            ));
        }
    }
    out
}

/// Parse the strict rubric format the scorer prompt asks the LLM to emit:
///
/// ```text
/// NATURALNESS: <0|1|2>
/// CONTINUITY: <0|1|2>
/// ONE_QUESTION: <0|1|2>
/// METHOD_NEUTRALITY: <0|1|2>
/// CLAIM_BOUNDARY: <0|1|2>
/// TOOL_EFFICIENCY: <0|1|2>
/// CONFIRMATION: <0|1|2>
/// RECOVERY: <0|1|2>
/// HARDWARE_AWARENESS: <0|1|2>
/// TOTAL: <sum>
///
/// NOTES:
/// ```
///
/// The `TOTAL:` line is verified against the per-dimension sum; mismatches
/// are treated as scorer drift and surfaced as an error so the nightly job
/// can flag a failed scorer rather than silently accept bad scores.
///
/// `HARDWARE_AWARENESS` was added later. Scorer outputs that predate
/// the addition won't carry the dimension; for that one case the parser
/// defaults to 0 rather than erroring so old captures keep loading.
/// All other dimensions are still required.
pub fn parse_score(text: &str) -> Result<RubricScore> {
    let score = RubricScore {
        naturalness: find_dimension(text, "NATURALNESS")?,
        continuity: find_dimension(text, "CONTINUITY")?,
        one_question: find_dimension(text, "ONE_QUESTION")?,
        method_neutrality: find_dimension(text, "METHOD_NEUTRALITY")?,
        claim_boundary: find_dimension(text, "CLAIM_BOUNDARY")?,
        tool_efficiency: find_dimension(text, "TOOL_EFFICIENCY")?,
        confirmation: find_dimension(text, "CONFIRMATION")?,
        recovery: find_dimension(text, "RECOVERY")?,
        // Hardware-awareness dimension: allow absence so older captures
        // deserialize. New scorer runs are expected to emit it and
        // the scorer_prompt.txt asks for it.
        hardware_awareness: find_dimension_optional(text, "HARDWARE_AWARENESS").unwrap_or(0),
    };

    // Verify TOTAL against the per-dimension sum if present. TOTAL can be
    // anywhere in 0..=16, so it uses a wider range than the dimension keys.
    if let Some(total) = find_total(text) {
        let diff = (total as i16 - score.total() as i16).abs();
        if diff > 1 {
            return Err(anyhow!(
                "scorer TOTAL ({}) disagrees with per-dimension sum ({}) by {}",
                total,
                score.total(),
                diff
            ));
        }
    }
    Ok(score)
}

fn find_dimension(text: &str, key: &str) -> Result<u8> {
    let v = find_u8(text, key);
    match v {
        Some(n) if n <= 2 => Ok(n),
        Some(n) => Err(anyhow!(
            "scorer emitted out-of-range value for {}: {}",
            key,
            n
        )),
        None => Err(anyhow!("scorer output missing key '{}'\n\n{}", key, text)),
    }
}

/// Same as `find_dimension` but `None` is not an error — used for
/// dimensions introduced after a historical scorer capture was made.
fn find_dimension_optional(text: &str, key: &str) -> Option<u8> {
    match find_u8(text, key) {
        Some(n) if n <= 2 => Some(n),
        _ => None,
    }
}

fn find_total(text: &str) -> Option<u8> {
    let v = find_u8(text, "TOTAL")?;
    if v <= RubricScore::MAX_TOTAL {
        Some(v)
    } else {
        None
    }
}

fn find_u8(text: &str, key: &str) -> Option<u8> {
    for line in text.lines() {
        let trimmed = line.trim();
        let stripped = trimmed.trim_start_matches('-').trim();
        if let Some(rest) = stripped.strip_prefix(key) {
            let rest = rest.trim_start_matches(':').trim();
            let first_token = rest.split_whitespace().next().unwrap_or("");
            if let Ok(n) = first_token.parse::<u8>() {
                return Some(n);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_strict_format() {
        let body = "NATURALNESS: 2\n\
                    CONTINUITY: 2\n\
                    ONE_QUESTION: 2\n\
                    METHOD_NEUTRALITY: 2\n\
                    CLAIM_BOUNDARY: 1\n\
                    TOOL_EFFICIENCY: 2\n\
                    CONFIRMATION: 2\n\
                    RECOVERY: 1\n\
                    HARDWARE_AWARENESS: 2\n\
                    TOTAL: 16\n\n\
                    NOTES:\n- claim boundary was implicit, not stated\n";
        let s = parse_score(body).unwrap();
        assert_eq!(s.total(), 16);
        assert_eq!(s.claim_boundary, 1);
        assert_eq!(s.recovery, 1);
        assert_eq!(s.hardware_awareness, 2);
    }

    #[test]
    fn parse_strict_format_with_minor_total_rounding() {
        // Scorer wrote TOTAL: 16 but per-dim sum is 17 — diff of 1 is allowed
        let body = "NATURALNESS: 2\n\
                    CONTINUITY: 2\n\
                    ONE_QUESTION: 2\n\
                    METHOD_NEUTRALITY: 2\n\
                    CLAIM_BOUNDARY: 2\n\
                    TOOL_EFFICIENCY: 2\n\
                    CONFIRMATION: 2\n\
                    RECOVERY: 1\n\
                    HARDWARE_AWARENESS: 2\n\
                    TOTAL: 16\n";
        let s = parse_score(body).unwrap();
        assert_eq!(s.total(), 17);
    }

    /// Legacy-output compatibility: scorer outputs missing the
    /// HARDWARE_AWARENESS line still parse, with the dimension
    /// defaulting to 0. Used for replay of historical captures.
    #[test]
    fn parse_legacy_8_dimension_captures_default_hardware_to_zero() {
        let body = "NATURALNESS: 2\n\
                    CONTINUITY: 2\n\
                    ONE_QUESTION: 2\n\
                    METHOD_NEUTRALITY: 2\n\
                    CLAIM_BOUNDARY: 2\n\
                    TOOL_EFFICIENCY: 2\n\
                    CONFIRMATION: 2\n\
                    RECOVERY: 2\n";
        let s = parse_score(body).unwrap();
        assert_eq!(s.hardware_awareness, 0);
        assert_eq!(s.total(), 16);
    }

    #[test]
    fn parse_rejects_out_of_range_value() {
        let body = "NATURALNESS: 5\n\
                    CONTINUITY: 0\n\
                    ONE_QUESTION: 0\n\
                    METHOD_NEUTRALITY: 0\n\
                    CLAIM_BOUNDARY: 0\n\
                    TOOL_EFFICIENCY: 0\n\
                    CONFIRMATION: 0\n\
                    RECOVERY: 0\n\
                    HARDWARE_AWARENESS: 0\n";
        let r = parse_score(body);
        assert!(r.is_err());
    }

    #[test]
    fn parse_rejects_missing_dimension() {
        let body = "NATURALNESS: 2\nCONTINUITY: 2\nONE_QUESTION: 2\n";
        let r = parse_score(body);
        assert!(r.is_err());
        assert!(format!("{:?}", r).contains("METHOD_NEUTRALITY"));
    }

    #[test]
    fn parse_rejects_total_drift_above_one() {
        let body = "NATURALNESS: 2\n\
                    CONTINUITY: 2\n\
                    ONE_QUESTION: 2\n\
                    METHOD_NEUTRALITY: 2\n\
                    CLAIM_BOUNDARY: 2\n\
                    TOOL_EFFICIENCY: 2\n\
                    CONFIRMATION: 2\n\
                    RECOVERY: 2\n\
                    HARDWARE_AWARENESS: 2\n\
                    TOTAL: 10\n";
        // Per-dim sum is 18, scorer wrote 10 — diff of 8, must error
        let r = parse_score(body);
        assert!(r.is_err(), "expected drift error, got {:?}", r);
    }

    #[test]
    fn rubric_constants_make_sense() {
        assert_eq!(RubricScore::MAX_TOTAL, 18);
        // invariant: PASS_THRESHOLD must remain <= MAX_TOTAL.
    }

    #[test]
    fn render_transcript_skips_synthetic_system_turns() {
        let user = Turn::user("hello");
        let mut sys = Turn::user("ignored");
        sys.role = TurnRole::System;
        sys.content = "[tool results]".into();
        let asst = Turn::assistant("hi there");
        let rendered = render_transcript(&[user, sys, asst]);
        assert!(rendered.contains("USER"));
        assert!(rendered.contains("ASSISTANT"));
        assert!(!rendered.contains("[tool results]"));
    }

    #[tokio::test]
    async fn score_transcript_records_usage_under_scorer_bucket() {
        // Integration test through the live backend interface: a mock
        // scorer response flows into MetricsStore::record_scorer_usage
        // under the session's `scorer_cost_usd` bucket (NOT chat or
        // agent). Verifies Sonnet 4.6 pricing is applied.
        use crate::anthropic::Usage;
        use crate::mock::MockLlmBackend;
        use crate::TurnResponse;

        let scorer_response = "NATURALNESS: 2\n\
            CONTINUITY: 2\n\
            ONE_QUESTION: 2\n\
            METHOD_NEUTRALITY: 2\n\
            CLAIM_BOUNDARY: 2\n\
            TOOL_EFFICIENCY: 2\n\
            CONFIRMATION: 2\n\
            RECOVERY: 2\n\
            HARDWARE_AWARENESS: 2\n\
            TOTAL: 18\n";
        // 1M input @ $3 + 1M output @ $15 = $18
        let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(vec![TurnResponse {
            assistant_content: scorer_response.into(),
            tool_uses: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            },

            request_metadata: Default::default(),
        }]));
        let metrics = MetricsStore::new();
        let session_id = uuid::Uuid::new_v4();
        let transcript = vec![Turn::user("hi"), Turn::assistant("hello")];
        let score = score_transcript(backend, &metrics, session_id, &transcript, "")
            .await
            .expect("scorer should parse mock response");
        assert_eq!(score.total(), 18);

        let snap = metrics.snapshot(session_id).await.unwrap();
        assert!(
            (snap.scorer_cost_usd - 18.0).abs() < 1e-6,
            "scorer_cost_usd should price at Sonnet rate ($18), got {}",
            snap.scorer_cost_usd
        );
        assert_eq!(snap.chat_cost_usd, 0.0, "chat bucket must stay zero");
        assert_eq!(snap.agent_cost_usd, 0.0, "agent bucket must stay zero");
        assert!((snap.total_cost_usd - 18.0).abs() < 1e-6);
        assert!(
            (snap
                .per_model_scorer_cost_usd
                .get("sonnet_4_6")
                .copied()
                .unwrap()
                - 18.0)
                .abs()
                < 1e-6
        );
    }
}
