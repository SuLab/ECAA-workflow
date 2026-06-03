//! One-shot LLM drafter for catalog atoms — the fourth side-call,
//! mirroring `renderer_drafter.rs`.
//!
//! Input: an `AtomDraftRequest` carrying a `HypothesizedProposal` plus the
//! catalog snapshot id. Output: a candidate `AtomDefinition` that has been
//! routed through `AtomRegistry::validate_candidate` — the SAME schema +
//! deserialize gate a hand-authored YAML atom hits in `load_from_dir`.
//!
//! Routed through `ModelPolicy::for_remediation_proposer()` (Opus 4.8) to
//! match the reasoning quality of the renderer drafter. One-shot
//! structured-JSON output, temperature 0 (byte-stable promotion).
//!
//! Method neutrality is enforced by the prompt AND by a post-parse guard
//! (`assignee == Agent`, `method_choice == None`); a drafted atom that
//! sets a method is rejected as `DraftError::MethodChoiceSet`.
//!
//! Cost is billed via `record_side_call_usage` so the Performance tab can
//! show drafter spend in `side_call_cost_usd`. The side-call output is
//! excluded from the byte-diff baseline; the PROMOTED atom is deterministic
//! from the proposal + drafted JSON.
//!
//! This module drafts; it does NOT write to `config/` or auto-execute.

use crate::anthropic::{LlmBackend, StopReason, TurnRequest};
use crate::metrics::MetricsStore;
use crate::model_policy::ModelPolicy;
use crate::prompt::SystemPromptBlock;
use crate::session::{SessionId, Turn};
use ecaa_workflow_core::atom::{AtomAssignee, AtomDefinition};
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::hypothesized_proposal::HypothesizedProposal;
use std::sync::Arc;

const DRAFTER_PROMPT: &str = include_str!("atom_drafter_prompt.txt");

/// `max_tokens` for the drafter call. A single atom definition fits well
/// inside 4096; 8192 leaves headroom for a richly-described atom.
const DRAFTER_MAX_OUTPUT_TOKENS: u32 = 8192;

/// Temperature 0 — the same proposal must produce the same atom on retry.
const DRAFTER_TEMPERATURE: f32 = 0.0;

/// Input to the atom drafter side-call.
#[derive(Debug, Clone)]
pub struct AtomDraftRequest {
    /// The proposal whose intent the atom fulfills.
    pub proposal: HypothesizedProposal,
    /// Catalog snapshot id, threaded into the prompt for provenance.
    pub catalog_snapshot_id: String,
}

/// Why the atom drafter side-call failed.
#[derive(Debug)]
pub enum DraftError {
    /// LLM response could not be parsed as JSON.
    ParseError {
        /// Raw (truncated) LLM response text that failed to parse.
        raw: String,
        /// Description of the parse failure.
        cause: String,
    },
    /// LLM returned a stop reason other than `end_turn`.
    UnexpectedStopReason {
        /// The unexpected stop reason string.
        reason: String,
    },
    /// Drafted JSON failed `AtomRegistry::validate_candidate` — the same
    /// schema gate a hand-authored YAML atom hits. `failures` is the
    /// validator error text (identical surface to a failed loader).
    ValidatorFailed {
        /// Validator error text.
        failures: String,
    },
    /// Drafted atom's `id` does not equal the proposal's `node_id`.
    IdMismatch {
        /// Proposal node id (the authoritative id).
        node_id: String,
        /// Id the drafter produced.
        drafted: String,
    },
    /// Method neutrality violated: `assignee != agent` or
    /// `method_choice` is set. The drafter MUST stay method-neutral.
    MethodChoiceSet {
        /// What was violated.
        detail: String,
    },
    /// Transport-level error from the LLM backend.
    Transport(anyhow::Error),
}

