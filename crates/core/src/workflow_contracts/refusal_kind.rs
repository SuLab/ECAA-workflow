//! `RefusalKind` — typed taxonomy for `ComposeOutcome::Refusal`.
//!
//! Every `ComposeOutcome::Refusal` carries a `RefusalKind` and a
//! (possibly empty) `Vec<UnblockPath>`. The kind drives the UI's
//! per-card dispatch (clinical-gate, license-missing, sandbox-refusal,
//! etc.) and the `permits_no_unblock_paths()` invariant gates
//! `RefusalReport::validate()`: only the unconditional hard-policy
//! kinds may carry zero unblock paths. Everything else must give the
//! SME at least one actionable recovery affordance — the invariant
//! ("every refusal carries actionable unblock paths or is unconditional
//! hard policy") is a deterministic type-boundary check.
//!
//! `SandboxRefused { category }` carries the `SandboxRefusalCategory`
//! axis so the UI can group sandbox-refusal kinds into canonical buckets
//! without re-shaping the refusal payload at every call site.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::sandbox_refusal_category::SandboxRefusalCategory;

/// Typed refusal taxonomy. Drives UI dispatch and the F21
/// "must carry actionable recovery" invariant.
///
/// The three "hard policy" kinds (`HardPolicyViolation`,
/// `PhiLeakBlocked`, `PrivacyViolation`) permit empty `unblock_paths`
/// because they're refusals where the SME has no recovery affordance
/// short of branching the session — the system MUST refuse. Every
/// other kind requires at least one populated `UnblockPath` (see
/// `RefusalReport::validate`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RefusalKind {
    /// Unconditional hard refusal — clinical policy / regulatory /
    /// safety. No unblock path is possible without branching the
    /// session.
    HardPolicyViolation,
    /// Privacy class violation (PHI exposed to a non-PHI sink,
    /// controlled-access leak to a public artifact).
    PrivacyViolation,
    /// Clinical workflow refused because a required clinical gate
    /// hasn't been satisfied (validated reference data, IRB-approved
    /// methods, etc.).
    ClinicalGateFailed,
    /// A PHI leak (or near-leak) was detected during composition.
    /// Hard refusal so the SME doesn't accidentally promote a leaky
    /// composition into execution.
    PhiLeakBlocked,
    /// A required dataset / annotation / reference is paywalled and
    /// the SME hasn't supplied license credentials.
    LicenseMissing,
    /// The goal description doesn't carry enough signal for the
    /// planner to converge (missing modality, missing project class,
    /// ambiguous archetype tie).
    GoalUnderspecified,
    /// The sample cohort the SME declared in `WorkflowIntent.sample_cohort`
    /// is outside the validated cohorts of every source archetype in
    /// the composed DAG, and no in-scope `PopulationWaiver` covers the
    /// gap. v3 P9 / §11.X.
    ///
    /// Field semantics:
    /// - `workflow_id` — the archetype id whose
    ///   `PopulationCoverageStatement` triggered the refusal (the
    ///   first one to fail on a multi-archetype DAG).
    /// - `sample_label` — the `CohortDescriptor.label` the SME
    ///   provided (mirrored back so the UI can render it without
    ///   refetching the intent).
    /// - `validated_labels` — the labels the workflow IS validated on,
    ///   surfaced inline so the UI can render the "validated cohort
    ///   set" alongside the refusal card without an extra GET.
    /// - `suggested_waiver_authority` — the canonical role the UI's
    ///   "Request waiver" button routes to (default `"clinical_lead"`).
    ///
    /// Framing constraint (v3 §11.X): this gate runs against the
    /// **workflow's** coverage statement, not the user's identity.
    /// `sample_label` is metadata about workflow applicability, not
    /// access-control on who's allowed to dispatch.
    PopulationOutOfCoverage {
        /// Workflow id.
        workflow_id: String,
        /// Sample label.
        sample_label: String,
        /// Validated labels.
        validated_labels: Vec<String>,
        /// Suggested waiver authority.
        suggested_waiver_authority: String,
    },
    /// A lossy adapter / semantic-loss step was required but the SME
    /// hasn't authorized the loss class.
    SemanticLossNotAuthorized,
    /// One or more GeneratedCode nodes refused under the active
    /// `SandboxPolicy`. The category groups the 12 sandbox-refusal
    /// kinds into the seven canonical buckets (v4 P7).
    SandboxRefused { category: SandboxRefusalCategory },
    /// One or more nodes failed the validation × lifecycle promotion
    /// grid (`config/promotion-gate-policy.yaml`). The grid's
    /// `consult` returned `Deny { missing_classes, missing_approvals }`
    /// for at least one node in the candidate DAG; the planner refuses
    /// to lower the DAG into an executable form because non-grid-
    /// passing nodes cannot be promoted to the target lifecycle state.
    /// Recovery affordances surface as
    /// `UnblockPath::EscalateToReviewer` (per missing-approval
    /// credential class plus a validation_engineer for the
    /// missing-validator-class evidence).
    PromotionRefused,
    /// Catch-all for refusals the typed taxonomy hasn't yet
    /// categorized. v4 P2's verifier substrate flags these as
    /// `VerifierDecision::ProposalRejected` so the maintainer can
    /// graduate the kind into the closed taxonomy.
    UncategorizedBlocker,
}

