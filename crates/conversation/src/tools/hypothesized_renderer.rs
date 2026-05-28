//! `propose_hypothesized_renderer` tool body.
//!
//! The parallel to `propose_hypothesized_node` — the LLM calls this when
//! the SME
//! describes a preferred renderer after a figure resolved via
//! `StructuralFallback`. The handler validates the proposal (parent-term
//! resolvability, figure-id shadowing, namespace shape) and writes a
//! `DecisionType::ProposedHypothesizedRenderer` record on accept; rejections
//! (LLM-recoverable validation errors) do not write to the decision log.
//!
//! The handler never mutates the closed `PlotAffordanceRegistry`. Only the
//! affordance resolver + catalog-promotion gates consult the renderer-proposals
//! registry, and only after the required promotion evidence accumulates.

use crate::errors::{ToolError, ToolResult};
use crate::prompt::CONFIRMATION_CARD_TOTAL_MAX_CHARS;
use crate::session::Session;
use ecaa_workflow_core::decision_log::{DecisionActor, DecisionRecord, DecisionType};
use ecaa_workflow_core::plot_affordance::PlotAffordanceRegistry;

/// Reserved namespace prefixes that the SME's `target_semantic_type` must
/// not use as the local-extension namespace (reserved for catalog-controlled
/// terms). `swfc:` is the correct local namespace; `EDAM:` is the ontology
/// namespace.
const RESERVED_NAMESPACES: &[&str] = &["EDAM:"];

/// The local-extension namespace that *is* valid for renderer proposals.
const LOCAL_EXTENSION_NS: &str = "swfc:";

/// Hex-character length of the truncated UUID slice used to build a
/// `renderer-proposal-<hex>` id. 12 hex chars = 48 bits of entropy —
/// plenty for in-session uniqueness while keeping the id readable in
/// chat surfaces and decision-log records.
const PROPOSAL_ID_HEX_LEN: usize = 12;