impl std::fmt::Display for DraftError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DraftError::ParseError { raw, cause } => {
                write!(f, "atom drafter parse error: {cause} (raw: {raw:.200})")
            }
            DraftError::UnexpectedStopReason { reason } => {
                write!(f, "atom drafter expected end_turn, got {reason}")
            }
            DraftError::ValidatorFailed { failures } => {
                write!(f, "atom drafter candidate failed validator: {failures}")
            }
            DraftError::IdMismatch { node_id, drafted } => {
                write!(
                    f,
                    "atom drafter id mismatch: proposal node_id={node_id} drafted id={drafted}"
                )
            }
            DraftError::MethodChoiceSet { detail } => {
                write!(f, "atom drafter violated method neutrality: {detail}")
            }
            DraftError::Transport(e) => write!(f, "atom drafter transport error: {e}"),
        }
    }
}

/// Ask Opus 4.8 to draft a conformant atom for a proposal, validate it
/// through `AtomRegistry::validate_candidate`, and return the
/// `AtomDefinition` on success.
///
/// # Errors
/// See [`DraftError`]. A schema failure surfaces as `ValidatorFailed`
/// with the loader's error text so the caller can fall back to the
/// minimal overlay — identical surface to a hand-authored atom that
/// fails the loader.
pub async fn draft_atom(
    backend: Arc<dyn LlmBackend>,
    metrics: &MetricsStore,
    session_id: SessionId,
    req: &AtomDraftRequest,
) -> std::result::Result<AtomDefinition, DraftError> {
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

    let value = parse_json_object(&resp.assistant_content)
        .map_err(|(raw, cause)| DraftError::ParseError { raw, cause })?;

    // Route through the SAME validator a YAML atom hits.
    let atom =
        AtomRegistry::validate_candidate(&value).map_err(|e| DraftError::ValidatorFailed {
            failures: e.to_string(),
        })?;

    if atom.id != req.proposal.node_id {
        return Err(DraftError::IdMismatch {
            node_id: req.proposal.node_id.clone(),
            drafted: atom.id.clone(),
        });
    }
    // Method-neutrality guard (belt-and-braces beyond the prompt).
    if atom.assignee != AtomAssignee::Agent {
        return Err(DraftError::MethodChoiceSet {
            detail: "assignee must be agent (runtime method selection is deferred)".into(),
        });
    }
    if atom.method_choice.is_some() {
        return Err(DraftError::MethodChoiceSet {
            detail: "method_choice must be unset; method selection is delegated to the agent"
                .into(),
        });
    }

    Ok(atom)
}

fn render_user_prompt(req: &AtomDraftRequest) -> String {
    let p = &req.proposal;
    let mut out = String::new();
    out.push_str("PROPOSAL:\n");
    out.push_str(&format!("  node_id: {}\n", p.node_id));
    out.push_str(&format!("  intent: {}\n", p.intent));
    out.push_str(&format!("  rationale: {}\n", p.llm_rationale));
    out.push_str(&format!("  parent_terms: {:?}\n", p.parent_terms));
    out.push_str(&format!("  upstream_atom_ids: {:?}\n", p.upstream_atom_ids));
    out.push_str(&format!(
        "  catalog_snapshot_id: {}\n\n",
        req.catalog_snapshot_id
    ));
    out.push_str(
        "Draft the atom definition for this proposal. Return ONLY the JSON \
         object described in the system prompt.\n",
    );
    out
}

