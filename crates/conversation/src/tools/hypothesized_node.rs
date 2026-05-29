//! `propose_hypothesized_node` tool body.
//!
//! The LLM calls this
//! when the SME asks for a capability the registry doesn't yet
//! provide. The handler validates the proposal (schema, name
//! uniqueness, parent term resolvability), inserts it into the
//! session-scoped `proposals` registry, and synchronously invokes
//! [`crate::proposal_gate::advance_proposal`] to run the validator
//! and sandbox gates against a transient
//! [`ecaa_workflow_core::workflow_contracts::task_node::TaskNode`]
//! synthesized from the proposal. A `DecisionType::ProposedHypothesizedNode`
//! audit record is appended in the same call so the long-term
//! decision log keeps its existing shape; rejections
//! (LLM-recoverable schema errors) write neither the proposal nor
//! the decision record.
//!
//! Gate outcomes are surfaced to the UI via `ServiceEventSink`
//! callbacks (`proposal_received` + one `proposal_gate_advanced` per
//! gate that fired). The handler never narrates future gate behavior
//! in the tool response — the chat-pane progress card renders the
//! deterministic lifecycle state instead.
//!
//! The handler never mutates the executable DAG. Materialization
//! (lifting `AwaitingSignoff` → `Promoted` and splicing a real
//! `TaskNode` into `workflow_dag`) is the server's
//! `POST /proposal/:id/signoff` endpoint, out of band from this tool.

use crate::errors::{ToolError, ToolResult};
use crate::session::Session;
use crate::tools::ToolContext;
use ecaa_workflow_core::decision_log::{DecisionActor, DecisionRecord, DecisionType};
use ecaa_workflow_core::hypothesized_proposal::HypothesizedProposal;

