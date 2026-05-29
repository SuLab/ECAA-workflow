//! One-shot LLM drafter for sandboxed renderer modules.
//!
//! Input: a `RendererDraftRequest` carrying the accepted renderer
//! proposal, the registry snapshot id, and the theme digest. Output:
//! `DraftedRenderer { module_source, figure_ids, lints }`.
//!
//! Routed through `ModelPolicy::for_remediation_proposer()` (Opus 4.7)
//! to match the reasoning quality of the remediation proposer — the same
//! model that handles structured-blocker resolution. One-shot structured-
//! JSON output; the response is parsed directly as `DraftedRenderer`.
//! No tool-use scheme — that would require adding to the closed tool
//! vocabulary, which is reserved for state-mutating actions.
//!
//! Cost is billed via `record_side_call_usage` so the Performance tab
//! can show drafter spend separately from chat / agent / scorer.
//!
//! This side-call drafts the Python module source but does NOT write it
//! to disk. The harness sandbox places the module and runs it.
//! This module ships the drafter + static check entry point + validator
//! obligations; runtime sandbox execution is deferred.

use crate::anthropic::{LlmBackend, StopReason, TurnRequest};
use crate::metrics::MetricsStore;
use crate::model_policy::ModelPolicy;
use crate::prompt::SystemPromptBlock;
use crate::session::{SessionId, Turn};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::session::state::RendererProposal;

const DRAFTER_PROMPT: &str = include_str!("renderer_drafter_prompt.txt");

/// `max_tokens` for the drafter call. A Python module with 3–5 figure
/// functions fits comfortably in 4000 tokens; the 8192 cap gives room
/// for a richly-commented module without truncating.
const DRAFTER_MAX_OUTPUT_TOKENS: u32 = 8192;

/// Temperature is 0 for deterministic output — the same proposal should
/// produce the same module source on retry (byte-stable YAML promotion).
const DRAFTER_TEMPERATURE: f32 = 0.0;

/// Input to the renderer drafter side-call.
#[derive(Debug, Clone)]
pub struct RendererDraftRequest {
    /// The accepted renderer proposal from the session-scoped registry.
    pub proposal: RendererProposal,
    /// Registry snapshot id the proposal was resolved against. Threaded
    /// into the user prompt so the LLM can include it in the module's
    /// provenance comment.
    pub registry_snapshot_id: String,
    /// Theme digest (`sha256:...` of `theme.json`). Threaded into the
    /// prompt so the module can carry the pinned digest for reproducibility.
    pub theme_digest: String,
}

/// Successful output from the renderer drafter.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DraftedRenderer {
    /// Full Python module source. The harness places this at
    /// `lib/plotting/stages/_generated/<stage_id>.py` after the static
    /// check and SME approval gates pass. The drafter does NOT write it.
    pub module_source: String,
    /// Figure function names implemented in `module_source`. Must match
    /// `proposal.proposed_figure_ids` after set-equality check.
    pub figure_ids: Vec<String>,
    /// Advisory lint messages. May be empty. Non-blocking; surfaced in
    /// the UI's affordance draft card.
    pub lints: Vec<String>,
}

/// Why the drafter side-call failed.
#[derive(Debug)]
pub enum DraftError {
    /// The LLM response could not be parsed as `DraftedRenderer`.
    ParseError {
        /// Raw LLM response text that failed to parse.
        raw: String,
        /// Description of the parse failure.
        cause: String,
    },
    /// The LLM returned a stop reason other than `end_turn`.
    UnexpectedStopReason {
        /// The unexpected stop reason string.
        reason: String,
    },
    /// The drafted `figure_ids` do not match the proposal's
    /// `proposed_figure_ids` (set-inequality). Surfaced separately so
    /// the caller can re-invoke with a corrected prompt rather than
    /// treating it as a hard failure.
    FigureIdMismatch {
        /// Figure ids from the original proposal.
        proposed: Vec<String>,
        /// Figure ids produced by the LLM draft.
        drafted: Vec<String>,
    },
    /// Transport-level error from the LLM backend.
    Transport(anyhow::Error),
}

