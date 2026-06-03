//! M3 — universal "boundary failures are conversationally actionable"
//! mapping. Each compatibility/validation failure (an engine
//! `IncompatibilityReason` or a composer `RefusalKind`) maps to a
//! `BlockerKind` plus at least one populated `UnblockPath` — so a
//! reviewer can mechanically prove no gate refusal is a dead end. The
//! companion conformance test enumerates every variant; this keeps the
//! gate-hardening items (WG1-WG5) from introducing a silent reject.

use crate::blocker::BlockerKind;
use crate::compatibility::reports::IncompatibilityReason;
use crate::workflow_contracts::refusal_kind::RefusalKind;
use crate::workflow_contracts::unblock_path::{ProjectedOutcome, UnblockPath};

/// Map an engine incompatibility reason to a typed blocker + the
/// recovery affordances an SME can dispatch. Never returns an empty
/// path list (every compatibility failure is recoverable in chat).
pub fn blocker_for_incompatibility(
    reason: &IncompatibilityReason,
) -> (BlockerKind, Vec<UnblockPath>) {
    match reason {
        IncompatibilityReason::SemanticTypeMismatch { producer, consumer } => (
            BlockerKind::DataShapeMismatch {
                expected: consumer.clone(),
                actual: producer.clone(),
            },
            vec![UnblockPath::AttemptRepair {
                strategy_id: "insert_type_adapter".into(),
                gap_id: format!("type:{producer}->{consumer}"),
                target_outcome: ProjectedOutcome::PartialDag,
            }],
        ),
        IncompatibilityReason::FacetMismatch { facet, .. } => (
            BlockerKind::AwaitingStructuredDecision {
                task_id: "compose".into(),
                decision_points_path: format!("facet:{facet}"),
                summary: format!("Resolve {facet} mismatch"),
            },
            vec![UnblockPath::ResolveAssumption {
                assumption_id: format!("facet:{facet}"),
                suggested_resolution: None,
                target_outcome: ProjectedOutcome::ValidatedExecutableDag,
            }],
        ),
        IncompatibilityReason::PrivacyClassWidening { .. } => (
            BlockerKind::ControlledAccessViolation {
                task_id: "compose".into(),
                port_name: "edge".into(),
                attempted_call: "privacy_class_widening".into(),
            },
            vec![UnblockPath::EscalateToReviewer {
                reviewer_class: "privacy_officer".into(),
                required_artifacts: vec![],
                target_outcome: ProjectedOutcome::DraftDag,
            }],
        ),
        IncompatibilityReason::CardinalityMismatch { producer, consumer } => (
            BlockerKind::DataShapeMismatch {
                expected: consumer.clone(),
                actual: producer.clone(),
            },
            vec![UnblockPath::AttemptRepair {
                strategy_id: "insert_scatter_gather".into(),
                gap_id: "cardinality".into(),
                target_outcome: ProjectedOutcome::PartialDag,
            }],
        ),
        IncompatibilityReason::PolicyViolation {
            bundle_id,
            check_kind,
            ..
        } => (
            BlockerKind::AwaitingSmeApproval {
                stage_id: "compose".into(),
                top_candidate: format!("{bundle_id}:{check_kind}"),
                runner_ups: vec![],
            },
            vec![UnblockPath::Waiver {
                rule_id: format!("{bundle_id}:{check_kind}"),
                required_credentials: vec!["clinical_lead".into()],
                target_outcome: ProjectedOutcome::DraftDag,
            }],
        ),
        IncompatibilityReason::ParameterMismatch {
            parameter,
            producer,
            consumer,
        } => (
            BlockerKind::AwaitingStructuredDecision {
                task_id: "compose".into(),
                decision_points_path: format!("parameter:{parameter}"),
                summary: format!(
                    "Declared parameter '{parameter}' clash: producer {producer} vs consumer {consumer}"
                ),
            },
            vec![UnblockPath::ResolveAssumption {
                assumption_id: format!("parameter:{parameter}"),
                suggested_resolution: None,
                target_outcome: ProjectedOutcome::DraftDag,
            }],
        ),
        IncompatibilityReason::Other { statement } => (
            BlockerKind::AwaitingStructuredDecision {
                task_id: "compose".into(),
                decision_points_path: "clarification".into(),
                summary: statement.clone(),
            },
            vec![UnblockPath::SupplyMissingMetadata {
                field: "clarification".into(),
                suggested_value: None,
                target_outcome: ProjectedOutcome::DraftDag,
            }],
        ),
    }
}

