//! Tier F property test for F21 — every refusal carries actionable
//! `UnblockPath`s, or is unconditional hard policy.
//!
//! The invariant the property locks in:
//!
//! - **Non-hard refusals** (`ClinicalGateFailed`, `LicenseMissing`,
//! `GoalUnderspecified`, `SemanticLossNotAuthorized`,
//! `PopulationOutOfCoverage`, `SandboxRefused`,
//! `UncategorizedBlocker`) MUST carry at least one
//! `UnblockPath`. `RefusalReport::validate()` returns
//! `MissingUnblockPaths` when the vec is empty.
//! - **Hard-policy refusals** (`HardPolicyViolation`,
//! `PhiLeakBlocked`, `PrivacyViolation`) permit an empty vec —
//! `validate()` is `Ok(())` even with no paths. These three kinds
//! are unconditional refusals; the SME's only recovery is to branch
//! the session.
//!
//! The property is asserted both empirically (every variant tested
//! over a prop-test sweep) and by construction (`permits_no_unblock_paths()`
//! is the canonical source of truth for which kinds may carry an
//! empty vec).

use proptest::prelude::*;
use ecaa_workflow_core::sandbox_refusal_category::SandboxRefusalCategory;
use ecaa_workflow_core::workflow_contracts::outcome::{RefusalReport, RefusalValidationError};
use ecaa_workflow_core::workflow_contracts::refusal_kind::RefusalKind;
use ecaa_workflow_core::workflow_contracts::unblock_path::{ProjectedOutcome, UnblockPath};

/// Strategy over non-hard refusal kinds. The property below asserts
/// that none of these admit an empty `unblock_paths` vec.
fn non_hard_kind_strategy() -> impl Strategy<Value = RefusalKind> {
    prop_oneof![
        Just(RefusalKind::ClinicalGateFailed),
        Just(RefusalKind::LicenseMissing),
        Just(RefusalKind::GoalUnderspecified),
        Just(RefusalKind::SemanticLossNotAuthorized),
        Just(RefusalKind::PopulationOutOfCoverage {
            workflow_id: "wf".into(),
            sample_label: "sample".into(),
            validated_labels: vec!["validated_A".into()],
            suggested_waiver_authority: "clinical_lead".into(),
        }),
        Just(RefusalKind::UncategorizedBlocker),
        Just(RefusalKind::SandboxRefused {
            category: SandboxRefusalCategory::Network,
        }),
        Just(RefusalKind::SandboxRefused {
            category: SandboxRefusalCategory::Filesystem,
        }),
        Just(RefusalKind::SandboxRefused {
            category: SandboxRefusalCategory::Resource,
        }),
        Just(RefusalKind::SandboxRefused {
            category: SandboxRefusalCategory::Identity,
        }),
        Just(RefusalKind::SandboxRefused {
            category: SandboxRefusalCategory::Capability,
        }),
        Just(RefusalKind::SandboxRefused {
            category: SandboxRefusalCategory::SupplyChain,
        }),
        Just(RefusalKind::SandboxRefused {
            category: SandboxRefusalCategory::OutputValidation,
        }),
    ]
}

/// Strategy over hard-policy refusal kinds. These three permit empty
/// `unblock_paths`.
fn hard_kind_strategy() -> impl Strategy<Value = RefusalKind> {
    prop_oneof![
        Just(RefusalKind::HardPolicyViolation),
        Just(RefusalKind::PhiLeakBlocked),
        Just(RefusalKind::PrivacyViolation),
    ]
}

proptest! {
    /// F21 — every non-hard refusal kind with zero unblock paths must
    /// fail validation with `MissingUnblockPaths`.
    #[test]
    fn non_hard_refusals_must_carry_unblock_paths(
        kind in non_hard_kind_strategy()
    ) {
        let report = RefusalReport {
            id: "r1".into(),
            kind: kind.clone(),
            statement: "test".into(),
            references: vec![],
            unblock_paths: vec![],
        };
        let res = report.validate();
        prop_assert!(
            matches!(res, Err(RefusalValidationError::MissingUnblockPaths { .. })),
            "refusal kind {:?} with empty unblock_paths should fail validation \
             (kind permits_no_unblock_paths={}), got: {:?}",
            kind, kind.permits_no_unblock_paths(), res
        );
    }

    /// F21 — every hard-policy refusal kind admits an empty
    /// `unblock_paths` vec. `validate()` returns `Ok(())`.
    #[test]
    fn hard_refusals_permit_empty_unblock_paths(
        kind in hard_kind_strategy()
    ) {
        let report = RefusalReport {
            id: "r1".into(),
            kind: kind.clone(),
            statement: "test".into(),
            references: vec![],
            unblock_paths: vec![],
        };
        prop_assert!(
            report.validate().is_ok(),
            "hard refusal kind {:?} with empty unblock_paths should pass validation",
            kind
        );
    }

    /// F21 invariant complement — any non-hard refusal carrying at
    /// least one unblock path passes validation.
    #[test]
    fn non_hard_refusals_with_at_least_one_path_pass(
        kind in non_hard_kind_strategy()
    ) {
        let report = RefusalReport {
            id: "r1".into(),
            kind: kind.clone(),
            statement: "test".into(),
            references: vec![],
            unblock_paths: vec![UnblockPath::EscalateToReviewer {
                reviewer_class: "bioinformatics_lead".into(),
                required_artifacts: vec!["refusal_review".into()],
                target_outcome: ProjectedOutcome::DraftDag,
            }],
        };
        prop_assert!(
            report.validate().is_ok(),
            "refusal kind {:?} with one unblock_path should pass validation",
            kind
        );
    }

    /// F21 — `permits_no_unblock_paths()` is the canonical source of
    /// truth: a kind for which this returns `true` admits the empty
    /// vec; the inverse holds for `false`.
    #[test]
    fn permits_no_unblock_paths_matches_validate(
        kind in prop_oneof![non_hard_kind_strategy(), hard_kind_strategy()]
    ) {
        let report_empty = RefusalReport {
            id: "r1".into(),
            kind: kind.clone(),
            statement: "test".into(),
            references: vec![],
            unblock_paths: vec![],
        };
        if kind.permits_no_unblock_paths() {
            prop_assert!(report_empty.validate().is_ok());
        } else {
            prop_assert!(report_empty.validate().is_err());
        }
    }
}
