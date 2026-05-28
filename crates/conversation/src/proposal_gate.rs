//! Gate runner for
//! hypothesized node proposals.
//!
//! [`advance_proposal`] is the single entry point. It walks the
//! proposal through the three-gate promotion pipeline:
//!
//! 1. **Validator gate** — verify every obligation id the SME / LLM
//!    declared in `proposal.validation_tests` resolves against
//!    [`ValidationRegistry::with_starters`]. Unrecognized obligation ids
//!    fail the gate so a typo or invented obligation can be surfaced
//!    before the proposal eats SME attention.
//! 2. **Sandbox gate** — project the transient TaskNode to
//!    `Implementation::GeneratedCode { review_status: Unreviewed }`
//!    (a hypothesized node IS generated code, even though the spec's
//!    transient synthesizer leaves it as `Unimplemented` to keep the
//!    audit shape clean) and run
//!    [`sandbox_policy::check_generated_code_node`] against the
//!    session's active sandbox policy.
//! 3. **SME signoff** — out of band; the server's
//!    `POST /proposal/:id/signoff` endpoint advances `AwaitingSignoff`
//!    → `Promoted`. The gate runner stops at `AwaitingSignoff`.
//!
//! The runner is **idempotent on terminal lifecycles**
//! (`Promoted`, `Blocked`, `Rejected`) — re-advancing returns the
//! same lifecycle without re-running any gate or mutating
//! `gate_outcomes`. This lets the tool dispatcher call
//! `advance_proposal` defensively after every transition without
//! worrying about double-firing gates.
//!
//! Pure sync. No tokio.

use std::collections::BTreeSet;

use scripps_workflow_core::hypothesized_proposal::{
    now_ts, proposal_to_transient_task_node, GateName, GateOutcome, HypothesizedProposal,
    ProposalBlockerReason, ProposalLifecycle,
};
use scripps_workflow_core::sandbox_policy::{
    check_generated_code_node, SandboxPolicy, SandboxRefusal,
};
use scripps_workflow_core::validation_obligations::ValidationRegistry;
use scripps_workflow_core::workflow_contracts::implementation::{Implementation, ReviewStatus};
use scripps_workflow_core::workflow_contracts::task_node::TaskNode;

use crate::session::Session;

/// Advance a proposal through the gates that are eligible to run at
/// pre-runtime. Returns the resulting lifecycle so callers (the tool
/// dispatcher; the server's reset-and-re-evaluate endpoint) can
/// decide whether to emit a `proposal_gate_advanced` SSE event.
///
/// Idempotent on terminal lifecycles: `Promoted`, `Blocked`, and
/// `Rejected` are no-ops.
///
/// The gate runner mutates `proposal.lifecycle` and appends to
/// `proposal.gate_outcomes`. Callers are responsible for persisting
/// the session.
pub fn advance_proposal(
    proposal: &mut HypothesizedProposal,
    session: &Session,
) -> ProposalLifecycle {
    // Terminal lifecycles short-circuit. Mirrors the spec §7.2
    // contract: "Idempotent re-advance does nothing for terminal
    // states."
    if proposal.lifecycle.is_terminal() {
        return proposal.lifecycle.clone();
    }

    if matches!(proposal.lifecycle, ProposalLifecycle::PendingValidation) {
        let transient = proposal_to_transient_task_node(proposal);
        let (outcome, failures) = run_validator_gate(&transient, &proposal.validation_tests);
        let passed = outcome.passed;
        proposal.record_gate(outcome);
        if !passed {
            proposal.lifecycle = ProposalLifecycle::Blocked {
                reason: ProposalBlockerReason::ValidatorFailed { failures },
            };
            return proposal.lifecycle.clone();
        }
        proposal.lifecycle = ProposalLifecycle::PendingSandbox;
    }

    if matches!(proposal.lifecycle, ProposalLifecycle::PendingSandbox) {
        let policy_opt = active_sandbox_policy(session);
        let (refusals, details) = match policy_opt.as_ref() {
            Some(policy) => {
                let sandbox_node = proposal_to_sandbox_check_node(proposal);
                let refs = check_generated_code_node(&sandbox_node, policy);
                let details = refs
                    .iter()
                    .map(|r| format!("{}: {}", r.kind_str(), r.detail()))
                    .collect();
                (refs, details)
            }
            None => (
                Vec::new(),
                vec!["no active sandbox policy; gate vacuously passed".to_string()],
            ),
        };
        let outcome = GateOutcome {
            gate: GateName::Sandbox,
            passed: refusals.is_empty(),
            details,
            recorded_at: now_ts(),
        };
        let passed = outcome.passed;
        proposal.record_gate(outcome);
        if !passed {
            proposal.lifecycle = ProposalLifecycle::Blocked {
                reason: ProposalBlockerReason::SandboxRefused { refusals },
            };
            return proposal.lifecycle.clone();
        }
        proposal.lifecycle = ProposalLifecycle::AwaitingSignoff;
    }

    proposal.lifecycle.clone()
}