impl RefusalKind {
    /// Returns true iff this refusal kind permits an empty
    /// `unblock_paths` vector. The three hard-policy kinds
    /// (`HardPolicyViolation`, `PhiLeakBlocked`, `PrivacyViolation`)
    /// are unconditional refusals — the SME's only recovery is to
    /// branch the session — so they're allowed to carry zero
    /// unblock paths. Every other kind MUST carry at least one
    /// (see `RefusalReport::validate`).
    pub fn permits_no_unblock_paths(&self) -> bool {
        matches!(
            self,
            RefusalKind::HardPolicyViolation
                | RefusalKind::PhiLeakBlocked
                | RefusalKind::PrivacyViolation
        )
    }

    /// Stable canonical kebab/snake-case name. Used by
    /// `RefusalValidationError::MissingUnblockPaths.kind` and by the
    /// existing UI legacy code paths that key off the pre-Phase-4
    /// string `kind` shape.
    pub fn canonical_name(&self) -> &'static str {
        match self {
            Self::HardPolicyViolation => "hard_policy_violation",
            Self::PrivacyViolation => "privacy_violation",
            Self::ClinicalGateFailed => "clinical_gate_failed",
            Self::PhiLeakBlocked => "phi_leak_blocked",
            Self::LicenseMissing => "license_missing",
            Self::GoalUnderspecified => "goal_underspecified",
            Self::PopulationOutOfCoverage { .. } => "population_out_of_coverage",
            Self::SemanticLossNotAuthorized => "semantic_loss_not_authorized",
            Self::SandboxRefused { .. } => "sandbox_refused",
            Self::PromotionRefused => "promotion_refused",
            Self::UncategorizedBlocker => "uncategorized_blocker",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_no_unblock_paths_for_hard_kinds_only() {
        assert!(RefusalKind::HardPolicyViolation.permits_no_unblock_paths());
        assert!(RefusalKind::PhiLeakBlocked.permits_no_unblock_paths());
        assert!(RefusalKind::PrivacyViolation.permits_no_unblock_paths());

        assert!(!RefusalKind::ClinicalGateFailed.permits_no_unblock_paths());
        assert!(!RefusalKind::LicenseMissing.permits_no_unblock_paths());
        assert!(!RefusalKind::GoalUnderspecified.permits_no_unblock_paths());
        assert!(!RefusalKind::PopulationOutOfCoverage {
            workflow_id: "wf".into(),
            sample_label: "s".into(),
            validated_labels: vec!["A".into()],
            suggested_waiver_authority: "clinical_lead".into(),
        }
        .permits_no_unblock_paths());
        assert!(!RefusalKind::SemanticLossNotAuthorized.permits_no_unblock_paths());
        assert!(!RefusalKind::SandboxRefused {
            category: SandboxRefusalCategory::Network,
        }
        .permits_no_unblock_paths());
        assert!(!RefusalKind::PromotionRefused.permits_no_unblock_paths());
        assert!(!RefusalKind::UncategorizedBlocker.permits_no_unblock_paths());
    }

    #[test]
    fn canonical_names_are_stable() {
        assert_eq!(
            RefusalKind::SandboxRefused {
                category: SandboxRefusalCategory::Filesystem,
            }
            .canonical_name(),
            "sandbox_refused"
        );
        assert_eq!(
            RefusalKind::ClinicalGateFailed.canonical_name(),
            "clinical_gate_failed"
        );
    }

    #[test]
    fn round_trips_through_serde() {
        let k = RefusalKind::SandboxRefused {
            category: SandboxRefusalCategory::SupplyChain,
        };
        let s = serde_json::to_string(&k).unwrap();
        let back: RefusalKind = serde_json::from_str(&s).unwrap();
        assert_eq!(k, back);

        let k2 = RefusalKind::ClinicalGateFailed;
        let s2 = serde_json::to_string(&k2).unwrap();
        let back2: RefusalKind = serde_json::from_str(&s2).unwrap();
        assert_eq!(k2, back2);
    }
}
