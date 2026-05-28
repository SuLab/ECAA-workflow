//! Integration tests for [`scripps_workflow_conversation::proposal_gate`].
//!
//! Covers the gate runner's promotion-pipeline contract:
//! full pass, validator failure, sandbox refusal, idempotent
//! re-advance on terminal states, and active-policy-bundle
//! propagation.

use scripps_workflow_conversation::proposal_gate::advance_proposal;
use scripps_workflow_conversation::session::Session;
use scripps_workflow_core::hypothesized_proposal::{
    HypothesizedProposal, ProposalBlockerReason, ProposalLifecycle,
};

fn fresh_session() -> Session {
    Session::new(false)
}

fn proposal_with_valid_obligation() -> HypothesizedProposal {
    HypothesizedProposal::new(
        "doublet_score",
        "Score per-cell doublet probability",
        vec!["data:2603".into()],
        "SME asked for doublet probability output",
        vec![],
        vec![],
        vec!["p_value_in_unit_interval".into()],
        vec![],
    )
}

fn proposal_with_unknown_obligation() -> HypothesizedProposal {
    HypothesizedProposal::new(
        "doublet_score",
        "intent",
        vec!["data:2603".into()],
        "rationale",
        vec![],
        vec![],
        vec!["totally_invented_obligation".into()],
        vec![],
    )
}

#[test]
fn full_pass_reaches_awaiting_signoff_when_no_active_policy() {
    // With no `active_policy_bundle` set the sandbox gate is a
    // vacuous pass (matching the existing system contract — the
    // harness skips its pre-dispatch sandbox check when there is
    // no `runtime/sandbox-policy.json` sidecar). Validator gate
    // passes on the recognized `p_value_in_unit_interval` id; the
    // proposal then reaches `AwaitingSignoff`.
    let session = fresh_session();
    assert!(session.active_policy_bundle.is_none());
    let mut proposal = proposal_with_valid_obligation();
    let lifecycle = advance_proposal(&mut proposal, &session);
    assert!(
        matches!(lifecycle, ProposalLifecycle::AwaitingSignoff),
        "expected AwaitingSignoff after both gates pass, got {lifecycle:?}"
    );
    // Both gate outcomes recorded; both passed.
    assert_eq!(proposal.gate_outcomes.len(), 2);
    assert!(proposal.gate_outcomes.iter().all(|o| o.passed));
}

#[test]
fn validator_soft_passes_unknown_obligation_with_warning() {
    // Spec §13 risk register: unknown obligation ids do NOT block —
    // they soft-pass with a warning. The sandbox gate runs after,
    // and (with no active policy bundle) vacuously passes, putting
    // the proposal at `AwaitingSignoff`.
    let session = fresh_session();
    let mut proposal = proposal_with_unknown_obligation();
    let lifecycle = advance_proposal(&mut proposal, &session);
    assert!(
        matches!(lifecycle, ProposalLifecycle::AwaitingSignoff),
        "expected AwaitingSignoff (validator soft-pass + vacuous sandbox), got {lifecycle:?}"
    );
    // Validator outcome must carry the soft-pass warning.
    let validator_outcome = proposal
        .gate_outcomes
        .iter()
        .find(|o| {
            matches!(
                o.gate,
                scripps_workflow_core::hypothesized_proposal::GateName::Validator
            )
        })
        .expect("validator gate must have run");
    assert!(validator_outcome.passed);
    assert!(
        validator_outcome
            .details
            .iter()
            .any(|d| d.contains("soft-pass")),
        "soft-pass warning must surface in details: {:?}",
        validator_outcome.details
    );
}

#[test]
fn sandbox_refusal_blocks_with_refusal_kinds() {
    // Activate `clinical_trial` bundle → `SandboxPolicy::default_strict`.
    // The proposal's GeneratedCode projection is `Unreviewed`, so
    // `default_strict` refuses with `StaticAnalysisRequired`.
    let mut session = fresh_session();
    session.active_policy_bundle = Some("clinical_trial".into());
    let mut proposal = proposal_with_valid_obligation();
    let _ = advance_proposal(&mut proposal, &session);
    match &proposal.lifecycle {
        ProposalLifecycle::Blocked {
            reason: ProposalBlockerReason::SandboxRefused { refusals },
        } => {
            assert!(
                !refusals.is_empty(),
                "expected sandbox refusal kinds, got empty vec"
            );
            let kinds: Vec<&'static str> = refusals.iter().map(|r| r.kind_str()).collect();
            assert!(
                kinds.contains(&"StaticAnalysisRequired"),
                "expected StaticAnalysisRequired refusal, got {kinds:?}"
            );
        }
        other => panic!("expected Blocked(SandboxRefused), got {other:?}"),
    }
}

