//! `ComposeOutcome` — typed result of a composition. Design §20.
//!
//! Every composer call returns exactly one of these variants.
//! `CompositionError` / `CompositionInfeasible` payloads map to
//! `Refusal` / `PartialDag` / `DraftDag` outcomes; this enum is the
//! unification layer across all composer paths.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::chain_of_custody::ChainOfCustody;
use super::evidence::AssumptionLedger;
use super::refusal_kind::RefusalKind;
use super::task_node::{TaskNode, WorkflowDag};
use super::unblock_path::UnblockPath;

/// Typed outcome of a composition. Every composer call returns
/// exactly one of these — never an unstructured "no valid DAG."
///
/// `Eq` not derived because `WorkflowDag` contains
/// `IterationDeclaration` with `f64`; round-trip equality tests
/// use serde snapshots instead.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ComposeOutcome {
    /// Successful production composition: validated, executable
    /// DAG with no unresolved assumptions blocking execution.
    ValidatedExecutableDag {
        /// Dag.
        dag: WorkflowDag,
        /// Report.
        report: ValidationReport,
    },
    /// Draft composition: the DAG is fully connected and
    /// internally consistent, but assumptions remain unresolved
    /// and/or some adapter classifies as `LossyDeclared` /
    /// `ScientificallyRisky`.
    DraftDag {
        /// Dag.
        dag: WorkflowDag,
        /// Assumptions.
        assumptions: AssumptionLedger,
        /// Blockers.
        blockers: Vec<BlockerContext>,
    },
    /// Partial composition: some required producer is absent or
    /// unresolvable. The drafted DAG covers what's available; the
    /// gap report enumerates unmet preconditions.
    PartialDag {
        /// Dag.
        dag: WorkflowDag,
        /// Unresolved gaps.
        unresolved_gaps: Vec<GapReport>,
    },
    /// Novel-node specification: the composer can't fulfill a
    /// goal from the existing registry and proposes a hypothesized
    /// node + the validation obligations a future implementation
    /// must satisfy. `node` is boxed because `TaskNode` is the
    /// largest variant and inflating the enum hurts cache
    /// locality on the success paths.
    NovelNodeSpec {
        /// Node.
        node: Box<TaskNode>,
        /// Required work.
        required_work: Vec<ValidationObligation>,
    },
    /// Composer refuses to produce a DAG. Refusal is typed (policy
    /// gate, regulatory block, missing license, etc.) so the UI
    /// can render the right card.
    Refusal { report: RefusalReport },
}

/// Evidence that a composition is internally consistent.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
pub struct ValidationReport {
    /// Atoms checked.
    pub nodes_checked: u32,
    /// Edges proven.
    pub edges_proven: u32,
    /// Whether all required validators are wired up and resolved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub validators_wired: bool,
    /// Whether all node versions are pinned.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub versions_pinned: bool,
    /// Whether all container digests are pinned (ADR 0025 contract).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub containers_pinned: bool,
    /// Free-form summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub summary: Option<String>,
    /// Chain-of-custody for the report when its summary or
    /// validator outputs carry suppressed content. `None` for ordinary
    /// validation reports. Older records without the field
    /// deserialize cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chain_of_custody: Option<ChainOfCustody>,
}

/// One blocker on a draft composition. Maps to UI `BlockerCard`
/// dispatch via the `BlockerKind` taxonomy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct BlockerContext {
    /// Stable id (so clients can dedupe / track resolution).
    pub id: String,
    /// Blocker kind (mirrors today's `BlockerKind` discriminator
    /// strings: `MissingReference`, `IncompatibleFormat`,
    /// `UnsetMethod`, `UnknownSemanticType`, etc.).
    pub kind: String,
    /// Human-readable statement.
    pub statement: String,
    /// Optional CTA suggestion (`amend_stage_method`, `rerun_task`,
    /// `confirm_assumption`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub suggested_action: Option<String>,
    /// Affected task node ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub affected_nodes: Vec<String>,
}

/// One unresolved gap in a partial composition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct GapReport {
    /// Stable id.
    pub id: String,
    /// What is missing (free text).
    pub statement: String,
    /// Required input port that has no producer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub missing_port: Option<String>,
    /// Suggested resolutions (e.g. SME upload, registry import).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggestions: Vec<String>,
}

