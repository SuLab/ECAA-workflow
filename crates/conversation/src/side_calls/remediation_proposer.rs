//! One-shot LLM proposer for `BlockerKind::ToolError` blockers.
//!
//! Input: a `ToolErrorEnvelope` + optional stage-taxonomy / intake-fact
//! context. Output: a ranked `Vec<RemediationSuggestion>` (1–3 items).
//!
//! Routed through `ModelPolicy::for_remediation_proposer()` (Opus 4.7
//! today — same model the main conversation escalates to on Blocked
//! state, so the proposer's reasoning quality matches what the SME
//! sees in chat). One-shot, structured-JSON output; we parse the
//! assistant_content as `Vec<RemediationSuggestion>` directly. No
//! tool-use scheme — that would require adding to the closed 16-tool
//! vocabulary, which is reserved for state-mutating actions.
//!
//! Cost is billed via `record_side_call_usage` so the Performance tab
//! can show remediation-proposer spend separately from chat / agent /
//! scorer.

use crate::anthropic::{LlmBackend, StopReason, TurnRequest};
use crate::metrics::MetricsStore;
use crate::model_policy::ModelPolicy;
use crate::prompt::SystemPromptBlock;
use crate::session::{SessionId, Turn};
use anyhow::{anyhow, Context, Result};
use scripps_workflow_core::error_envelope::ToolErrorEnvelope;
use scripps_workflow_core::remediation::{
    AppliedRemediation, RemediationSuggestion, MAX_REMEDIATION_ATTEMPTS,
};
use std::sync::Arc;

const PROPOSER_PROMPT: &str = include_str!("remediation_proposer_prompt.txt");

/// `max_tokens` for the proposer call. 2000 fits 3 detailed suggestions
/// with rationales without truncating; the schema cap of 3 limits the
/// total volume.
const PROPOSER_MAX_OUTPUT_TOKENS: u32 = 2000;

/// Temperature is 0 for deterministic output — same envelope should
/// produce the same suggestions on retry.
const PROPOSER_TEMPERATURE: f32 = 0.0;

/// Maximum suggestions the proposer is allowed to return. Three is
/// enough to give the SME real choice without overwhelming the
/// BlockerCard.
pub const MAX_SUGGESTIONS_PER_BLOCKER: usize = 3;

/// Optional context layered on top of the envelope. The proposer can
/// reason without these but produces better-targeted suggestions when
/// they're present.
#[derive(Debug, Default, Clone)]
pub struct ProposerContext {
    /// Stage description / claim boundary from the taxonomy YAML, when
    /// available. Helps the proposer respect "method neutrality"
    /// (don't propose a specific aligner if the SME hasn't named one).
    pub stage_description: Option<String>,
    /// Selected intake facts for sizing-relevant suggestions
    /// (sample_count, cell_count, genome_size_gb).
    pub intake_summary: Option<String>,
    /// Prior remediation attempts on this task. The proposer is
    /// instructed to avoid repeating a remediation that already failed.
    pub prior_attempts: Vec<AppliedRemediation>,
}