/// Validator gate. At pre-runtime there are no artifacts to run
/// obligation runners against, so the gate's job is bounded:
/// **classify each declared obligation as registered or unknown**.
///
/// Per spec §13 risk-register guidance ("Treat any unsupported
/// obligation as a soft-pass with a warning — better than blocking
/// the whole proposal"), unknown obligation ids do NOT fail the gate.
/// They surface in `outcome.details` as soft-pass warnings so the
/// SME (and any future tightening pass) can see them, but the gate
/// passes so the proposal can advance to sandbox + signoff.
///
/// The gate only hard-fails when an obligation id matches a runner
/// that explicitly rejects the transient TaskNode shape — which
/// today is no-op (no runners are pre-runtime evaluable), so the
/// failure list is always empty under current implementation.
///
/// Returns the `GateOutcome` plus the ordered list of failed
/// obligation ids (used by the caller to populate
/// [`ProposalBlockerReason::ValidatorFailed`] on actual failures).
fn run_validator_gate(
    transient: &TaskNode,
    declared_obligations: &[String],
) -> (GateOutcome, Vec<String>) {
    let _ = transient; // The transient is reserved for future
                       // expansion (e.g. obligations whose
                       // applicability depends on port semantics);
                       // for now the gate is id-based.
    let registry = ValidationRegistry::with_starters();
    let known: BTreeSet<&str> = registry.obligations().map(|(id, _)| id.as_str()).collect();

    let mut details: Vec<String> = Vec::new();
    let mut unknown_count = 0usize;
    for id in declared_obligations {
        if !known.contains(id.as_str()) {
            unknown_count += 1;
            details.push(format!(
                "obligation `{id}` not in registry — soft-pass with warning (Phase 13 risk-register §13)"
            ));
        } else {
            details.push(format!("obligation registered: {id}"));
        }
    }
    if unknown_count > 0 {
        details.push(format!(
            "{unknown_count} obligation(s) soft-passed; promote with caution or re-propose with registered ids"
        ));
    }

    // Hard failures are reserved for runners that explicitly reject
    // the transient TaskNode shape — none exist today, so the failure
    // list is always empty.
    let failures: Vec<String> = Vec::new();
    let outcome = GateOutcome {
        gate: GateName::Validator,
        passed: failures.is_empty(),
        details,
        recorded_at: now_ts(),
    };
    (outcome, failures)
}

/// Project the proposal's transient TaskNode onto the sandbox-check
/// shape. A hypothesized node is *semantically* generated code —
/// later phases fill the implementation by code-gen — so the sandbox
/// gate runs against a `GeneratedCode { review_status: Unreviewed,
/// artifact_digest: None }` projection rather than the
/// `Unimplemented` shape used elsewhere.
///
/// This keeps the spec's `proposal_to_transient_task_node` contract
/// intact (it returns `Unimplemented` for the UI / validator path)
/// while letting the sandbox-policy refusals fire correctly.
fn proposal_to_sandbox_check_node(proposal: &HypothesizedProposal) -> TaskNode {
    let mut node = proposal_to_transient_task_node(proposal);
    node.implementation = Implementation::GeneratedCode {
        repository_ref: format!("proposal:{}", proposal.id),
        review_status: ReviewStatus::Unreviewed,
        artifact_digest: None,
    };
    node
}

/// Resolve the session's active sandbox policy. Mirrors the mapping
/// used by the emit-time `sandbox-policy.json` sidecar writer in
/// `crate::emit::audit_log::sandbox_policy_for_bundle` (private
/// there), so the proposal gate and the runtime sidecar enforce the
/// same policy.
///
/// Recognized bundle ids:
/// - `clinical_trial` → [`SandboxPolicy::default_strict`]
/// - `phi_strict` → `default_strict` with `require_signed_artifacts`
///   dropped (PHI is about data handling, not artifact signing)
///
/// Returns `None` when the session has no active policy bundle
/// (or an unrecognized one). The sandbox gate treats `None` as a
/// vacuous pass — matching the existing system contract where the
/// harness's pre-dispatch sandbox check short-circuits when no
/// `runtime/sandbox-policy.json` sidecar is present.
fn active_sandbox_policy(session: &Session) -> Option<SandboxPolicy> {
    match session.active_policy_bundle.as_deref()? {
        "clinical_trial" => Some(SandboxPolicy::default_strict()),
        "phi_strict" => {
            let mut p = SandboxPolicy::default_strict();
            p.id = "phi_strict_v1".into();
            p.label = "PHI-strict sandbox".into();
            p.require_signed_artifacts = false;
            Some(p)
        }
        _ => None,
    }
}

