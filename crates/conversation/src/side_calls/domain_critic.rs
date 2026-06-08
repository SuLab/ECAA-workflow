//! Advisory domain-correctness critic side-call.
//!
//! Reads a completed stage's narrative + a short stage description and
//! returns a `DomainVerdict` (Plausible / ImplausibleMinor /
//! ImplausibleMajor) with a confidence + failed-checks list.
//!
//! # Invariants
//! - **Advisory only.** The verdict is surfaced as a sidecar
//!   (`runtime/outputs/<task_id>/domain_critique.json`). It never blocks
//!   a task, never adds a `BlockerKind`, and never extends the closed
//!   22-tool vocabulary.
//! - **Method-neutral.** The prompt instructs the critic to judge
//!   whether the *reported* result is biologically plausible and whether
//!   the chosen method is *explained* — never to recommend a different
//!   method.
//! - **Billed via `record_side_call_usage`.** Routed through the
//!   `side_call_kind == domain_critic` model-policy rule (Opus 4.8).

use crate::anthropic::{LlmBackend, StopReason, TurnRequest};
use crate::metrics::MetricsStore;
use crate::model_policy::registry::{EvalContext, ModelRoutingTable};
use crate::model_policy::ModelId;
use crate::prompt::SystemPromptBlock;
use crate::session::{SessionId, Turn};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const CRITIC_PROMPT: &str = include_str!("domain_critic_prompt.txt");

/// `max_tokens` for the critic call. 1024 fits a verdict + a paragraph
/// rationale + a handful of failed-check ids without truncating the
/// closing JSON brace.
const CRITIC_MAX_OUTPUT_TOKENS: u32 = 1024;

/// Temperature is 0 for deterministic output — the same narrative
/// should produce the same verdict on re-verify.
const CRITIC_TEMPERATURE: f32 = 0.0;

/// Three-level domain-plausibility verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainVerdictKind {
    /// The reported result is biologically plausible.
    Plausible,
    /// Minor concern; result is usable but a check looks off.
    ImplausibleMinor,
    /// Major concern; the result is biologically implausible.
    ImplausibleMajor,
}

/// Advisory verdict written to the per-task sidecar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainVerdict {
    /// Three-level plausibility verdict.
    pub verdict: DomainVerdictKind,
    /// Critic-reported confidence in `[0.0, 1.0]`.
    pub confidence: f64,
    /// Named checks the critic judged failed (free-form ids).
    pub failed_checks: Vec<String>,
    /// One-paragraph rationale.
    pub rationale: String,
}

/// Resolve the domain-critic model via the routing table
/// (`side_call_kind == domain_critic`).
fn for_domain_critic() -> ModelId {
    ModelRoutingTable::current()
        .resolve(&EvalContext {
            session: None,
            side_call_kind: Some("domain_critic"),
        })
        .model
}