/// Validation obligation a `NovelNodeSpec` must satisfy before
/// promoting toward production. Evaluated by the
/// claim_extractor/claim_verifier substrate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ValidationObligation {
    /// Stable id (`p_value_in_unit_interval`,
    /// `gene_id_in_annotation`, etc.).
    pub id: String,
    /// Obligation kind (`contract_test`, `golden_test`,
    /// `metamorphic_test`, `biological_invariant`,
    /// `statistical_sanity`, `reproducibility`, `security`,
    /// `resource_bound`).
    pub kind: String,
    /// Human-readable statement.
    pub statement: String,
    /// Optional reference (script path, fixture file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reference: Option<String>,
}

/// Refusal report — composer declines to produce a DAG.
///
/// `kind` is the typed `RefusalKind` enum; the report carries a
/// `Vec<UnblockPath>` enumerating the actionable recovery affordances
/// the SME can dispatch.
///
/// The invariant ("every refusal carries actionable unblock paths or
/// is unconditional hard policy") is enforced at the type boundary by
/// `RefusalReport::validate()`: hard-policy kinds
/// (`HardPolicyViolation`, `PhiLeakBlocked`, `PrivacyViolation`) may
/// carry an empty `unblock_paths` vector; every other kind must carry
/// at least one populated path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RefusalReport {
    /// Stable id.
    pub id: String,
    /// Typed refusal kind. Drives UI per-card dispatch and the F21
    /// invariant. See `RefusalKind` for the closed taxonomy.
    pub kind: RefusalKind,
    /// Human-readable explanation.
    pub statement: String,
    /// Optional pointers (policy ids, regulation citations).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<String>,
    /// Actionable recovery affordances. Empty only for
    /// `permits_no_unblock_paths()` kinds; every other kind is
    /// rejected by `validate()` when this vec is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unblock_paths: Vec<UnblockPath>,
}

impl RefusalReport {
    /// v3 P3 / v4 P3 F11/F19 — construct a `PromotionRefused` refusal
    /// for a DAG containing one or more nodes that failed the validation ×
    /// lifecycle promotion grid (`config/promotion-gate-policy.yaml`).
    ///
    /// `non_promotable_node_ids` names every node whose
    /// `PromotionDecision::Deny` triggered the refusal; `missing_summary`
    /// is a free-form rendering for the statement field; `unblock_paths`
    /// is populated by the planner's `synthesize_unblock_paths` once the
    /// kind has been classified.
    pub fn promotion_refused(
        non_promotable_node_ids: Vec<String>,
        missing_summary: impl Into<String>,
        unblock_paths: Vec<UnblockPath>,
    ) -> Self {
        let n = non_promotable_node_ids.len();
        let statement = format!(
            "{n} node{} failed the validation × lifecycle promotion grid: {}",
            if n == 1 { "" } else { "s" },
            missing_summary.into()
        );
        Self {
            id: "promotion_refused".into(),
            kind: RefusalKind::PromotionRefused,
            statement,
            references: non_promotable_node_ids,
            unblock_paths,
        }
    }

    /// v3 P9 §11.X — construct a `PopulationOutOfCoverage` refusal with
    /// the canonical statement string and the two recovery affordances
    /// (supply-cohort-metadata + escalate-to-waiver-authority).
    ///
    /// The same `unblock_paths` are synthesized by
    /// `composer_v4::planner::synthesize_unblock_paths` when the planner's
    /// refusal classifier produces a `PopulationOutOfCoverage` kind; this
    /// constructor is the explicit form callers outside the planner reach
    /// for.
    pub fn population_out_of_coverage(
        workflow_id: impl Into<String>,
        sample_label: impl Into<String>,
        validated_labels: Vec<String>,
        suggested_waiver_authority: impl Into<String>,
    ) -> Self {
        let workflow_id = workflow_id.into();
        let sample_label = sample_label.into();
        let authority = suggested_waiver_authority.into();
        let n = validated_labels.len();
        let statement = format!(
            "sample cohort `{sample_label}` is outside the validated cohort set for \
             workflow `{workflow_id}` ({n} validated cohort{}); a population waiver \
             from `{authority}` is required to proceed",
            if n == 1 { "" } else { "s" },
        );
        use super::unblock_path::ProjectedOutcome;
        let unblock_paths = vec![
            UnblockPath::SupplyMissingMetadata {
                field: "population".into(),
                suggested_value: None,
                target_outcome: ProjectedOutcome::DraftDag,
            },
            UnblockPath::EscalateToReviewer {
                reviewer_class: authority.clone(),
                required_artifacts: vec!["population_waiver_rationale".into()],
                target_outcome: ProjectedOutcome::DraftDag,
            },
        ];
        Self {
            id: format!("population_out_of_coverage:{workflow_id}"),
            kind: RefusalKind::PopulationOutOfCoverage {
                workflow_id,
                sample_label,
                validated_labels,
                suggested_waiver_authority: authority,
            },
            statement,
            references: Vec::new(),
            unblock_paths,
        }
    }