/// Map a composer refusal kind to a typed blocker + recovery paths.
/// Unconditional hard-policy kinds (per
/// `RefusalKind::permits_no_unblock_paths`) legitimately return an empty
/// path list — the only recovery is branching the session.
pub fn recovery_for_refusal(kind: &RefusalKind) -> (BlockerKind, Vec<UnblockPath>) {
    if kind.permits_no_unblock_paths() {
        return (
            BlockerKind::AwaitingSmeApproval {
                stage_id: "compose".into(),
                top_candidate: kind.canonical_name().into(),
                runner_ups: vec![],
            },
            vec![],
        );
    }
    match kind {
        RefusalKind::LicenseMissing => (
            BlockerKind::ControlledAccessViolation {
                task_id: "compose".into(),
                port_name: "license".into(),
                attempted_call: "license_missing".into(),
            },
            vec![UnblockPath::SupplyMissingMetadata {
                field: "license_credentials".into(),
                suggested_value: None,
                target_outcome: ProjectedOutcome::ValidatedExecutableDag,
            }],
        ),
        RefusalKind::GoalUnderspecified => (
            BlockerKind::AwaitingStructuredDecision {
                task_id: "compose".into(),
                decision_points_path: "goal".into(),
                summary: "Clarify the goal".into(),
            },
            vec![UnblockPath::SupplyMissingMetadata {
                field: "modality".into(),
                suggested_value: None,
                target_outcome: ProjectedOutcome::DraftDag,
            }],
        ),
        RefusalKind::ClinicalGateFailed
        | RefusalKind::PromotionRefused
        | RefusalKind::SandboxRefused { .. } => (
            BlockerKind::AwaitingSmeApproval {
                stage_id: "compose".into(),
                top_candidate: kind.canonical_name().into(),
                runner_ups: vec![],
            },
            vec![UnblockPath::EscalateToReviewer {
                reviewer_class: "clinical_lead".into(),
                required_artifacts: vec![],
                target_outcome: ProjectedOutcome::DraftDag,
            }],
        ),
        RefusalKind::PopulationOutOfCoverage {
            suggested_waiver_authority,
            workflow_id,
            ..
        } => (
            BlockerKind::AwaitingSmeApproval {
                stage_id: "compose".into(),
                top_candidate: workflow_id.clone(),
                runner_ups: vec![],
            },
            vec![UnblockPath::Waiver {
                rule_id: format!("population_coverage:{workflow_id}"),
                required_credentials: vec![suggested_waiver_authority.clone()],
                target_outcome: ProjectedOutcome::DraftDag,
            }],
        ),
        RefusalKind::SemanticLossNotAuthorized => (
            BlockerKind::AwaitingSmeApproval {
                stage_id: "compose".into(),
                top_candidate: "semantic_loss".into(),
                runner_ups: vec![],
            },
            vec![UnblockPath::Waiver {
                rule_id: "semantic_loss".into(),
                required_credentials: vec!["data_steward".into()],
                target_outcome: ProjectedOutcome::DraftDag,
            }],
        ),
        RefusalKind::UncategorizedBlocker => (
            BlockerKind::AwaitingStructuredDecision {
                task_id: "compose".into(),
                decision_points_path: "clarification".into(),
                summary: "Uncategorized refusal".into(),
            },
            vec![UnblockPath::SupplyMissingMetadata {
                field: "clarification".into(),
                suggested_value: None,
                target_outcome: ProjectedOutcome::DraftDag,
            }],
        ),
        // Hard-policy kinds are handled by the early return above; this
        // arm is the #[non_exhaustive] safety net for any future
        // RefusalKind variant — always actionable via reviewer escalation.
        _ => (
            BlockerKind::AwaitingSmeApproval {
                stage_id: "compose".into(),
                top_candidate: kind.canonical_name().into(),
                runner_ups: vec![],
            },
            vec![UnblockPath::EscalateToReviewer {
                reviewer_class: "clinical_lead".into(),
                required_artifacts: vec![],
                target_outcome: ProjectedOutcome::DraftDag,
            }],
        ),
    }
}