/// Parse the LLM text as a JSON object. Tolerates a markdown fence.
fn parse_json_object(raw: &str) -> std::result::Result<serde_json::Value, (String, String)> {
    let stripped = strip_fence(raw.trim());
    serde_json::from_str::<serde_json::Value>(stripped)
        .map_err(|e| (stripped.chars().take(500).collect::<String>(), e.to_string()))
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
            self.captured.lock().unwrap().push(req); // lock-unwrap-allow: test
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

    fn proposal(node_id: &str) -> HypothesizedProposal {
        HypothesizedProposal::new(
            node_id,
            "Score per-cell doublet probability",
            vec!["operation:0292".into()],
            "rationale",
            vec![],
            vec![],
            vec![],
            vec![],
        )
    }
    fn request(node_id: &str) -> AtomDraftRequest {
        AtomDraftRequest {
            proposal: proposal(node_id),
            catalog_snapshot_id: "snap-2026-06-02-a".into(),
        }
    }
    fn well_formed_atom(id: &str) -> String {
        format!(
            r#"{{
  "id": "{id}",
  "version": "0.1.0",
  "role": "operation",
  "description": "Score per-cell doublet probability.",
  "edam_operation": "operation:0292",
  "assignee": "agent"
}}"#
        )
    }

    #[tokio::test]
    async fn parses_and_validates_well_formed_atom() {
        let backend = StubBackend::new(&well_formed_atom("doublet_score"));
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let atom = draft_atom(backend, &metrics, id, &request("doublet_score"))
            .await
            .unwrap();
        assert_eq!(atom.id, "doublet_score");
        assert_eq!(atom.assignee, AtomAssignee::Agent);
        assert!(atom.method_choice.is_none());
    }

    #[tokio::test]
    async fn parses_markdown_fenced_atom() {
        let inner = well_formed_atom("doublet_score");
        let backend = StubBackend::new(&format!("```json\n{inner}\n```"));
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let atom = draft_atom(backend, &metrics, id, &request("doublet_score"))
            .await
            .unwrap();
        assert_eq!(atom.id, "doublet_score");
    }

    #[tokio::test]
    async fn routes_through_remediation_proposer_model() {
        let backend = StubBackend::new(&well_formed_atom("doublet_score"));
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let _ = draft_atom(backend.clone(), &metrics, id, &request("doublet_score")).await;
        let reqs = backend.captured.lock().unwrap(); // lock-unwrap-allow: test
        assert_eq!(reqs[0].model, ModelId::Opus48);
        assert_eq!(reqs[0].model, ModelPolicy::for_remediation_proposer());
    }

    #[tokio::test]
    async fn bills_into_side_call_bucket() {
        let backend = StubBackend::new(&well_formed_atom("doublet_score"));
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let _ = draft_atom(backend, &metrics, id, &request("doublet_score")).await;
        let snap = metrics.snapshot(id).await.unwrap();
        assert!(snap.side_call_cost_usd > 0.0, "side-call bucket empty");
        assert!((snap.chat_cost_usd - 0.0).abs() < 1e-9, "chat bucket polluted");
    }

    #[tokio::test]
    async fn rejects_schema_invalid_candidate_via_same_validator() {
        // No `role` — the same gate a YAML atom hits → ValidatorFailed.
        let backend = StubBackend::new(
            r#"{"id":"doublet_score","version":"0.1.0","description":"x","edam_operation":"operation:0292","assignee":"agent"}"#,
        );
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let err = draft_atom(backend, &metrics, id, &request("doublet_score"))
            .await
            .unwrap_err();
        assert!(matches!(err, DraftError::ValidatorFailed { .. }), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_method_non_neutral_atom() {
        // assignee: sme violates method neutrality.
        let backend = StubBackend::new(
            r#"{"id":"doublet_score","version":"0.1.0","role":"operation","description":"x","edam_operation":"operation:0292","assignee":"sme"}"#,
        );
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let err = draft_atom(backend, &metrics, id, &request("doublet_score"))
            .await
            .unwrap_err();
        assert!(matches!(err, DraftError::MethodChoiceSet { .. }), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_id_mismatch() {
        // Schema-valid id (lowercase snake_case) but not the proposal's
        // node_id — must surface as IdMismatch, not ValidatorFailed.
        let backend = StubBackend::new(&well_formed_atom("wrong_id"));
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let err = draft_atom(backend, &metrics, id, &request("doublet_score"))
            .await
            .unwrap_err();
        assert!(matches!(err, DraftError::IdMismatch { .. }), "got: {err}");
    }

    #[tokio::test]
    async fn errors_on_unexpected_stop_reason() {
        let backend =
            StubBackend::with_stop(&well_formed_atom("doublet_score"), StopReason::MaxTokens);
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let err = draft_atom(backend, &metrics, id, &request("doublet_score"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, DraftError::UnexpectedStopReason { .. }),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn errors_on_malformed_json() {
        let backend = StubBackend::new("not json at all");
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let err = draft_atom(backend, &metrics, id, &request("doublet_score"))
            .await
            .unwrap_err();
        assert!(matches!(err, DraftError::ParseError { .. }), "got: {err}");
    }
}
