//! M3 — every compatibility/validation failure path is conversationally
//! actionable: each IncompatibilityReason + RefusalKind maps to a
//! BlockerKind with at least one populated UnblockPath (or is a
//! documented unconditional hard-policy refusal). Universal property
//! guarding the gate-hardening items (WG1-WG5) from silent rejects.

use ecaa_workflow_core::boundary_actionability::{
    blocker_for_incompatibility, recovery_for_refusal,
};
use ecaa_workflow_core::compatibility::reports::IncompatibilityReason;
use ecaa_workflow_core::sandbox_refusal_category::SandboxRefusalCategory;
use ecaa_workflow_core::workflow_contracts::refusal_kind::RefusalKind;
use ecaa_workflow_core::workflow_contracts::unblock_path::UnblockPath;

#[test]
fn every_incompatibility_reason_maps_to_an_actionable_blocker() {
    let all = [
        IncompatibilityReason::SemanticTypeMismatch {
            producer: "p".into(),
            consumer: "c".into(),
        },
        IncompatibilityReason::FacetMismatch {
            facet: "genome_build".into(),
            producer: "GRCh37".into(),
            consumer: "GRCh38".into(),
            rationale: "r".into(),
        },
        IncompatibilityReason::PrivacyClassWidening {
            producer: "phi".into(),
            consumer: "public".into(),
        },
        IncompatibilityReason::CardinalityMismatch {
            producer: "many".into(),
            consumer: "one".into(),
        },
        IncompatibilityReason::PolicyViolation {
            bundle_id: "b".into(),
            check_kind: "k".into(),
            statement: "s".into(),
        },
        IncompatibilityReason::ParameterMismatch {
            parameter: "assembly".into(),
            producer: "GRCh38".into(),
            consumer: "GRCm39".into(),
        },
        IncompatibilityReason::Other {
            statement: "s".into(),
        },
    ];
    for reason in all {
        let (_blocker, paths) = blocker_for_incompatibility(&reason);
        assert!(
            !paths.is_empty(),
            "IncompatibilityReason {reason:?} has no UnblockPath"
        );
        for p in &paths {
            assert!(
                unblock_path_is_populated(p),
                "empty UnblockPath for {reason:?}"
            );
        }
    }
}

#[test]
fn every_refusal_kind_is_actionable_or_documented_hard_policy() {
    let all = [
        RefusalKind::HardPolicyViolation,
        RefusalKind::PrivacyViolation,
        RefusalKind::ClinicalGateFailed,
        RefusalKind::PhiLeakBlocked,
        RefusalKind::LicenseMissing,
        RefusalKind::GoalUnderspecified,
        RefusalKind::PopulationOutOfCoverage {
            workflow_id: "w".into(),
            sample_label: "s".into(),
            validated_labels: vec!["A".into()],
            suggested_waiver_authority: "clinical_lead".into(),
        },
        RefusalKind::SemanticLossNotAuthorized,
        RefusalKind::SandboxRefused {
            category: SandboxRefusalCategory::Network,
        },
        RefusalKind::PromotionRefused,
        RefusalKind::UncategorizedBlocker,
    ];
    for kind in all {
        let (_blocker, paths) = recovery_for_refusal(&kind);
        if kind.permits_no_unblock_paths() {
            continue; // unconditional hard policy — recovery is branch-the-session
        }
        assert!(
            !paths.is_empty(),
            "RefusalKind {kind:?} (non-hard) has no UnblockPath"
        );
        for p in &paths {
            assert!(unblock_path_is_populated(p), "empty UnblockPath for {kind:?}");
        }
    }
}

fn unblock_path_is_populated(p: &UnblockPath) -> bool {
    match p {
        UnblockPath::ResolveAssumption { assumption_id, .. } => !assumption_id.is_empty(),
        UnblockPath::Waiver {
            rule_id,
            required_credentials,
            ..
        } => !rule_id.is_empty() && !required_credentials.is_empty(),
        UnblockPath::AttemptRepair {
            strategy_id, gap_id, ..
        } => !strategy_id.is_empty() && !gap_id.is_empty(),
        UnblockPath::SupplyMissingMetadata { field, .. } => !field.is_empty(),
        UnblockPath::EscalateToReviewer { reviewer_class, .. } => !reviewer_class.is_empty(),
        _ => false,
    }
}