#[allow(clippy::too_many_arguments)]
pub(super) fn propose_hypothesized_node(
    session: &mut Session,
    proposed_id: &str,
    intent: &str,
    parent_terms: &[String],
    llm_rationale: &str,
    assumptions: &[String],
    failure_modes: &[String],
    validation_tests: &[String],
    upstream_atom_ids: &[String],
    ctx: &ToolContext,
) -> ToolResult {
    // ── Schema validation: required-field non-emptiness ──
    if proposed_id.trim().is_empty() {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "proposed_id must be non-empty".into(),
            hint: "Use snake_case identifying the new capability (e.g. `doublet_score`).".into(),
        });
    }
    if intent.trim().is_empty() {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "intent must be non-empty".into(),
            hint: "One sentence describing what the proposed node does.".into(),
        });
    }
    if parent_terms.is_empty() {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "parent_terms must contain at least one ontology term".into(),
            hint: "Use a parent EDAM term (data:NNNN, format:NNNN, operation:NNNN, topic:NNNN, \
                   or ecaax:<slug>) so the compatibility engine can subsume."
                .into(),
        });
    }
    if llm_rationale.trim().is_empty() {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "llm_rationale must be non-empty".into(),
            hint: "Summarize the SME's free-text justification from prior turns.".into(),
        });
    }

    // ── Schema validation: id shape (snake_case, no whitespace) ──
    if !proposed_id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        || proposed_id.starts_with('_')
        || proposed_id.ends_with('_')
    {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: format!(
                "proposed_id `{}` must be snake_case lowercase with no leading/trailing \
                 underscore",
                proposed_id
            ),
            hint: "Use snake_case (e.g. `doublet_score`).".into(),
        });
    }

    // ── Schema validation: parent term IRI shape ──
    for term in parent_terms {
        if !ecaa_workflow_core::goal_spec::is_valid_edam_iri(term) {
            return ToolResult::err(ToolError::PreconditionFailure {
                reason: format!("parent_term `{}` is not a valid EDAM / ecaax IRI", term),
                hint: "Use `data:NNNN`, `format:NNNN`, `operation:NNNN`, `topic:NNNN`, or \
                       `ecaax:<slug>`."
                    .into(),
            });
        }
    }

    // ── Idempotency: a same-session repeat-proposal of an
    // identical proposed_id is a deterministic no-op. The LLM
    // gets the same proposal_id + lifecycle back so it can carry
    // on without forking the audit trail or re-running gates. ──
    //
    // Also catches token-permutation duplicates (`logcpm_pca_plot`
    // vs `pca_logcpm_plot`): the LLM occasionally re-proposes the
    // same logical atom under a reordered name after the first
    // proposal is promoted; without this guard the SME sees the
    // same proposal twice in the panel.
    let proposed_tokens: std::collections::BTreeSet<&str> =
        proposed_id.split('_').filter(|t| !t.is_empty()).collect();
    if let Some((existing_id, existing_proposal)) = session.proposals.iter().find(|(_, p)| {
        if p.node_id == proposed_id {
            return true;
        }
        let existing_tokens: std::collections::BTreeSet<&str> =
            p.node_id.split('_').filter(|t| !t.is_empty()).collect();
        // Require ≥3 tokens to avoid collapsing short generic names
        // (`qc_report` and `report_qc` both have 2 tokens but are not
        // necessarily the same atom). Real-world hypothesized atom
        // names regularly have 3+ tokens; the duplicate pattern we
        // observed (`logcpm_pca_plot` ↔ `pca_logcpm_plot`) is at 3+.
        existing_tokens.len() >= 3
            && existing_tokens == proposed_tokens
            && p.parent_terms.as_slice() == parent_terms
    }) {
        return ToolResult::ok(serde_json::json!({
            "outcome": "proposal_accepted",
            "proposal_id": existing_id.as_str(),
            "node_id": existing_proposal.node_id,
            "duplicate": true,
            "lifecycle": serde_json::to_value(&existing_proposal.lifecycle)
                .unwrap_or(serde_json::Value::Null),
        }));
    }

    // ── Build the proposal and advance it through the eager gates. ──
    // Snapshot gate_outcomes len BEFORE advance so we can compute
    // which gates fired (advance_proposal appends to the vec).
    let mut proposal = HypothesizedProposal::new(
        proposed_id.to_string(),
        intent.to_string(),
        parent_terms.to_vec(),
        llm_rationale.to_string(),
        assumptions.to_vec(),
        failure_modes.to_vec(),
        validation_tests.to_vec(),
        upstream_atom_ids.to_vec(),
    );
    let outcomes_before = proposal.gate_outcomes.len();
    // Borrow session immutably for the gate runner; advance_proposal
    // only reads session state (active_policy_bundle) and mutates the
    // proposal in place. The subsequent insert is what writes to the
    // mutable session.
    let _new_lifecycle = crate::proposal_gate::advance_proposal(&mut proposal, session);
    // Capture every gate the runner just appended so the dispatcher
    // can fire one `proposal_gate_advanced` SSE event per gate.
    let new_gate_outcomes: Vec<_> = proposal.gate_outcomes[outcomes_before..]
        .iter()
        .map(|o| (o.gate, o.passed))
        .collect();
    let proposal_id = proposal.id.clone();
    let proposal_node_id = proposal.node_id.clone();
    let lifecycle_value =
        serde_json::to_value(&proposal.lifecycle).unwrap_or(serde_json::Value::Null);

    // ── Insert into session.proposals. ──
    session
        .proposals
        .insert(proposal.id.clone(), proposal.clone());

    // ── Audit: append the ProposedHypothesizedNode decision record so
    // the long-term audit log shape stays identical to the pre-proposal-tool baseline.
    // The proposals registry carries the live lifecycle; the
    // decisions vec carries the immutable trail.
    //
    // The LLM-authored `llm_rationale` is no longer captured on the
    // canonical decision record. The narrative
    // still rides the LLM's conversation turn (the SME can read it
    // there); the audit log keeps only the structural fields so a
    // replayer cannot mistake LLM prose for SME intent. The legacy
    // field remains in the (now-`Option<String>`) shape so on-disk
    // records from earlier deployments still deserialize. ──
    session.decisions.push(DecisionRecord::new(
        session.id.to_string().as_str(),
        DecisionType::ProposedHypothesizedNode {
            node_id: proposed_id.to_string(),
            parent_terms: parent_terms.to_vec(),
            llm_rationale: None,
        },
        DecisionActor::Llm,
        Some(format!(
            "Proposal: {}; intent: {}; assumptions: {}; failure_modes: {}; tests: {}",
            proposed_id,
            intent,
            assumptions.join(", "),
            failure_modes.join(", "),
            validation_tests.join(", "),
        )),
    ));

    // ── SSE: one `proposal_received` + one `proposal_gate_advanced`
    // per gate that fired. Fire-and-forget; default-no-op sinks
    // (CLI / tests) drop the events on the floor. ──
    if let (Some(sink), Some(sid)) = (&ctx.event_sink, ctx.session_id) {
        sink.proposal_received(sid, &proposal_id, &proposal_node_id);
        for (gate, passed) in &new_gate_outcomes {
            sink.proposal_gate_advanced(sid, &proposal_id, *gate, *passed);
        }
    }

    ToolResult::ok(serde_json::json!({
        "outcome": "proposal_accepted",
        "proposal_id": proposal_id.as_str(),
        "node_id": proposal_node_id,
        "lifecycle": lifecycle_value,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;
    use crate::tools::ToolContext;
    use std::path::PathBuf;

    fn fresh_session() -> Session {
        Session::new(false)
    }

    fn tool_ctx() -> ToolContext {
        // The propose_hypothesized_node handler only reads
        // `ctx.event_sink` + `ctx.session_id`; both are None in tests
        // so the fire-and-forget SSE block is a no-op.
        ToolContext::new(PathBuf::from("/tmp"), "claude-sonnet-4-6")
    }

    #[test]
    fn rejects_empty_proposed_id() {
        let mut s = fresh_session();
        let ctx = tool_ctx();
        let r = propose_hypothesized_node(
            &mut s,
            "",
            "x",
            &["data:2603".into()],
            "x",
            &[],
            &[],
            &[],
            &[],
            &ctx,
        );
        assert!(r.is_error);
    }

    #[test]
    fn rejects_invalid_id_shape() {
        let mut s = fresh_session();
        let ctx = tool_ctx();
        let r = propose_hypothesized_node(
            &mut s,
            "BadID",
            "x",
            &["data:2603".into()],
            "x",
            &[],
            &[],
            &[],
            &[],
            &ctx,
        );
        assert!(r.is_error);
    }

    #[test]
    fn rejects_empty_parent_terms() {
        let mut s = fresh_session();
        let ctx = tool_ctx();
        let r = propose_hypothesized_node(&mut s, "x", "x", &[], "x", &[], &[], &[], &[], &ctx);
        assert!(r.is_error);
    }

    #[test]
    fn rejects_invalid_parent_iri() {
        let mut s = fresh_session();
        let ctx = tool_ctx();
        let r = propose_hypothesized_node(
            &mut s,
            "doublet_score",
            "intent",
            &["not-an-iri".into()],
            "rationale",
            &[],
            &[],
            &[],
            &[],
            &ctx,
        );
        assert!(r.is_error);
    }

    #[test]
    fn accepts_well_formed_proposal_and_writes_decision() {
        let mut s = fresh_session();
        let ctx = tool_ctx();
        assert!(s.decisions.is_empty());
        assert!(s.proposals.is_empty());
        let r = propose_hypothesized_node(
            &mut s,
            "doublet_score",
            "Score per-cell doublet probability",
            &["data:2603".into()],
            "SME asked for doublet probability output; no atom in registry produces this directly",
            &["scrublet defaults reasonable for this dataset".into()],
            &["doublet probability outside [0,1]".into()],
            &["p_value_in_unit_interval".into()],
            &[],
            &ctx,
        );
        assert!(!r.is_error, "expected acceptance, got {:?}", r);
        // Post-Phase-7.1: a single accept must populate BOTH the
        // proposals registry AND the decision-record audit trail.
        assert_eq!(
            s.proposals.len(),
            1,
            "proposal must land in session.proposals"
        );
        assert_eq!(
            s.decisions.len(),
            1,
            "audit decision record must be appended"
        );
        let rec = &s.decisions[0];
        assert!(matches!(
            &rec.decision,
            DecisionType::ProposedHypothesizedNode { node_id, .. } if node_id == "doublet_score"
        ));
        // The durable record must NOT carry the LLM-authored narrative.
        // New writes always set `llm_rationale` to `None`; the legacy
        // field stays on the struct purely for backward-compatible
        // deserialization of older on-disk records.
        if let DecisionType::ProposedHypothesizedNode { llm_rationale, .. } = &rec.decision {
            assert!(
                llm_rationale.is_none(),
                "durable decision must not record LLM-authored rationale; \
                 got Some({llm_rationale:?})"
            );
        }
        // The single proposal record must carry the same node_id.
        let proposal = s.proposals.values().next().unwrap();
        assert_eq!(proposal.node_id, "doublet_score");
    }

    #[test]
    fn duplicate_proposal_is_deterministic_noop() {
        let mut s = fresh_session();
        let ctx = tool_ctx();
        // First call: writes proposal + decision.
        let _ = propose_hypothesized_node(
            &mut s,
            "doublet_score",
            "intent",
            &["data:2603".into()],
            "rationale",
            &[],
            &[],
            &[],
            &[],
            &ctx,
        );
        let proposals_after_first = s.proposals.len();
        let decisions_after_first = s.decisions.len();
        // Second identical call: no new proposal, no new decision.
        let r = propose_hypothesized_node(
            &mut s,
            "doublet_score",
            "intent",
            &["data:2603".into()],
            "rationale",
            &[],
            &[],
            &[],
            &[],
            &ctx,
        );
        assert!(!r.is_error);
        assert_eq!(
            s.proposals.len(),
            proposals_after_first,
            "duplicate must not add a second proposal to the registry"
        );
        assert_eq!(s.proposals.len(), 1, "registry must still hold exactly one");
        assert_eq!(
            s.decisions.len(),
            decisions_after_first,
            "duplicate must not append a second decision record"
        );
        // The response carries `duplicate: true`.
        match &r.content {
            serde_json::Value::Object(map) => {
                assert_eq!(map.get("duplicate"), Some(&serde_json::Value::Bool(true)));
            }
            other => panic!("expected json object response, got {other:?}"),
        }
    }

    #[test]
    fn accepted_proposal_has_lifecycle_in_response() {
        let mut s = fresh_session();
        let ctx = tool_ctx();
        let r = propose_hypothesized_node(
            &mut s,
            "doublet_score",
            "intent",
            &["data:2603".into()],
            "rationale",
            &[],
            &[],
            &["p_value_in_unit_interval".into()],
            &[],
            &ctx,
        );
        assert!(!r.is_error);
        // The response must surface the current lifecycle so the
        // calling LLM can decide whether to relay the state to the
        // SME via short prose (truthful only, no narration of future
        // gate behavior — the card carries the live state).
        let lifecycle = r
            .content
            .get("lifecycle")
            .expect("response must carry lifecycle");
        let kind = lifecycle
            .get("kind")
            .expect("lifecycle must be tagged with kind");
        assert!(kind.is_string());
    }

    #[test]
    fn tool_response_does_not_advertise_future_gates() {
        let mut s = fresh_session();
        let ctx = tool_ctx();
        let r = propose_hypothesized_node(
            &mut s,
            "doublet_score",
            "intent",
            &["data:2603".into()],
            "rationale",
            &[],
            &[],
            &[],
            &[],
            &ctx,
        );
        assert!(!r.is_error);
        // The tool body must not claim that the validator / sandbox /
        // signoff gate pipeline is about to run or will succeed. The
        // serialized response must carry zero references to the
        // forbidden narration substrings — the deterministic card
        // renders gate state; the tool body MUST NOT.
        let serialized = serde_json::to_string(&r.content).unwrap();
        for forbidden in [
            "Phase 13",
            "Phase 14",
            "Phase 7",
            "promotion authority records",
            "sandbox approves",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "tool response leaks forbidden narration `{}`: {}",
                forbidden,
                serialized
            );
        }
    }
}
