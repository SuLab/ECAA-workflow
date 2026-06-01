//! Pillar C (Phase 2) — branch-level proposal pipeline.
//!
//! When the multi-branch composer cannot satisfy a requested modality
//! from the catalog, it does NOT silently drop the strand: it pushes a
//! `GapReport { id: "unsatisfiable_modality:<m>", .. }` into
//! `MultiBranchComposition.unresolved`, and the planner surfaces those
//! as [`ComposeOutcome::PartialDag { dag, unresolved_gaps }`].
//!
//! Phase 1 (already shipped) leaves the gap as a dead end. Phase 2
//! (this module) upgrades each unsatisfiable-modality gap into a
//! [`HypothesizedProposal`] advanced through the existing
//! validator/sandbox gates via [`crate::proposal_gate::advance_proposal`]
//! — exactly as the LLM-facing `propose_hypothesized_node` tool does —
//! so the synthesized proposal reaches `AwaitingSignoff` (or `Blocked`,
//! under a strict sandbox policy) and awaits SME signoff in the same
//! UI panel as a manually proposed node.
//!
//! This module does NOT promote or lower the proposal: the existing
//! server `POST /proposal/:id/signoff` path promotes an SME-approved
//! proposal and splices it into the executable DAG. Pillar C only needs
//! to CREATE + advance the proposal from the gap. The helper is
//! side-effect-only on `session.proposals` (no DAG mutation, no SSE,
//! no decision-log writes — it runs inside `rebuild_dag`, which is
//! already on a deterministic recompute path).

use ecaa_workflow_core::hypothesized_proposal::HypothesizedProposal;
use ecaa_workflow_core::workflow_contracts::outcome::ComposeOutcome;

use crate::session::Session;

/// Prefix that marks an unsatisfiable-modality gap (mirrors
/// `composer_v4::multi_branch_synthesis::missing_modality_gap`).
const UNSATISFIABLE_MODALITY_PREFIX: &str = "unsatisfiable_modality:";

/// Derive the stable hypothesized-node id for an unsatisfiable
/// modality. Keyed on the modality so re-running `rebuild_dag` against
/// the same `PartialDag` does not fork a second proposal.
fn modality_node_id(modality: &str) -> String {
    format!("{modality}_pipeline")
}

/// Pillar C entry point. When `outcome` is a [`ComposeOutcome::PartialDag`],
/// turn every unsatisfiable-modality gap into a [`HypothesizedProposal`],
/// advance it through the eager gates, and insert it into
/// `session.proposals` so it awaits SME signoff instead of dying as a
/// dead gap.
///
/// Idempotent: a gap whose derived `node_id` already exists in
/// `session.proposals` is skipped, so calling this repeatedly (every
/// `rebuild_dag`) never duplicates a proposal.
///
/// No-op for any non-`PartialDag` outcome (and for `PartialDag`s whose
/// gaps are not unsatisfiable-modality gaps).
pub(super) fn surface_unsatisfiable_modality_proposals(
    session: &mut Session,
    outcome: &ComposeOutcome,
) {
    let gaps = match outcome {
        ComposeOutcome::PartialDag {
            unresolved_gaps, ..
        } => unresolved_gaps,
        _ => return,
    };

    // Parent terms for the synthesized proposal. The
    // `propose_hypothesized_node` tool requires ≥1 EDAM/ecaax IRI, but
    // the GATE runner (`advance_proposal`) does not validate parent
    // terms — it only reads `validation_tests` (validator gate) and the
    // session's `active_policy_bundle` (sandbox gate). So we seed
    // parent terms from the workflow intent's desired outputs when a
    // valid IRI is available, and otherwise leave them empty: an
    // empty-spec proposal still advances cleanly to `AwaitingSignoff`.
    let parent_terms = goal_parent_terms(session);

    // Collect the node_ids already present so the dedup check sees a
    // stable snapshot even as we insert (we only insert ids not in this
    // set, and never two gaps with the same modality in one pass).
    let mut existing_node_ids: std::collections::BTreeSet<String> = session
        .proposals
        .values()
        .map(|p| p.node_id.clone())
        .collect();

    for gap in gaps {
        let Some(modality) = gap.id.strip_prefix(UNSATISFIABLE_MODALITY_PREFIX) else {
            continue;
        };
        if modality.is_empty() {
            continue;
        }
        let node_id = modality_node_id(modality);
        // DEDUP: a prior `rebuild_dag` (or this same pass) already
        // surfaced this modality.
        if !existing_node_ids.insert(node_id.clone()) {
            continue;
        }
        if session.proposals.values().any(|p| p.node_id == node_id) {
            continue;
        }

        let rationale = if gap.suggestions.is_empty() {
            gap.statement.clone()
        } else {
            format!(
                "{}\nSuggestions: {}",
                gap.statement,
                gap.suggestions.join("; ")
            )
        };

        let mut proposal = HypothesizedProposal::new(
            node_id,
            gap.statement.clone(),
            parent_terms.clone(),
            rationale,
            Vec::new(), // assumptions
            Vec::new(), // failure_modes
            Vec::new(), // validation_tests (empty → validator gate vacuous-passes)
            Vec::new(), // upstream_atom_ids
        );

        // Advance through the eager gates exactly as
        // `propose_hypothesized_node` does. With no declared validation
        // tests the validator gate soft/vacuous-passes; with no active
        // sandbox policy the sandbox gate vacuously passes — so the
        // proposal lands at `AwaitingSignoff`. Under a strict policy
        // bundle the sandbox gate may land it at `Blocked` instead;
        // either way it is past `PendingValidation`.
        let _ = crate::proposal_gate::advance_proposal(&mut proposal, session);

        session.proposals.insert(proposal.id.clone(), proposal);
    }
}

