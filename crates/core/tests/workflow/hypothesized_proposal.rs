//! Integration tests for [`ecaa_workflow_core::hypothesized_proposal`].
//!
//! Covers the transient / materialized synthesis helpers and the
//! `record_gate` mutation contract.

use ecaa_workflow_core::atom::AtomRole;
use ecaa_workflow_core::hypothesized_proposal::{
    promoted_proposal_to_atom_definition, proposal_to_materialized_task_node,
    proposal_to_transient_task_node, GateName, GateOutcome, HypothesizedProposal,
    ProposalBlockerReason, ProposalLifecycle,
};
use ecaa_workflow_core::workflow_contracts::implementation::Implementation;
use ecaa_workflow_core::workflow_contracts::lifecycle::{LifecycleState, PromotionAuthority};
use ecaa_workflow_core::workflow_contracts::semantic_type::SemanticType;

fn sample_proposal() -> HypothesizedProposal {
    HypothesizedProposal::new(
        "doublet_score",
        "Score per-cell doublet probability",
        vec!["data:2603".into()],
        "SME asked for doublet probability output; no atom in registry produces this directly",
        vec!["scrublet defaults reasonable for this dataset".into()],
        vec!["doublet probability outside [0,1]".into()],
        vec![
            "p_value_in_unit_interval".into(),
            "barcode_matrix_dim_consistency".into(),
        ],
        vec![],
    )
}

#[test]
fn transient_synthesis_sets_hypothesized_lifecycle() {
    let proposal = sample_proposal();
    let node = proposal_to_transient_task_node(&proposal);
    assert_eq!(node.id, "doublet_score");
    assert_eq!(node.machine_name, "doublet_score");
    assert!(
        matches!(node.lifecycle_state, LifecycleState::Hypothesized),
        "expected Hypothesized lifecycle, got {:?}",
        node.lifecycle_state
    );
}

#[test]
fn transient_synthesis_populates_validators_from_validation_tests() {
    let proposal = sample_proposal();
    let node = proposal_to_transient_task_node(&proposal);
    let validator_ids: Vec<&str> = node.validators.iter().map(|v| v.id.as_str()).collect();
    assert!(validator_ids.contains(&"p_value_in_unit_interval"));
    assert!(validator_ids.contains(&"barcode_matrix_dim_consistency"));
    assert_eq!(validator_ids.len(), 2);
}

#[test]
fn transient_synthesis_uses_unimplemented() {
    let proposal = sample_proposal();
    let node = proposal_to_transient_task_node(&proposal);
    assert!(
        matches!(node.implementation, Implementation::Unimplemented),
        "expected Implementation::Unimplemented, got {:?}",
        node.implementation
    );
}

#[test]
fn transient_synthesis_has_output_ports_for_each_parent_term() {
    // Single parent term → single `output` port.
    let proposal = sample_proposal();
    let node = proposal_to_transient_task_node(&proposal);
    assert_eq!(node.outputs.len(), 1);
    assert_eq!(node.outputs[0].name, "output");

    // Multiple parent terms → numbered output ports.
    let multi = HypothesizedProposal::new(
        "multi_out",
        "Produces two semantic outputs",
        vec!["data:2603".into(), "data:0863".into()],
        "rationale",
        vec![],
        vec![],
        vec![],
        vec![],
    );
    let multi_node = proposal_to_transient_task_node(&multi);
    assert_eq!(multi_node.outputs.len(), 2);
    assert_eq!(multi_node.outputs[0].name, "output_1");
    assert_eq!(multi_node.outputs[1].name, "output_2");
}

#[test]
fn transient_synthesis_records_provenance_source() {
    let proposal = sample_proposal();
    let node = proposal_to_transient_task_node(&proposal);
    let source = node.provenance.source.as_deref().unwrap_or("");
    assert!(
        source.starts_with("proposal:"),
        "expected `proposal:<id>` provenance source, got {source:?}",
    );
}

#[test]
fn materialized_synthesis_sets_contracted_lifecycle() {
    let proposal = sample_proposal();
    let authority = PromotionAuthority {
        kind: "sme_user".into(),
        id: "alan".into(),
        at: "2026-05-12T12:00:00Z".into(),
    };
    let node = proposal_to_materialized_task_node(&proposal, authority.clone());
    assert!(
        matches!(node.lifecycle_state, LifecycleState::Contracted),
        "expected Contracted lifecycle on materialized node, got {:?}",
        node.lifecycle_state
    );
    assert_eq!(
        node.provenance.promotion_history.len(),
        1,
        "expected one promotion-history entry after materialization"
    );
    assert_eq!(node.provenance.promotion_history[0], authority);
}

#[test]
fn materialized_synthesis_inherits_validators_and_ports() {
    // Materialized node must carry the same validators + ports as
    // the transient node so the planner sees the full contract.
    let proposal = sample_proposal();
    let authority = PromotionAuthority {
        kind: "sme_user".into(),
        id: "alan".into(),
        at: "2026-05-12T12:00:00Z".into(),
    };
    let transient = proposal_to_transient_task_node(&proposal);
    let materialized = proposal_to_materialized_task_node(&proposal, authority);
    assert_eq!(transient.validators, materialized.validators);
    assert_eq!(transient.outputs, materialized.outputs);
    assert_eq!(transient.id, materialized.id);
    assert_eq!(transient.intent, materialized.intent);
}