impl std::fmt::Display for DraftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DraftError::ParseError { raw, cause } => {
                write!(f, "renderer drafter parse error: {cause} (raw: {raw:.200})")
            }
            DraftError::UnexpectedStopReason { reason } => {
                write!(f, "renderer drafter expected end_turn, got {reason}")
            }
            DraftError::FigureIdMismatch { proposed, drafted } => {
                write!(
                    f,
                    "renderer drafter figure_ids mismatch: proposed={:?} drafted={:?}",
                    proposed, drafted
                )
            }
            DraftError::Transport(e) => write!(f, "renderer drafter transport error: {e}"),
        }
    }
}

/// Ask Opus 4.7 (via `ModelPolicy::for_remediation_proposer()`) to draft
/// a Python renderer module for the given proposal.
///
/// # Errors
///
/// - `DraftError::Transport` — LLM backend call failed.
/// - `DraftError::UnexpectedStopReason` — LLM returned something other
///   than `end_turn`.
/// - `DraftError::ParseError` — response could not be parsed as JSON.
/// - `DraftError::FigureIdMismatch` — drafted `figure_ids` differ from
///   the proposal's `proposed_figure_ids` (set inequality). The caller
///   should surface this as a soft failure and may retry.
pub async fn draft_renderer(
    backend: Arc<dyn LlmBackend>,
    metrics: &MetricsStore,
    session_id: SessionId,
    req: &RendererDraftRequest,
) -> std::result::Result<DraftedRenderer, DraftError> {
    let user_prompt = render_user_prompt(req);
    let model = ModelPolicy::for_remediation_proposer();
    let turn_req = TurnRequest {
        system_prompt: vec![SystemPromptBlock {
            text: DRAFTER_PROMPT.to_string(),
            cache: false,
        }],
        conversation: Arc::new(vec![Turn::user(user_prompt)]),
        tool_schemas: vec![],
        model,
        temperature: DRAFTER_TEMPERATURE,
        max_tokens: DRAFTER_MAX_OUTPUT_TOKENS,
        tool_exchange: vec![],
        tool_choice: None,
    };

    let resp = backend
        .send_turn(turn_req)
        .await
        .map_err(DraftError::Transport)?;

    if resp.stop_reason != StopReason::EndTurn {
        return Err(DraftError::UnexpectedStopReason {
            reason: format!("{:?}", resp.stop_reason),
        });
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

    let drafted = parse_drafted_renderer(&resp.assistant_content).map_err(|(raw, cause)| {
        DraftError::ParseError {
            raw: raw.to_string(),
            cause,
        }
    })?;

    // Validate that the drafted figure_ids match the proposal's
    // proposed_figure_ids (set equality; order is not mandated).
    let mut proposed_sorted = req.proposal.proposed_figure_ids.clone();
    proposed_sorted.sort();
    let mut drafted_sorted = drafted.figure_ids.clone();
    drafted_sorted.sort();
    if proposed_sorted != drafted_sorted {
        return Err(DraftError::FigureIdMismatch {
            proposed: proposed_sorted,
            drafted: drafted_sorted,
        });
    }

    Ok(drafted)
}

/// Render the user half of the drafter call.
fn render_user_prompt(req: &RendererDraftRequest) -> String {
    let p = &req.proposal;
    let mut out = String::new();
    out.push_str("PROPOSAL:\n");
    out.push_str(&format!("  proposal_id: {}\n", p.id));
    out.push_str(&format!(
        "  target_semantic_type: {}\n",
        p.target_semantic_type
    ));
    out.push_str(&format!(
        "  proposed_figure_ids: {:?}\n",
        p.proposed_figure_ids
    ));
    out.push_str(&format!(
        "  proposed_parent_terms: {:?}\n",
        p.proposed_parent_terms
    ));
    out.push_str(&format!("  sme_intent: {}\n", p.sme_intent));
    if let Some(pb) = &p.primitive_basis {
        out.push_str(&format!("  primitive_basis: {}\n", pb));
    }
    out.push_str(&format!(
        "  registry_snapshot_id: {}\n",
        req.registry_snapshot_id
    ));
    out.push_str(&format!("  theme_digest: {}\n", req.theme_digest));
    out.push('\n');

    // Stage id is derived from the semantic type's local part (the part
    // after the last `/` or `:`). Used in the module docstring and
    // provenance_kind labels.
    let stage_id = derive_stage_id(&p.target_semantic_type);
    out.push_str(&format!("STAGE_ID: {}\n\n", stage_id));

    out.push_str(
        "Draft the Python renderer module for this proposal. \
        Return ONLY the JSON object described in the system prompt. \
        No prose.\n",
    );
    out
}

/// Derive a filesystem-safe stage id from a semantic type IRI.
///
/// `ecaax:my_custom_output` → `my_custom_output`
/// `http://edamontology.org/data_1234` → `data_1234`
/// `plain_stage` → `plain_stage`
fn derive_stage_id(semantic_type: &str) -> String {
    // Take the local name after the last `:` or `/`.
    semantic_type
        .rsplit_once([':', '/'])
        .map(|(_, local)| local)
        .unwrap_or(semantic_type)
        .to_lowercase()
        .replace(|c: char| !c.is_alphanumeric() && c != '_', "_")
}

/// Parse the LLM's text output as a `DraftedRenderer`.
///
/// Tolerates:
/// * Bare JSON object: `{... }`
/// * Markdown-fenced block: ` ```json\n{... }\n``` `
fn parse_drafted_renderer(raw: &str) -> std::result::Result<DraftedRenderer, (String, String)> {
    let trimmed = raw.trim();
    let stripped = strip_fence(trimmed);
    serde_json::from_str::<DraftedRenderer>(stripped).map_err(|e| {
        (
            stripped.chars().take(500).collect::<String>(),
            e.to_string(),
        )
    })
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
    use chrono::Utc;
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

        fn with_stop(canned: &str, stop: StopReason) -> Arc<Self> {
            Arc::new(Self {
                captured: StdMutex::new(Vec::new()),
                canned: canned.to_string(),
                stop,
            })
        }
    }

    #[async_trait]
    impl LlmBackend for StubBackend {
        async fn send_turn(&self, req: TurnRequest) -> anyhow::Result<TurnResponse> {
            self.captured.lock().unwrap().push(req);
            Ok(TurnResponse {
                assistant_content: self.canned.clone(),
                tool_uses: Vec::new(),
                stop_reason: self.stop,
                usage: Usage {
                    input_tokens: 500,
                    output_tokens: 200,
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
        ) -> anyhow::Result<TurnResponse> {
            self.send_turn(req).await
        }
    }

    fn sample_proposal(figure_ids: Vec<&str>) -> RendererProposal {
        RendererProposal {
            id: "renderer-proposal-abc123def456".into(),
            target_semantic_type: "ecaax:custom_volcano".into(),
            proposed_parent_terms: vec!["data:1049".into()],
            proposed_figure_ids: figure_ids.into_iter().map(String::from).collect(),
            sme_intent: "Volcano plot with highlighted genes".into(),
            primitive_basis: Some("__structural_matrix_overview".into()),
            created_at: Utc::now(),
        }
    }

    fn sample_request(figure_ids: Vec<&str>) -> RendererDraftRequest {
        RendererDraftRequest {
            proposal: sample_proposal(figure_ids),
            registry_snapshot_id: "snap-2026-05-08-a".into(),
            theme_digest: "sha256:abc123".into(),
        }
    }

    fn well_formed_response(figure_ids: &[&str]) -> String {
        let ids_json = figure_ids
            .iter()
            .map(|id| format!("\"{}\"", id))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            r#"{{
  "module_source": "import numpy as np\nimport matplotlib.pyplot as plt\nimport runtime.plotting.core as _core\n\ndef volcano(data, *, png_path, pdf_path, title: str, theme_path: str):\n    fig, ax = _core.figure(theme_path=theme_path)\n    ax.set_title(title)\n    _core.savefig(fig, png_path=png_path, pdf_path=pdf_path, provenance_kind=\"custom_volcano_volcano\")\n",
  "figure_ids": [{ids_json}],
  "lints": []
}}"#
        )
    }

    #[tokio::test]
    async fn parses_well_formed_json_response() {
        let canned = well_formed_response(&["volcano"]);
        let backend = StubBackend::new(&canned);
        let metrics = crate::metrics::MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let req = sample_request(vec!["volcano"]);
        let result = draft_renderer(backend, &metrics, id, &req).await.unwrap();
        assert_eq!(result.figure_ids, vec!["volcano"]);
        assert!(result.module_source.contains("_core.savefig"));
    }

    #[tokio::test]
    async fn parses_markdown_fenced_json_response() {
        let inner = well_formed_response(&["volcano"]);
        let canned = format!("```json\n{}\n```", inner);
        let backend = StubBackend::new(&canned);
        let metrics = crate::metrics::MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let req = sample_request(vec!["volcano"]);
        let result = draft_renderer(backend, &metrics, id, &req).await.unwrap();
        assert_eq!(result.figure_ids, vec!["volcano"]);
    }

    #[tokio::test]
    async fn routes_through_remediation_proposer_model() {
        let canned = well_formed_response(&["volcano"]);
        let backend = StubBackend::new(&canned);
        let metrics = crate::metrics::MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let req = sample_request(vec!["volcano"]);
        let _ = draft_renderer(backend.clone(), &metrics, id, &req).await;
        let reqs = backend.captured.lock().unwrap();
        assert_eq!(reqs[0].model, ModelId::Opus47);
        assert_eq!(reqs[0].model, ModelPolicy::for_remediation_proposer());
    }

    #[tokio::test]
    async fn bills_into_side_call_bucket() {
        let canned = well_formed_response(&["volcano"]);
        let backend = StubBackend::new(&canned);
        let metrics = crate::metrics::MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let req = sample_request(vec!["volcano"]);
        let _ = draft_renderer(backend, &metrics, id, &req).await;
        let snap = metrics.snapshot(id).await.unwrap();
        assert!(snap.side_call_cost_usd > 0.0, "side-call bucket empty");
        assert!(
            (snap.chat_cost_usd - 0.0).abs() < 1e-9,
            "chat bucket polluted: {}",
            snap.chat_cost_usd
        );
    }

    #[tokio::test]
    async fn errors_on_figure_id_mismatch() {
        // Proposal says ["volcano"] but drafter returns ["ma_plot"]
        let canned = well_formed_response(&["ma_plot"]);
        let backend = StubBackend::new(&canned);
        let metrics = crate::metrics::MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let req = sample_request(vec!["volcano"]);
        let err = draft_renderer(backend, &metrics, id, &req)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DraftError::FigureIdMismatch { .. }),
            "expected FigureIdMismatch, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn errors_on_unexpected_stop_reason() {
        use crate::anthropic::StopReason;
        let canned = well_formed_response(&["volcano"]);
        let backend = StubBackend::with_stop(&canned, StopReason::MaxTokens);
        let metrics = crate::metrics::MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let req = sample_request(vec!["volcano"]);
        let err = draft_renderer(backend, &metrics, id, &req)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DraftError::UnexpectedStopReason { .. }),
            "expected UnexpectedStopReason, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn errors_on_malformed_json() {
        let backend = StubBackend::new("not json at all");
        let metrics = crate::metrics::MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let req = sample_request(vec!["volcano"]);
        let err = draft_renderer(backend, &metrics, id, &req)
            .await
            .unwrap_err();
        assert!(
            matches!(err, DraftError::ParseError { .. }),
            "expected ParseError, got: {}",
            err
        );
    }

    #[test]
    fn derive_stage_id_from_ecaax_iri() {
        assert_eq!(derive_stage_id("ecaax:custom_volcano"), "custom_volcano");
    }

    #[test]
    fn derive_stage_id_from_edam_url() {
        assert_eq!(
            derive_stage_id("http://edamontology.org/data_1234"),
            "data_1234"
        );
    }

    #[test]
    fn derive_stage_id_plain() {
        assert_eq!(derive_stage_id("plain_stage"), "plain_stage");
    }

    #[test]
    fn drafted_renderer_round_trips_serde() {
        let d = DraftedRenderer {
            module_source: "import numpy as np\n".into(),
            figure_ids: vec!["volcano".into()],
            lints: vec!["advisory note".into()],
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: DraftedRenderer = serde_json::from_str(&json).unwrap();
        assert_eq!(back.figure_ids, d.figure_ids);
        assert_eq!(back.lints, d.lints);
    }
}