/// Best-effort parent-term seeding from the session's workflow intent.
/// Returns the first valid EDAM/ecaax IRI found among the intent's
/// desired outputs, or an empty vec when none is available. An empty
/// `parent_terms` is acceptable to the gate runner (which never
/// validates parent terms), so the proposal still advances.
fn goal_parent_terms(session: &Session) -> Vec<String> {
    let Some(intent) = session.workflow_intent.as_ref() else {
        return Vec::new();
    };
    intent
        .desired_outputs
        .iter()
        .filter_map(|o| o.edam_data.as_deref())
        .find(|iri| ecaa_workflow_core::goal_spec::is_valid_edam_iri(iri))
        .map(|iri| vec![iri.to_string()])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::hypothesized_proposal::ProposalLifecycle;
    use ecaa_workflow_core::workflow_contracts::outcome::{GapReport, ValidationReport};
    use ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag;

    fn partial_dag_with_gap(modality: &str) -> ComposeOutcome {
        ComposeOutcome::PartialDag {
            dag: WorkflowDag::default(),
            unresolved_gaps: vec![GapReport {
                id: format!("unsatisfiable_modality:{modality}"),
                statement: format!(
                    "modality '{modality}' has no catalog satisfier; surface a \
                     hypothesized-node proposal instead of dropping it"
                ),
                missing_port: None,
                suggestions: vec![
                    format!("Propose a hypothesized pipeline for '{modality}'"),
                    "Add an archetype or atoms covering this modality".into(),
                ],
            }],
        }
    }

    #[test]
    fn partial_dag_gap_becomes_one_awaiting_signoff_proposal() {
        let mut s = Session::new(false);
        let outcome = partial_dag_with_gap("made_up_modality");
        surface_unsatisfiable_modality_proposals(&mut s, &outcome);

        // (a) Exactly one proposal, keyed `<modality>_pipeline`.
        assert_eq!(
            s.proposals.len(),
            1,
            "expected exactly one synthesized proposal"
        );
        let p = s.proposals.values().next().unwrap();
        assert_eq!(p.node_id, "made_up_modality_pipeline");

        // (b) Past PendingValidation. With empty validation_tests the
        // validator gate vacuously passes and (no active sandbox policy)
        // the sandbox gate vacuously passes → AwaitingSignoff.
        assert!(
            !matches!(p.lifecycle, ProposalLifecycle::PendingValidation),
            "proposal must have advanced past PendingValidation, got {:?}",
            p.lifecycle
        );
        assert!(
            matches!(p.lifecycle, ProposalLifecycle::AwaitingSignoff),
            "empty-spec proposal with no sandbox policy must land at AwaitingSignoff, got {:?}",
            p.lifecycle
        );
    }

    #[test]
    fn second_call_with_same_outcome_is_idempotent() {
        let mut s = Session::new(false);
        let outcome = partial_dag_with_gap("made_up_modality");
        surface_unsatisfiable_modality_proposals(&mut s, &outcome);
        assert_eq!(s.proposals.len(), 1);
        // (c) Idempotency: re-running rebuild_dag must not duplicate.
        surface_unsatisfiable_modality_proposals(&mut s, &outcome);
        assert_eq!(
            s.proposals.len(),
            1,
            "re-running against the same gap must not add a duplicate proposal"
        );
    }

    #[test]
    fn validated_executable_dag_adds_zero_proposals() {
        let mut s = Session::new(false);
        // Negative control: a fully-validated DAG carries no gaps.
        let outcome = ComposeOutcome::ValidatedExecutableDag {
            dag: WorkflowDag::default(),
            report: ValidationReport::default(),
        };
        surface_unsatisfiable_modality_proposals(&mut s, &outcome);
        assert!(
            s.proposals.is_empty(),
            "a validated DAG with no gaps must add zero proposals"
        );
    }

    #[test]
    fn non_modality_gaps_are_ignored() {
        let mut s = Session::new(false);
        // A PartialDag whose gap is NOT an unsatisfiable-modality gap
        // (e.g. a missing-port gap) must be left untouched.
        let outcome = ComposeOutcome::PartialDag {
            dag: WorkflowDag::default(),
            unresolved_gaps: vec![GapReport {
                id: "missing_producer:data:0951".into(),
                statement: "no producer for differential-expression table".into(),
                missing_port: Some("de_table".into()),
                suggestions: vec![],
            }],
        };
        surface_unsatisfiable_modality_proposals(&mut s, &outcome);
        assert!(
            s.proposals.is_empty(),
            "non-modality gaps must not synthesize proposals"
        );
    }

    #[test]
    fn multiple_distinct_modalities_yield_distinct_proposals() {
        let mut s = Session::new(false);
        let outcome = ComposeOutcome::PartialDag {
            dag: WorkflowDag::default(),
            unresolved_gaps: vec![
                GapReport {
                    id: "unsatisfiable_modality:cytof".into(),
                    statement: "cytof has no satisfier".into(),
                    missing_port: None,
                    suggestions: vec![],
                },
                GapReport {
                    id: "unsatisfiable_modality:cryo_em".into(),
                    statement: "cryo_em has no satisfier".into(),
                    missing_port: None,
                    suggestions: vec![],
                },
            ],
        };
        surface_unsatisfiable_modality_proposals(&mut s, &outcome);
        assert_eq!(s.proposals.len(), 2);
        let ids: std::collections::BTreeSet<&str> =
            s.proposals.values().map(|p| p.node_id.as_str()).collect();
        assert!(ids.contains("cytof_pipeline"));
        assert!(ids.contains("cryo_em_pipeline"));
    }
}