#[test]
fn record_gate_mutates_last_transition_at() {
    let mut proposal = sample_proposal();
    let before = proposal.last_transition_at;
    // Force the previous transition timestamp backward so the
    // mutation is unambiguously observable even when the test
    // executes in the same epoch second as `new()`.
    proposal.last_transition_at = before - 60;
    let stale = proposal.last_transition_at;
    proposal.record_gate(GateOutcome {
        gate: GateName::Validator,
        passed: true,
        details: vec![],
        recorded_at: before,
    });
    assert!(
        proposal.last_transition_at > stale,
        "record_gate must advance last_transition_at (was {stale}, now {})",
        proposal.last_transition_at
    );
    assert_eq!(proposal.gate_outcomes.len(), 1);
}

/// Plan D — the synthesized `AtomDefinition` overlay row MUST share
/// the materialized TaskNode's id + port shape; otherwise the planner
/// sees one contract during search and the spliced TaskNode declares
/// another, and every `EdgeContract` proof against the promoted node
/// would be inconsistent.
#[test]
fn promoted_proposal_synthesizes_atom_definition_with_matching_ports() {
    let mut proposal = sample_proposal();
    proposal.lifecycle = ProposalLifecycle::Promoted {
        task_node_id: proposal.node_id.clone(),
    };
    let atom = promoted_proposal_to_atom_definition(&proposal)
        .expect("Promoted proposal must synthesize an AtomDefinition");

    // The id MUST equal `node_id` (NOT the proposal id). The planner
    // keys atoms by `id`, and the materialized TaskNode also uses
    // `node_id`; lifecycle alignment requires both records to share
    // one stable handle.
    assert_eq!(atom.id, proposal.node_id);
    assert_ne!(atom.id, proposal.id.as_str());

    // Computation-equivalent role per the helper's contract.
    assert_eq!(atom.role, AtomRole::Operation);

    // Output ports must match the materialized TaskNode in count +
    // semantic-type ids.
    let authority = PromotionAuthority {
        kind: "sme_user".into(),
        id: "alan".into(),
        at: "2026-05-12T12:00:00Z".into(),
    };
    let materialized = proposal_to_materialized_task_node(&proposal, authority);
    assert_eq!(atom.outputs.len(), materialized.outputs.len());
    for (atom_port, node_port) in atom.outputs.iter().zip(materialized.outputs.iter()) {
        assert_eq!(atom_port.name, node_port.name);
        match (&atom_port.semantic_type, &node_port.semantic_type) {
            (
                SemanticType::OntologyTerm { iri: a_iri, .. },
                SemanticType::OntologyTerm { iri: n_iri, .. },
            ) => {
                assert_eq!(
                    a_iri, n_iri,
                    "overlay-atom semantic IRI must match TaskNode"
                );
            }
            (a, n) => panic!("expected OntologyTerm on both sides, got atom={a:?} node={n:?}"),
        }
    }

    // Marker attribute MUST be present so downstream code can
    // distinguish overlay atoms from registry atoms.
    assert_eq!(
        atom.attributes
            .get("_proposal_overlay")
            .and_then(|v| v.as_bool()),
        Some(true),
        "overlay atom must carry `_proposal_overlay: true` marker attribute"
    );

    // Inputs must be empty (proposals only declare output ports —
    // forward search treats overlay atoms like a source frontier).
    assert!(
        atom.inputs.is_empty(),
        "overlay atom must declare no inputs (proposals only carry parent_terms as outputs)"
    );

    // Validators propagate so the v4 validator gate has the same
    // obligations to run as the materialized TaskNode.
    assert_eq!(atom.validators, proposal.validation_tests);
}

/// Plan D — only `Promoted` lifecycle participates in the overlay.
/// Pre-promotion the proposal MUST NOT influence planning; surfacing
/// it as an atom before SME signoff would let the planner thread a
/// downstream dependency through an un-gated node.
#[test]
fn non_promoted_proposal_returns_none() {
    let base = sample_proposal();

    // Helper to clone the base and force a lifecycle variant onto it.
    let with_lifecycle = |lifecycle: ProposalLifecycle| {
        let mut p = base.clone();
        p.lifecycle = lifecycle;
        p
    };

    let variants = [
        ProposalLifecycle::PendingValidation,
        ProposalLifecycle::PendingSandbox,
        ProposalLifecycle::AwaitingSignoff,
        ProposalLifecycle::Blocked {
            reason: ProposalBlockerReason::ValidatorFailed { failures: vec![] },
        },
        ProposalLifecycle::Rejected {
            rationale: Some("wrong approach".into()),
        },
    ];

    for lifecycle in variants {
        let p = with_lifecycle(lifecycle.clone());
        assert!(
            promoted_proposal_to_atom_definition(&p).is_none(),
            "lifecycle {:?} must NOT produce an overlay atom",
            lifecycle.kind_str()
        );
    }

    // Sanity: Promoted DOES produce a row.
    let promoted = with_lifecycle(ProposalLifecycle::Promoted {
        task_node_id: base.node_id.clone(),
    });
    assert!(
        promoted_proposal_to_atom_definition(&promoted).is_some(),
        "Promoted lifecycle MUST produce an overlay atom (positive control)"
    );
}

#[test]
fn record_gate_is_append_only() {
    // Repeated record_gate calls append entries; they never replace.
    let mut proposal = sample_proposal();
    proposal.record_gate(GateOutcome {
        gate: GateName::Validator,
        passed: true,
        details: vec![],
        recorded_at: 100,
    });
    proposal.record_gate(GateOutcome {
        gate: GateName::Sandbox,
        passed: false,
        details: vec!["StaticAnalysisRequired".into()],
        recorded_at: 110,
    });
    assert_eq!(proposal.gate_outcomes.len(), 2);
    assert!(matches!(
        proposal.gate_outcomes[0].gate,
        GateName::Validator
    ));
    assert!(matches!(proposal.gate_outcomes[1].gate, GateName::Sandbox));
    assert!(!proposal.gate_outcomes[1].passed);
}