pub(super) fn propose_hypothesized_renderer(
    session: &mut Session,
    registry: &dyn PlotAffordanceRegistry,
    target_semantic_type: &str,
    proposed_parent_terms: &[String],
    proposed_figure_ids: &[String],
    sme_intent: &str,
    primitive_basis: Option<&str>,
) -> ToolResult {
    // ── Schema validation: required-field non-emptiness ──
    if target_semantic_type.trim().is_empty() {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "target_semantic_type must be non-empty".into(),
            hint: "Use a SemanticType IRI (e.g. `swfc:my_custom_output`) that identifies the \
                   output port the renderer addresses."
                .into(),
        });
    }
    if proposed_parent_terms.is_empty() {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "proposed_parent_terms must contain at least one term".into(),
            hint: "Name at least one parent term surfaced in the affordance proof for this output \
                   port. The parent must be a registered renderer semantic type."
                .into(),
        });
    }
    if proposed_figure_ids.is_empty() {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "proposed_figure_ids must contain at least one figure id".into(),
            hint: "Name at least one figure id the preferred renderer would produce \
                   (e.g. `volcano`, `ridge_plot`)."
                .into(),
        });
    }
    if sme_intent.trim().is_empty() {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "sme_intent must be non-empty".into(),
            hint: format!(
                "Summarize the SME's description of the preferred renderer in ≤ {} chars.",
                CONFIRMATION_CARD_TOTAL_MAX_CHARS
            ),
        });
    }
    if sme_intent.chars().count() > CONFIRMATION_CARD_TOTAL_MAX_CHARS {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: format!(
                "sme_intent is {} chars; must be ≤ {}",
                sme_intent.chars().count(),
                CONFIRMATION_CARD_TOTAL_MAX_CHARS
            ),
            hint: format!(
                "Shorten sme_intent to ≤ {} chars.",
                CONFIRMATION_CARD_TOTAL_MAX_CHARS
            ),
        });
    }

    // ── Validation 1: target_semantic_type namespace ──
    // `EDAM:` is reserved for ontology-controlled terms.
    // `swfc:` is the correct local-extension namespace for renderer proposals.
    for reserved in RESERVED_NAMESPACES {
        if target_semantic_type.starts_with(reserved) {
            return ToolResult::err(ToolError::PreconditionFailure {
                reason: format!(
                    "target_semantic_type `{}` uses the reserved namespace `{}`",
                    target_semantic_type, reserved
                ),
                hint: format!(
                    "Use `{}` for local-extension semantic types \
                     (e.g. `swfc:my_custom_output`). `EDAM:` is reserved for \
                     ontology-controlled terms.",
                    LOCAL_EXTENSION_NS
                ),
            });
        }
    }

    // ── Validation 2: every proposed_parent_term resolves in the registry ──
    for term in proposed_parent_terms {
        if registry.lookup_exact(term).is_none() {
            return ToolResult::err(ToolError::PreconditionFailure {
                reason: format!(
                    "proposed_parent_term `{}` is not a registered renderer semantic type",
                    term
                ),
                hint: "Use only parent terms that appear in the affordance proof for this output \
                       port, or terms the SME explicitly named that are registered in the \
                       PlotAffordanceRegistry."
                    .into(),
            });
        }
    }

    // ── Validation 3: proposed_figure_ids do not shadow registered figure ids ──
    for fid in proposed_figure_ids {
        if registry.is_registered_figure_id(fid) {
            return ToolResult::err(ToolError::PreconditionFailure {
                reason: format!(
                    "figure_id `{}` is already registered in the catalog; \
                     cannot redefine without lifecycle promotion",
                    fid
                ),
                hint: "Propose a new, distinct figure id that doesn't conflict with any existing \
                       registered figure."
                    .into(),
            });
        }
    }

    // ── Idempotency: same (target_semantic_type, sorted figure_ids) in session ──
    let mut sorted_figure_ids = proposed_figure_ids.to_vec();
    sorted_figure_ids.sort();
    if let Some(existing_id) = session
        .renderer_proposals
        .find_duplicate(target_semantic_type, &sorted_figure_ids)
    {
        return ToolResult::ok(serde_json::json!({
            "outcome": "proposal_accepted",
            "proposal_id": existing_id,
            "duplicate": true,
            "note": "An identical renderer proposal already exists in this session; \
                     returning the same proposal_id.",
        }));
    }

    // ── Append to the session-scoped renderer-proposals registry ──
    let proposal_id = format!(
        "renderer-proposal-{}",
        &uuid::Uuid::new_v4().as_simple().to_string()[..PROPOSAL_ID_HEX_LEN]
    );
    session.renderer_proposals.push_proposal(
        proposal_id.clone(),
        target_semantic_type.to_string(),
        proposed_parent_terms.to_vec(),
        proposed_figure_ids.to_vec(),
        sme_intent.to_string(),
        primitive_basis.map(|s| s.to_string()),
    );

    // ── Audit: write the ProposedHypothesizedRenderer decision record ──
    session.decisions.push(DecisionRecord::new(
        session.id.to_string().as_str(),
        DecisionType::ProposedHypothesizedRenderer {
            proposal_id: proposal_id.clone(),
            target_semantic_type: target_semantic_type.to_string(),
            sme_intent: sme_intent.to_string(),
        },
        DecisionActor::Llm,
        Some(format!(
            "Renderer proposal: {}; parent_terms: {}; figure_ids: {}; primitive_basis: {}",
            target_semantic_type,
            proposed_parent_terms.join(", "),
            proposed_figure_ids.join(", "),
            primitive_basis.unwrap_or("none"),
        )),
    ));

    ToolResult::ok(serde_json::json!({
        "outcome": "proposal_accepted",
        "proposal_id": proposal_id,
        "lifecycle_state": "Hypothesized",
        "trust_level": "Unverified",
        "note": "The proposed renderer will not replace the structural fallback until \
                 catalog-promotion evidence accumulates: Phase 13 validators pass + \
                 Phase 14 sandbox approves + Phase 7 promotion authority records.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use ecaa_workflow_core::plot_affordance::{PlotAffordanceRegistry, RegisteredAffordance};

    /// Minimal in-memory registry for unit tests.
    struct MockRegistry {
        /// Registered semantic types (keyed by semantic_type IRI).
        affordances: std::collections::BTreeMap<String, RegisteredAffordance>,
    }

    impl MockRegistry {
        fn new() -> Self {
            Self {
                affordances: std::collections::BTreeMap::new(),
            }
        }

        fn with_affordance(mut self, semantic_type: &str, figure_ids: Vec<&str>) -> Self {
            self.affordances.insert(
                semantic_type.to_string(),
                RegisteredAffordance {
                    semantic_type: semantic_type.to_string(),
                    figure_ids: figure_ids.into_iter().map(|s| s.to_string()).collect(),
                    renderer_module: "test".to_string(),
                    theme_version: "1.0".to_string(),
                },
            );
            self
        }
    }

    impl PlotAffordanceRegistry for MockRegistry {
        fn lookup_exact(&self, semantic_type: &str) -> Option<&RegisteredAffordance> {
            self.affordances.get(semantic_type)
        }

        fn parents_of(&self, _semantic_type: &str) -> Vec<String> {
            vec![]
        }

        fn snapshot_id(&self) -> &str {
            "test-snapshot"
        }

        fn iter(&self) -> Box<dyn Iterator<Item = (&str, &RegisteredAffordance)> + '_> {
            Box::new(self.affordances.iter().map(|(k, v)| (k.as_str(), v)))
        }
    }

    fn fresh_session() -> Session {
        Session::new(false)
    }

    #[test]
    fn accepted_proposal_with_valid_parent_term() {
        let mut s = fresh_session();
        let reg = MockRegistry::new().with_affordance("EDAM:data_3134", vec!["scatter"]);
        let r = propose_hypothesized_renderer(
            &mut s,
            &reg,
            "swfc:my_custom_plot",
            &["EDAM:data_3134".to_string()],
            &["my_volcano".to_string()],
            "SME wants a volcano plot with custom axis labels",
            Some("__structural_matrix_overview"),
        );
        assert!(!r.is_error, "expected acceptance, got {:?}", r);
        // Decision log should have one entry.
        assert_eq!(s.decisions.len(), 1);
        assert!(matches!(
            &s.decisions[0].decision,
            DecisionType::ProposedHypothesizedRenderer { target_semantic_type, .. }
            if target_semantic_type == "swfc:my_custom_plot"
        ));
        // Renderer proposals registry should have one entry.
        assert_eq!(s.renderer_proposals.len(), 1);
    }

    #[test]
    fn rejected_proposal_with_unknown_parent_term() {
        let mut s = fresh_session();
        let reg = MockRegistry::new(); // empty — no registered affordances
        let r = propose_hypothesized_renderer(
            &mut s,
            &reg,
            "swfc:my_custom_plot",
            &["EDAM:data_3134".to_string()],
            &["my_volcano".to_string()],
            "SME wants a plot",
            None,
        );
        assert!(r.is_error, "expected rejection for unknown parent term");
        // No decision record written for rejections.
        assert!(s.decisions.is_empty());
    }

    #[test]
    fn rejected_proposal_with_shadowing_figure_id() {
        let mut s = fresh_session();
        // Registry has "volcano" as a registered figure id.
        let reg = MockRegistry::new().with_affordance("EDAM:data_3134", vec!["volcano"]);
        let r = propose_hypothesized_renderer(
            &mut s,
            &reg,
            "swfc:my_custom_plot",
            &["EDAM:data_3134".to_string()],
            &["volcano".to_string()], // shadows registered figure id
            "SME wants to redefine the volcano plot",
            None,
        );
        assert!(r.is_error, "expected rejection for shadowing figure id");
        assert!(s.decisions.is_empty());
    }

    #[test]
    fn repeat_proposal_of_same_target_returns_existing_proposal_id() {
        let mut s = fresh_session();
        let reg = MockRegistry::new().with_affordance("EDAM:data_3134", vec!["scatter"]);

        // First call: accepted, writes decision.
        let r1 = propose_hypothesized_renderer(
            &mut s,
            &reg,
            "swfc:my_custom_plot",
            &["EDAM:data_3134".to_string()],
            &["my_volcano".to_string()],
            "SME wants a volcano plot",
            None,
        );
        assert!(!r1.is_error);
        let id1 = r1.content["proposal_id"].as_str().unwrap().to_string();
        let decision_count_after_first = s.decisions.len();

        // Second identical call: should return the same proposal id, no new decision.
        let r2 = propose_hypothesized_renderer(
            &mut s,
            &reg,
            "swfc:my_custom_plot",
            &["EDAM:data_3134".to_string()],
            &["my_volcano".to_string()],
            "SME wants a volcano plot",
            None,
        );
        assert!(!r2.is_error);
        let id2 = r2.content["proposal_id"].as_str().unwrap().to_string();

        assert_eq!(
            id1, id2,
            "duplicate proposal should return the same proposal_id"
        );
        assert_eq!(
            s.decisions.len(),
            decision_count_after_first,
            "no new decision record should be written for a duplicate"
        );
        // Response should carry duplicate: true.
        assert_eq!(
            r2.content["duplicate"],
            serde_json::Value::Bool(true),
            "duplicate response should set duplicate: true"
        );
    }
}