#[test]
fn re_advance_is_idempotent_on_blocked() {
    // Drive a proposal into Blocked via the sandbox gate (clinical
    // trial bundle refuses an Unreviewed projection), then re-advance
    // and assert the runner is a no-op.
    let mut session = fresh_session();
    session.active_policy_bundle = Some("clinical_trial".into());
    let mut proposal = proposal_with_valid_obligation();
    let lifecycle_after_first = advance_proposal(&mut proposal, &session);
    assert!(matches!(
        lifecycle_after_first,
        ProposalLifecycle::Blocked {
            reason: ProposalBlockerReason::SandboxRefused { .. }
        }
    ));
    let gate_count_after_first = proposal.gate_outcomes.len();
    let transition_after_first = proposal.last_transition_at;

    // Re-advance: lifecycle is terminal, runner must short-circuit.
    let lifecycle_after_second = advance_proposal(&mut proposal, &session);
    assert_eq!(lifecycle_after_first, lifecycle_after_second);
    assert_eq!(proposal.gate_outcomes.len(), gate_count_after_first);
    assert_eq!(proposal.last_transition_at, transition_after_first);
}

#[test]
fn re_advance_is_idempotent_on_promoted() {
    // Manually set the lifecycle to Promoted and observe the
    // runner is a no-op.
    let session = fresh_session();
    let mut proposal = proposal_with_valid_obligation();
    proposal.lifecycle = ProposalLifecycle::Promoted {
        task_node_id: "doublet_score".into(),
    };
    let original_outcomes = proposal.gate_outcomes.clone();
    let original_transition_at = proposal.last_transition_at;
    let lifecycle = advance_proposal(&mut proposal, &session);
    assert!(matches!(lifecycle, ProposalLifecycle::Promoted { .. }));
    assert_eq!(proposal.gate_outcomes, original_outcomes);
    assert_eq!(proposal.last_transition_at, original_transition_at);
}

#[test]
fn re_advance_is_idempotent_on_rejected() {
    let session = fresh_session();
    let mut proposal = proposal_with_valid_obligation();
    proposal.lifecycle = ProposalLifecycle::Rejected {
        rationale: Some("wrong approach".into()),
    };
    let original_outcomes = proposal.gate_outcomes.clone();
    let lifecycle = advance_proposal(&mut proposal, &session);
    assert!(matches!(
        lifecycle,
        ProposalLifecycle::Rejected { ref rationale } if rationale.as_deref() == Some("wrong approach")
    ));
    assert_eq!(proposal.gate_outcomes, original_outcomes);
}

#[test]
fn active_policy_bundle_propagates_to_sandbox_check() {
    // clinical_trial bundle → SandboxPolicy::default_strict (per
    // crate::emit::audit_log::sandbox_policy_for_bundle). The
    // sandbox gate runs on the proposal's GeneratedCode projection
    // and refuses with `StaticAnalysisRequired`.
    let mut session = fresh_session();
    session.active_policy_bundle = Some("clinical_trial".into());
    let mut proposal = proposal_with_valid_obligation();
    let lifecycle = advance_proposal(&mut proposal, &session);
    match lifecycle {
        ProposalLifecycle::Blocked {
            reason: ProposalBlockerReason::SandboxRefused { refusals },
        } => {
            assert!(
                !refusals.is_empty(),
                "expected clinical_trial bundle to surface refusals"
            );
            let kinds: Vec<&'static str> = refusals.iter().map(|r| r.kind_str()).collect();
            assert!(
                kinds.contains(&"StaticAnalysisRequired"),
                "expected StaticAnalysisRequired under clinical_trial, got {kinds:?}"
            );
        }
        other => panic!("expected Blocked(SandboxRefused) under clinical_trial, got {other:?}"),
    }
}

#[test]
fn validator_passes_when_obligations_empty_then_sandbox_runs() {
    // Vacuous-pass validator (empty obligation list) MUST still
    // advance into the sandbox stage; the validator gate is not
    // gated on declaring obligations.
    let session = fresh_session();
    let mut proposal = HypothesizedProposal::new(
        "no_validators",
        "intent",
        vec!["data:2603".into()],
        "rationale",
        vec![],
        vec![],
        vec![], // no obligations
        vec![],
    );
    let _ = advance_proposal(&mut proposal, &session);
    // No active policy bundle → sandbox vacuous-passes → reaches
    // AwaitingSignoff.
    assert_eq!(proposal.lifecycle.kind_str(), "awaiting_signoff");
    // Both gates recorded outcomes.
    assert_eq!(proposal.gate_outcomes.len(), 2);
    assert!(proposal.gate_outcomes.iter().all(|o| o.passed));
}