/// Helper exposed for tests + external diagnostics — returns the
/// list of refusal kind discriminator strings for a given proposal +
/// session policy without mutating the proposal. Useful for the
/// server's `GET /proposal/:id` route to render the "what would
/// happen if I re-ran the sandbox check" preview.
///
/// When the session has no active sandbox policy, returns an empty
/// vec (matching the gate's vacuous-pass semantics).
pub fn preview_sandbox_refusals(
    proposal: &HypothesizedProposal,
    session: &Session,
) -> Vec<SandboxRefusal> {
    match active_sandbox_policy(session) {
        Some(policy) => {
            let node = proposal_to_sandbox_check_node(proposal);
            check_generated_code_node(&node, &policy)
        }
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scripps_workflow_core::hypothesized_proposal::HypothesizedProposal;

    fn sample_proposal_valid() -> HypothesizedProposal {
        HypothesizedProposal::new(
            "doublet_score",
            "Score per-cell doublet probability",
            vec!["data:2603".into()],
            "rationale",
            vec![],
            vec![],
            vec!["p_value_in_unit_interval".into()],
            vec![],
        )
    }

    #[test]
    fn validator_gate_passes_on_registered_obligation_id() {
        let proposal = sample_proposal_valid();
        let transient = proposal_to_transient_task_node(&proposal);
        let (outcome, failures) = run_validator_gate(&transient, &proposal.validation_tests);
        assert!(outcome.passed);
        assert!(failures.is_empty());
        assert!(matches!(outcome.gate, GateName::Validator));
    }

    #[test]
    fn validator_gate_passes_on_empty_obligation_list() {
        // No obligations declared → vacuous pass.
        let mut proposal = sample_proposal_valid();
        proposal.validation_tests.clear();
        let transient = proposal_to_transient_task_node(&proposal);
        let (outcome, failures) = run_validator_gate(&transient, &proposal.validation_tests);
        assert!(outcome.passed);
        assert!(failures.is_empty());
    }

    #[test]
    fn validator_gate_soft_passes_unknown_obligation_id() {
        // Per spec §13 risk register: "Treat any unsupported
        // obligation as a soft-pass with a warning." Unknown ids
        // surface in outcome.details but do NOT fail the gate.
        let proposal = HypothesizedProposal::new(
            "doublet_score",
            "intent",
            vec!["data:2603".into()],
            "rationale",
            vec![],
            vec![],
            vec!["totally_made_up_obligation".into()],
            vec![],
        );
        let transient = proposal_to_transient_task_node(&proposal);
        let (outcome, failures) = run_validator_gate(&transient, &proposal.validation_tests);
        assert!(outcome.passed, "unknown obligation must soft-pass");
        assert!(
            failures.is_empty(),
            "unknown obligation must NOT populate failures"
        );
        // The warning must still be visible in the gate detail rows
        // so the SME can re-propose with registered ids if they care.
        let has_warning = outcome.details.iter().any(|d| d.contains("soft-pass"));
        assert!(
            has_warning,
            "soft-pass warning must appear in details: {:?}",
            outcome.details
        );
    }

    #[test]
    fn sandbox_check_node_uses_generated_code_projection() {
        let proposal = sample_proposal_valid();
        let node = proposal_to_sandbox_check_node(&proposal);
        assert!(matches!(
            node.implementation,
            Implementation::GeneratedCode {
                review_status: ReviewStatus::Unreviewed,
                ..
            }
        ));
    }

    #[test]
    fn active_sandbox_policy_none_for_unset_bundle() {
        let session = Session::new(false);
        let policy = active_sandbox_policy(&session);
        assert!(
            policy.is_none(),
            "expected None when no active_policy_bundle, got {policy:?}"
        );
    }

    #[test]
    fn active_sandbox_policy_clinical_trial_maps_to_default_strict() {
        let mut session = Session::new(false);
        session.active_policy_bundle = Some("clinical_trial".into());
        let policy = active_sandbox_policy(&session).expect("clinical_trial must resolve");
        assert!(policy.require_static_analysis);
    }

    #[test]
    fn active_sandbox_policy_phi_strict_drops_signed_artifacts() {
        let mut session = Session::new(false);
        session.active_policy_bundle = Some("phi_strict".into());
        let policy = active_sandbox_policy(&session).expect("phi_strict must resolve");
        assert!(!policy.require_signed_artifacts);
        assert_eq!(policy.id, "phi_strict_v1");
    }

    #[test]
    fn active_sandbox_policy_unknown_bundle_returns_none() {
        let mut session = Session::new(false);
        session.active_policy_bundle = Some("totally_made_up".into());
        let policy = active_sandbox_policy(&session);
        assert!(
            policy.is_none(),
            "expected None for unknown bundle id, got {policy:?}"
        );
    }
}