/// Ask the proposer for ranked remediation suggestions.
///
/// # Errors
/// - `AttemptsExhausted` — `prior_attempts.len() >= MAX_REMEDIATION_ATTEMPTS`.
/// - LLM transport / parse errors propagate as `anyhow::Error` via
///   `context(...)` so the caller can surface them at 5xx status.
pub async fn propose_remediations(
    backend: Arc<dyn LlmBackend>,
    metrics: &MetricsStore,
    session_id: SessionId,
    envelope: &ToolErrorEnvelope,
    ctx: &ProposerContext,
) -> Result<Vec<RemediationSuggestion>> {
    if ctx.prior_attempts.len() as u32 >= MAX_REMEDIATION_ATTEMPTS {
        return Err(anyhow!(
            "remediation attempts exhausted ({} of {} cap)",
            ctx.prior_attempts.len(),
            MAX_REMEDIATION_ATTEMPTS
        ));
    }

    let user_prompt = render_user_prompt(envelope, ctx);
    let model = ModelPolicy::for_remediation_proposer();
    let req = TurnRequest {
        system_prompt: vec![SystemPromptBlock {
            text: PROPOSER_PROMPT.to_string(),
            // R-28 — the proposer prompt is ~3KB static rubric that
            // never varies between calls; flipping cache: true lets
            // every repeat call within the 5-minute TTL cache-read
            // at 0.1× input rate. Per-call envelope + context stays
            // uncached in the user turn so its uniqueness can't
            // invalidate the cacheable prefix.
            cache: true,
        }],
        conversation: Arc::new(vec![Turn::user(user_prompt)]),
        tool_schemas: vec![],
        model,
        temperature: PROPOSER_TEMPERATURE,
        max_tokens: PROPOSER_MAX_OUTPUT_TOKENS,
        tool_exchange: vec![],
        tool_choice: None,
    };

    let resp = backend
        .send_turn(req)
        .await
        .context("remediation proposer LLM call failed")?;
    if resp.stop_reason != StopReason::EndTurn {
        return Err(anyhow!(
            "remediation proposer expected end_turn, got {:?}",
            resp.stop_reason
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

    let mut suggestions = parse_suggestions(&resp.assistant_content)
        .with_context(|| format!("parsing proposer output: {}", resp.assistant_content))?;
    suggestions.truncate(MAX_SUGGESTIONS_PER_BLOCKER);
    Ok(suggestions)
}

/// Render the `USER` half of the proposer call.
fn render_user_prompt(envelope: &ToolErrorEnvelope, ctx: &ProposerContext) -> String {
    let envelope_json = serde_json::to_string_pretty(envelope).unwrap_or_else(|_| "{}".to_string());
    let mut out = String::new();
    out.push_str("ENVELOPE:\n");
    out.push_str(&envelope_json);
    out.push_str("\n\n");
    if let Some(d) = &ctx.stage_description {
        out.push_str("STAGE_DESCRIPTION:\n");
        out.push_str(d);
        out.push_str("\n\n");
    }
    if let Some(s) = &ctx.intake_summary {
        out.push_str("INTAKE_SUMMARY:\n");
        out.push_str(s);
        out.push_str("\n\n");
    }
    if !ctx.prior_attempts.is_empty() {
        out.push_str("PRIOR_REMEDIATION_ATTEMPTS:\n");
        for attempt in &ctx.prior_attempts {
            out.push_str(&format!(
                "- id={} kind={} outcome={:?}\n",
                attempt.suggestion_id,
                serde_json::to_string(&attempt.kind).unwrap_or_else(|_| "?".into()),
                attempt.outcome
            ));
        }
        out.push('\n');
    }
    out.push_str(
        "Emit a JSON array of 1 to 3 RemediationSuggestion objects, ranked best first. \
        No prose around it.\n",
    );
    out
}

/// Parse the LLM's text output as a JSON array of `RemediationSuggestion`.
///
/// Tolerates a few common output shapes:
/// * Bare JSON array: `[ {...}, {...} ]`
/// * Markdown-fenced block: ` ```json\n[... ]\n``` `
/// * Single-object output (older Anthropic models): `{...}` → `[{...}]`
fn parse_suggestions(raw: &str) -> Result<Vec<RemediationSuggestion>> {
    let trimmed = raw.trim();
    let stripped = strip_fence(trimmed);
    if stripped.starts_with('[') {
        return serde_json::from_str(stripped).context("parsing JSON array of suggestions");
    }
    if stripped.starts_with('{') {
        let one: RemediationSuggestion =
            serde_json::from_str(stripped).context("parsing JSON object as single suggestion")?;
        return Ok(vec![one]);
    }
    Err(anyhow!(
        "proposer output did not start with `[` or `{{` after fence stripping"
    ))
}

fn strip_fence(s: &str) -> &str {
    let s = s.trim();
    if let Some(rest) = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```JSON"))
        .or_else(|| s.strip_prefix("```"))
    {
        return rest.trim().trim_end_matches("```").trim();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::{TurnResponse, Usage};
    use crate::model_policy::ModelId;
    use async_trait::async_trait;
    use scripps_workflow_core::error_envelope::synthesize;
    use scripps_workflow_core::error_envelope::EnvelopeInput;
    use std::sync::Mutex as StdMutex;

    struct StubBackend {
        captured: StdMutex<Vec<TurnRequest>>,
        canned: String,
        stop: StopReason,
    }

    impl StubBackend {
        fn new(canned: &str) -> Arc<Self> {
            Arc::new(Self {
                captured: StdMutex::new(Vec::new()),
                canned: canned.to_string(),
                stop: StopReason::EndTurn,
            })
        }
    }

    #[async_trait]
    impl LlmBackend for StubBackend {
        async fn send_turn(&self, req: TurnRequest) -> Result<TurnResponse> {
            self.captured.lock().unwrap().push(req);
            Ok(TurnResponse {
                assistant_content: self.canned.clone(),
                tool_uses: Vec::new(),
                stop_reason: self.stop,
                usage: Usage {
                    input_tokens: 200,
                    output_tokens: 80,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },

                request_metadata: Default::default(),
            })
        }
        async fn send_turn_streaming(
            &self,
            req: TurnRequest,
            _on: crate::anthropic::delta_sink::DeltaSink,
        ) -> Result<TurnResponse> {
            self.send_turn(req).await
        }
    }

    fn oom_envelope() -> ToolErrorEnvelope {
        synthesize(EnvelopeInput {
            task_id: "alignment".into(),
            stage_id: "alignment".into(),
            library: Some("STAR".into()),
            stderr: "STAR: out of memory; killed",
            executor: "local".into(),
            captured_at: "2026-05-04T00:00:00Z".into(),
            exit_code: Some(137),
            signal: Some("SIGKILL".into()),
            attempt: 1,
            ..Default::default()
        })
    }

    #[tokio::test]
    async fn parses_well_formed_array_response() {
        let canned = r#"[
            {
                "id": "rs-001",
                "kind": {
                    "kind": "bump_resources",
                    "target": { "memory_gb": 64 }
                },
                "rationale": "STAR ran out of memory at 32 GiB; bump to 64.",
                "confidence": "high",
                "evidence": ["error_class", "signal"],
                "tool_binding": "rerun_task"
            }
        ]"#;
        let backend = StubBackend::new(canned);
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let ctx = ProposerContext::default();
        let suggestions = propose_remediations(backend, &metrics, id, &oom_envelope(), &ctx)
            .await
            .unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].id, "rs-001");
    }

    #[tokio::test]
    async fn parses_markdown_fenced_response() {
        let canned = "```json\n[{\"id\":\"a\",\"kind\":{\"kind\":\"retry_as_is\",\"reason\":\"x\"},\"rationale\":\"r\",\"confidence\":\"low\",\"evidence\":[],\"tool_binding\":\"rerun_task\"}]\n```";
        let backend = StubBackend::new(canned);
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let ctx = ProposerContext::default();
        let suggestions = propose_remediations(backend, &metrics, id, &oom_envelope(), &ctx)
            .await
            .unwrap();
        assert_eq!(suggestions.len(), 1);
    }

    #[tokio::test]
    async fn truncates_to_three_suggestions() {
        let mut items = Vec::new();
        for i in 0..5 {
            items.push(format!(
                r#"{{
                    "id": "rs-{i}",
                    "kind": {{"kind":"retry_as_is","reason":"x"}},
                    "rationale": "r",
                    "confidence": "low",
                    "evidence": [],
                    "tool_binding": "rerun_task"
                }}"#
            ));
        }
        let canned = format!("[{}]", items.join(","));
        let backend = StubBackend::new(&canned);
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let ctx = ProposerContext::default();
        let suggestions = propose_remediations(backend, &metrics, id, &oom_envelope(), &ctx)
            .await
            .unwrap();
        assert_eq!(suggestions.len(), MAX_SUGGESTIONS_PER_BLOCKER);
    }

    #[tokio::test]
    async fn errors_on_attempts_exhausted() {
        let backend = StubBackend::new("[]");
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let mut ctx = ProposerContext::default();
        for i in 0..MAX_REMEDIATION_ATTEMPTS {
            ctx.prior_attempts.push(AppliedRemediation {
                suggestion_id: format!("a{}", i),
                kind: scripps_workflow_core::remediation::RemediationKind::RetryAsIs {
                    reason: "x".into(),
                },
                applied_at: "now".into(),
                applied_by: "sme".into(),
                outcome: scripps_workflow_core::remediation::RemediationOutcome::Recurred,
            });
        }
        let err = propose_remediations(backend, &metrics, id, &oom_envelope(), &ctx)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("remediation attempts exhausted"));
    }

    #[tokio::test]
    async fn routes_through_remediation_proposer_model() {
        let canned = r#"[]"#;
        let backend = StubBackend::new(canned);
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let ctx = ProposerContext::default();
        let _ = propose_remediations(backend.clone(), &metrics, id, &oom_envelope(), &ctx).await;
        let reqs = backend.captured.lock().unwrap();
        assert_eq!(reqs[0].model, ModelId::Opus47);
        assert_eq!(reqs[0].model, ModelPolicy::for_remediation_proposer());
    }

    #[tokio::test]
    async fn surfaces_parse_failure_for_malformed_output() {
        let backend = StubBackend::new("not json at all");
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let ctx = ProposerContext::default();
        let err = propose_remediations(backend, &metrics, id, &oom_envelope(), &ctx)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("parsing proposer output"));
    }

    #[test]
    fn render_user_prompt_includes_envelope_and_attempts() {
        let env = oom_envelope();
        let ctx = ProposerContext {
            stage_description: Some("STAR alignment stage".into()),
            intake_summary: Some("6 samples, hg38".into()),
            prior_attempts: vec![],
        };
        let p = render_user_prompt(&env, &ctx);
        assert!(p.contains("STAR alignment stage"));
        assert!(p.contains("6 samples"));
        assert!(p.contains("ENVELOPE"));
        assert!(p.contains("OOM"));
    }
}