    /// F21 invariant: every non-hard refusal must carry at least one
    /// `UnblockPath`. Hard-policy refusals (clinical / PHI / privacy)
    /// are unconditional and permit zero paths because the SME's only
    /// recovery is to branch the session.
    pub fn validate(&self) -> Result<(), RefusalValidationError> {
        if self.unblock_paths.is_empty() && !self.kind.permits_no_unblock_paths() {
            return Err(RefusalValidationError::MissingUnblockPaths {
                kind: self.kind.canonical_name().to_string(),
            });
        }
        Ok(())
    }
}

/// Typed validation error for `RefusalReport::validate`.
///
/// Returned only by `validate()`; the constructor doesn't enforce the
/// invariant at construction time so the typed test harness can build
/// malformed reports and assert that `validate()` rejects them.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RefusalValidationError {
    /// A non-hard-policy refusal carried zero unblock paths.
    #[error("refusal of kind `{kind}` must carry at least one unblock_path")]
    MissingUnblockPaths {
        /// Canonical kind name (per `RefusalKind::canonical_name()`).
        kind: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_validated() {
        let o = ComposeOutcome::ValidatedExecutableDag {
            dag: WorkflowDag::default(),
            report: ValidationReport {
                nodes_checked: 5,
                edges_proven: 4,
                validators_wired: true,
                versions_pinned: true,
                containers_pinned: true,
                summary: Some("All checks passed".into()),
                chain_of_custody: None,
            },
        };
        let json = serde_json::to_string(&o).unwrap();
        let back: ComposeOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(o, back);
    }

    #[test]
    fn round_trip_refusal() {
        use super::super::unblock_path::{ProjectedOutcome, UnblockPath};
        let o = ComposeOutcome::Refusal {
            report: RefusalReport {
                id: "r1".into(),
                kind: RefusalKind::ClinicalGateFailed,
                statement: "Clinical workflow requires validated reference data".into(),
                references: vec!["policy:clinical_v3".into()],
                unblock_paths: vec![UnblockPath::EscalateToReviewer {
                    reviewer_class: "clinical_lead".into(),
                    required_artifacts: vec!["IRB_approval.pdf".into()],
                    target_outcome: ProjectedOutcome::DraftDag,
                }],
            },
        };
        let json = serde_json::to_string(&o).unwrap();
        let back: ComposeOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(o, back);
    }

    #[test]
    fn validate_hard_policy_allows_empty_unblock_paths() {
        let r = RefusalReport {
            id: "r1".into(),
            kind: RefusalKind::HardPolicyViolation,
            statement: "blocked".into(),
            references: vec![],
            unblock_paths: vec![],
        };
        assert!(r.validate().is_ok());
    }

    #[test]
    fn validate_clinical_gate_rejects_empty_unblock_paths() {
        let r = RefusalReport {
            id: "r1".into(),
            kind: RefusalKind::ClinicalGateFailed,
            statement: "blocked".into(),
            references: vec![],
            unblock_paths: vec![],
        };
        match r.validate() {
            Err(RefusalValidationError::MissingUnblockPaths { kind }) => {
                assert_eq!(kind, "clinical_gate_failed");
            }
            other => panic!("expected MissingUnblockPaths, got {:?}", other),
        }
    }

    #[test]
    fn round_trip_novel_node() {
        let o = ComposeOutcome::NovelNodeSpec {
            node: Box::new(super::super::task_node::TaskNode::skeleton(
                "propose_x",
                "Hypothesized node",
            )),
            required_work: vec![ValidationObligation {
                id: "v1".into(),
                kind: "contract_test".into(),
                statement: "Output must be in [0, 1]".into(),
                reference: None,
            }],
        };
        let yaml = serde_yml::to_string(&o).unwrap();
        let back: ComposeOutcome = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(o, back);
    }
}