/// Run the advisory domain critic over a completed stage's narrative.
///
/// # Errors
/// Transport / non-`end_turn` / parse failures propagate as `anyhow`.
pub async fn verify_stage_domain_correctness(
    backend: Arc<dyn LlmBackend>,
    metrics: &MetricsStore,
    session_id: SessionId,
    stage_id: &str,
    narrative_text: &str,
    stage_description: &str,
) -> Result<DomainVerdict> {
    let user_prompt = format!(
        "STAGE_ID:\n{stage_id}\n\nSTAGE_DESCRIPTION:\n{stage_description}\n\nNARRATIVE:\n{narrative_text}\n\nEmit a single JSON DomainVerdict object. No prose around it.\n"
    );
    let model = for_domain_critic();
    let req = TurnRequest {
        system_prompt: vec![SystemPromptBlock {
            // The critic rubric is a static ~2KB prompt that never varies
            // between calls; opting into cache lets repeated verify passes
            // within the 5-minute TTL cache-read the prefix. The per-call
            // narrative stays in the uncached user turn so its uniqueness
            // can't invalidate the cacheable prefix.
            text: CRITIC_PROMPT.to_string(),
            cache: true,
        }],
        conversation: Arc::new(vec![Turn::user(user_prompt)]),
        tool_schemas: vec![],
        model,
        temperature: CRITIC_TEMPERATURE,
        max_tokens: CRITIC_MAX_OUTPUT_TOKENS,
        tool_exchange: vec![],
        tool_choice: None,
    };

    let resp = backend
        .send_turn(req)
        .await
        .context("domain critic LLM call failed")?;
    if resp.stop_reason != StopReason::EndTurn {
        return Err(anyhow!(
            "domain critic expected end_turn, got {:?}",
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

    parse_verdict(&resp.assistant_content)
        .with_context(|| format!("parsing domain critic output: {}", resp.assistant_content))
}

/// Parse the critic's text output as a `DomainVerdict`, tolerating a
/// markdown ```json fence. Confidence is clamped into `[0.0, 1.0]` so a
/// runaway value can't poison downstream display.
fn parse_verdict(raw: &str) -> Result<DomainVerdict> {
    let trimmed = raw.trim();
    let stripped = if let Some(rest) = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
    {
        rest.trim().trim_end_matches("```").trim()
    } else {
        trimmed
    };
    let mut v: DomainVerdict =
        serde_json::from_str(stripped).context("deserializing DomainVerdict")?;
    v.confidence = v.confidence.clamp(0.0, 1.0);
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::{TurnResponse, Usage};
    use crate::model_policy::ModelId;
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;

    struct StubBackend {
        captured: StdMutex<Vec<TurnRequest>>,
        canned: String,
    }
    impl StubBackend {
        fn new(canned: &str) -> Arc<Self> {
            Arc::new(Self {
                captured: StdMutex::new(Vec::new()),
                canned: canned.to_string(),
            })
        }
    }
    #[async_trait]
    impl LlmBackend for StubBackend {
        async fn send_turn(&self, req: TurnRequest) -> Result<TurnResponse> {
            self.captured.lock().unwrap().push(req); // lock-unwrap-allow: test
            Ok(TurnResponse {
                assistant_content: self.canned.clone(),
                tool_uses: Vec::new(),
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 100,
                    output_tokens: 40,
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

    #[tokio::test]
    async fn parses_plausible_verdict() {
        let canned = r#"{"verdict":"plausible","confidence":0.9,"failed_checks":[],"rationale":"AF spectrum is right-skewed as expected for mtDNA."}"#;
        let backend = StubBackend::new(canned);
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let v = verify_stage_domain_correctness(
            backend,
            &metrics,
            id,
            "variant_calling",
            "AF median 0.04",
            "Call mtDNA variants",
        )
        .await
        .unwrap();
        assert!(matches!(v.verdict, DomainVerdictKind::Plausible));
        assert!((v.confidence - 0.9).abs() < 1e-6);
    }

    #[tokio::test]
    async fn parses_implausible_major_with_failed_checks() {
        let canned = r#"```json
{"verdict":"implausible_major","confidence":0.8,"failed_checks":["af_spectrum_uniform"],"rationale":"Uniform AF distribution is biologically implausible for mtDNA heteroplasmy."}
```"#;
        let backend = StubBackend::new(canned);
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let v = verify_stage_domain_correctness(
            backend,
            &metrics,
            id,
            "variant_calling",
            "AF uniform",
            "Call mtDNA variants",
        )
        .await
        .unwrap();
        assert!(matches!(v.verdict, DomainVerdictKind::ImplausibleMajor));
        assert_eq!(v.failed_checks, vec!["af_spectrum_uniform".to_string()]);
    }

    #[tokio::test]
    async fn routes_through_domain_critic_model() {
        let backend = StubBackend::new(
            r#"{"verdict":"plausible","confidence":0.5,"failed_checks":[],"rationale":"ok"}"#,
        );
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let _ = verify_stage_domain_correctness(backend.clone(), &metrics, id, "s", "n", "d").await;
        let reqs = backend.captured.lock().unwrap(); // lock-unwrap-allow: test
        assert_eq!(reqs[0].model, ModelId::Opus48);
        assert_eq!(reqs[0].model, for_domain_critic());
    }

    #[tokio::test]
    async fn clamps_out_of_range_confidence() {
        // A runaway confidence (>1.0) is clamped so the sidecar never
        // surfaces an impossible value to the UI.
        let canned = r#"{"verdict":"plausible","confidence":4.2,"failed_checks":[],"rationale":"ok"}"#;
        let backend = StubBackend::new(canned);
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let v = verify_stage_domain_correctness(backend, &metrics, id, "s", "n", "d")
            .await
            .unwrap();
        assert!((v.confidence - 1.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn surfaces_parse_failure_for_malformed_output() {
        let backend = StubBackend::new("not json at all");
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let err = verify_stage_domain_correctness(backend, &metrics, id, "s", "n", "d")
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("parsing domain critic output"));
    }

    #[tokio::test]
    async fn records_usage_in_side_call_bucket() {
        // The advisory critic must bill into the side-call cost bucket so
        // its spend shows up distinctly in the Performance tab — never
        // into the chat bucket.
        let backend = StubBackend::new(
            r#"{"verdict":"plausible","confidence":0.5,"failed_checks":[],"rationale":"ok"}"#,
        );
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let _ = verify_stage_domain_correctness(backend, &metrics, id, "s", "n", "d")
            .await
            .unwrap();
        let snap = metrics.snapshot(id).await.unwrap();
        assert!(snap.side_call_cost_usd > 0.0, "side-call bucket empty");
        assert!(
            (snap.chat_cost_usd - 0.0).abs() < 1e-9,
            "chat bucket polluted by side-call: {}",
            snap.chat_cost_usd
        );
    }

    #[test]
    fn ws1_does_not_grow_tool_vocabulary() {
        // The domain_critic is a side-call, NOT a Tool. Adding it must
        // leave the closed tool vocabulary at 22.
        assert_eq!(crate::tools::Tool::COUNT, 22);
    }
}
