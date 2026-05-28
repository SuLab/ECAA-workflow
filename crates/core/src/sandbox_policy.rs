//! Generated-code sandbox policy.
//!
//! Mirrors design §17 exactly: static analysis, dependency
//! allowlist, container sandbox, no network by default, no
//! secrets mounted, no host filesystem access except workdir,
//! resource limits, timeouts, signed artifacts, human review for
//! high-risk nodes, output schema validation against
//! postconditions.
//!
//! The policy lives in `crates/core` so the composer can refuse
//! to lower a `GeneratedCode` implementation to `WORKFLOW.json`
//! before any harness work happens. The harness reads the same
//! policy at dispatch time to enforce the runtime constraints.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use ts_rs::TS;

use crate::sandbox_refusal_category::SandboxRefusalCategory;
use crate::workflow_contracts::implementation::{Implementation, ReviewStatus};
use crate::workflow_contracts::task_node::TaskNode;

/// Sandbox policy applied to generated-code implementations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct SandboxPolicy {
    /// Stable id for the policy (e.g. `default_v1`,
    /// `clinical_v1`).
    pub id: String,
    /// Human label.
    pub label: String,
    /// True when static analysis must pass before execution.
    pub require_static_analysis: bool,
    /// Allowed dependencies. Empty = empty-allowlist mode (no
    /// dependencies allowed at all). Use a wildcard policy
    /// (`vec!["*".into()]`) only for development environments.
    pub dependency_allowlist: BTreeSet<String>,
    /// True when the implementation must run in a container.
    pub require_container: bool,
    /// True when network access is blocked.
    pub deny_network: bool,
    /// True when secrets cannot be mounted.
    pub deny_secrets: bool,
    /// True when host filesystem access is blocked except for
    /// the explicit workdir.
    pub deny_host_fs: bool,
    /// Memory ceiling in MB (`None` = unlimited).
    pub memory_limit_mb: Option<u32>,
    /// Wall-clock timeout in seconds (`None` = unlimited).
    pub wall_timeout_secs: Option<u32>,
    /// True when artifacts must be signed.
    pub require_signed_artifacts: bool,
    /// True when high-risk implementations require explicit
    /// human review (separate from static analysis).
    pub require_human_review_for_high_risk: bool,
    /// True when output schema must be validated against the
    /// node's `postconditions` and any
    /// `RequiredArtifact.schema_ref`. Failure → `Blocked
    /// { ValidationFailed }` regardless of exit code.
    pub validate_output_schema: bool,
    /// Strict env-var allowlist. When non-empty, only these env var
    /// names are passed through to sandboxed subprocesses; everything
    /// else is `--unsetenv`'d. Independent of `deny_secrets` (which
    /// scrubs by name pattern). Empty default means pass-through.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow_envs: Vec<String>,
}

impl SandboxPolicy {
    /// Default strict.
    pub fn default_strict() -> Self {
        Self {
            id: "default_strict".into(),
            label: "Default strict sandbox".into(),
            require_static_analysis: true,
            dependency_allowlist: BTreeSet::new(),
            require_container: true,
            deny_network: true,
            deny_secrets: true,
            deny_host_fs: true,
            memory_limit_mb: Some(8192),
            wall_timeout_secs: Some(7200),
            require_signed_artifacts: true,
            require_human_review_for_high_risk: true,
            validate_output_schema: true,
            allow_envs: vec!["PATH".into(), "LANG".into(), "LC_ALL".into(), "TZ".into()],
        }
    }
}

/// Why a generated-code implementation was refused for
/// execution. Surfaces as `BlockerKind::SandboxRefused` in the
/// harness; `BlockerKind::ValidationFailed` for output-schema
/// failures.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SandboxRefusal {
    /// StaticAnalysisRequired variant.
    StaticAnalysisRequired,
    /// Variant.
    /// Field value.
    DependencyNotAllowed { dep: String },
    /// ContainerRequired variant.
    ContainerRequired,
    /// NetworkDenied variant.
    NetworkDenied,
    /// SecretsDenied variant.
    SecretsDenied,
    /// HostFsDenied variant.
    HostFsDenied,
    /// Variant.
    /// Field value.
    /// Field value.
    MemoryLimitExceeded { requested_mb: u32, limit_mb: u32 },
    /// WallTimeoutExceeded variant.
    WallTimeoutExceeded,
    /// SignedArtifactRequired variant.
    SignedArtifactRequired,
    /// HumanReviewRequired variant.
    HumanReviewRequired,
    /// ReviewStatusRejected variant.
    ReviewStatusRejected,
    /// Variant.
    /// Field value.
    OutputSchemaValidationFailed { reason: String },
}

impl SandboxRefusal {
    /// Stable discriminator string for the typed UI BlockerKind::SandboxRefused
    /// dispatch. One per variant; never internationalized.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::StaticAnalysisRequired => "StaticAnalysisRequired",
            Self::DependencyNotAllowed { .. } => "DependencyNotAllowed",
            Self::ContainerRequired => "ContainerRequired",
            Self::NetworkDenied => "NetworkDenied",
            Self::SecretsDenied => "SecretsDenied",
            Self::HostFsDenied => "HostFsDenied",
            Self::MemoryLimitExceeded { .. } => "MemoryLimitExceeded",
            Self::WallTimeoutExceeded => "WallTimeoutExceeded",
            Self::SignedArtifactRequired => "SignedArtifactRequired",
            Self::HumanReviewRequired => "HumanReviewRequired",
            Self::ReviewStatusRejected => "ReviewStatusRejected",
            Self::OutputSchemaValidationFailed { .. } => "OutputSchemaValidationFailed",
        }
    }

    /// Short one-line human-readable detail (no newlines). Used to
    /// build the [sandbox_refused] reason string the harness emits
    /// and the UI parses.
    pub fn detail(&self) -> String {
        match self {
            Self::StaticAnalysisRequired => String::new(),
            Self::DependencyNotAllowed { dep } => format!("dependency={dep}"),
            Self::ContainerRequired => String::new(),
            Self::NetworkDenied => String::new(),
            Self::SecretsDenied => String::new(),
            Self::HostFsDenied => String::new(),
            Self::MemoryLimitExceeded {
                requested_mb,
                limit_mb,
            } => format!("requested_mb={requested_mb} limit_mb={limit_mb}"),
            Self::WallTimeoutExceeded => String::new(),
            Self::SignedArtifactRequired => String::new(),
            Self::HumanReviewRequired => String::new(),
            Self::ReviewStatusRejected => String::new(),
            Self::OutputSchemaValidationFailed { reason } => reason.clone(),
        }
    }

    /// V4 projection onto the 7-element categorical axis
    /// (`SandboxRefusalCategory`). Drives UI dispatch (BlockerCard
    /// renders category-grouped recovery hints) and Tier 10.3
    /// statistics aggregation. Every variant maps to exactly one
    /// category; total function, no `_ => _` catch-all so the
    /// compiler forces an explicit category choice on every future
    /// variant addition.
    pub fn category(&self) -> SandboxRefusalCategory {
        match self {
            Self::NetworkDenied => SandboxRefusalCategory::Network,
            Self::HostFsDenied => SandboxRefusalCategory::Filesystem,
            Self::MemoryLimitExceeded { .. } => SandboxRefusalCategory::Resource,
            Self::WallTimeoutExceeded => SandboxRefusalCategory::Resource,
            Self::SecretsDenied => SandboxRefusalCategory::Identity,
            Self::HumanReviewRequired => SandboxRefusalCategory::Identity,
            Self::ReviewStatusRejected => SandboxRefusalCategory::Identity,
            Self::StaticAnalysisRequired => SandboxRefusalCategory::Capability,
            Self::DependencyNotAllowed { .. } => SandboxRefusalCategory::SupplyChain,
            Self::ContainerRequired => SandboxRefusalCategory::SupplyChain,
            Self::SignedArtifactRequired => SandboxRefusalCategory::SupplyChain,
            Self::OutputSchemaValidationFailed { .. } => SandboxRefusalCategory::OutputValidation,
        }
    }
}

/// Sweep an entire
/// `WorkflowDag` and collect every per-node sandbox refusal.
/// Returns the refusals keyed by node id. Empty = ready for
/// promotion to ValidatedExecutableDag.
///
/// The v4 planner calls this before lowering a candidate composition
/// to `ComposeOutcome::ValidatedExecutableDag`; if any
/// node fails, the planner downgrades to
/// `ComposeOutcome::DraftDag` with the refusals attached as
/// `BlockerContext`s. The harness re-runs the check at dispatch
/// time as a defense in depth — a node that drifts from
/// HumanReviewed back to Unreviewed mid-flight is caught at
/// runtime, not just at composition.
pub fn check_workflow_dag(
    dag: &crate::workflow_contracts::task_node::WorkflowDag,
    policy: &SandboxPolicy,
) -> Vec<(String, SandboxRefusal)> {
    let mut all: Vec<(String, SandboxRefusal)> = Vec::new();
    let mut sorted: Vec<&TaskNode> = dag.nodes.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));
    for node in sorted {
        for refusal in check_generated_code_node(node, policy) {
            all.push((node.id.clone(), refusal));
        }
    }
    all
}

/// Check whether a `TaskNode` with `Implementation::GeneratedCode`
/// satisfies the policy. Returns `Vec<SandboxRefusal>`; empty means
/// approved.
pub fn check_generated_code_node(node: &TaskNode, policy: &SandboxPolicy) -> Vec<SandboxRefusal> {
    let mut refusals = Vec::new();

    let review_status = match &node.implementation {
        Implementation::GeneratedCode { review_status, .. } => *review_status,
        _ => return refusals, // Not a generated-code node; nothing to check.
    };

    match review_status {
        ReviewStatus::Rejected => {
            refusals.push(SandboxRefusal::ReviewStatusRejected);
        }
        ReviewStatus::Unreviewed => {
            if policy.require_static_analysis {
                refusals.push(SandboxRefusal::StaticAnalysisRequired);
            }
        }
        ReviewStatus::AutoReviewed => {
            // Static analysis passed; human review may still be
            // required for high-risk nodes.
            if policy.require_human_review_for_high_risk
                && matches!(
                    node.risk,
                    crate::workflow_contracts::evidence::RiskClass::High
                        | crate::workflow_contracts::evidence::RiskClass::Clinical
                )
            {
                refusals.push(SandboxRefusal::HumanReviewRequired);
            }
        }
        ReviewStatus::HumanReviewed => {
            // Best case — no refusals from the review-status axis.
        }
    }

    if policy.require_container {
        // GeneratedCode currently has no container field; the
        // harness must wrap it in a container before dispatch.
        // The check is "if not yet container-wrapped" — at this
        // layer we record the requirement so the harness honors
        // it. A future pass will tighten this when the
        // container-wrapping path lands.
        // Intentionally not refused here.
    }

    refusals
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::evidence::RiskClass;
    use crate::workflow_contracts::task_node::TaskNode;

    fn generated_node(status: ReviewStatus) -> TaskNode {
        let mut n = TaskNode::skeleton("g", "Generated");
        n.implementation = Implementation::GeneratedCode {
            repository_ref: "git@example.com/repo".into(),
            review_status: status,
            artifact_digest: Some("sha256:abc".into()),
        };
        n
    }

    #[test]
    fn rejected_review_blocks_execution() {
        let node = generated_node(ReviewStatus::Rejected);
        let refusals = check_generated_code_node(&node, &SandboxPolicy::default_strict());
        assert!(refusals.contains(&SandboxRefusal::ReviewStatusRejected));
    }

    #[test]
    fn unreviewed_blocks_when_static_analysis_required() {
        let node = generated_node(ReviewStatus::Unreviewed);
        let refusals = check_generated_code_node(&node, &SandboxPolicy::default_strict());
        assert!(refusals.contains(&SandboxRefusal::StaticAnalysisRequired));
    }

    #[test]
    fn auto_reviewed_high_risk_requires_human() {
        let mut node = generated_node(ReviewStatus::AutoReviewed);
        node.risk = RiskClass::High;
        let refusals = check_generated_code_node(&node, &SandboxPolicy::default_strict());
        assert!(refusals.contains(&SandboxRefusal::HumanReviewRequired));
    }

    #[test]
    fn auto_reviewed_low_risk_passes() {
        let node = generated_node(ReviewStatus::AutoReviewed);
        let refusals = check_generated_code_node(&node, &SandboxPolicy::default_strict());
        assert!(refusals.is_empty());
    }

    #[test]
    fn human_reviewed_passes_clean() {
        let node = generated_node(ReviewStatus::HumanReviewed);
        let refusals = check_generated_code_node(&node, &SandboxPolicy::default_strict());
        assert!(refusals.is_empty());
    }

    #[test]
    fn non_generated_node_ignored() {
        let node = TaskNode::skeleton("x", "");
        let refusals = check_generated_code_node(&node, &SandboxPolicy::default_strict());
        assert!(refusals.is_empty());
    }

    #[test]
    fn default_strict_policy_round_trips() {
        let policy = SandboxPolicy::default_strict();
        let json = serde_json::to_string(&policy).unwrap();
        let back: SandboxPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(policy, back);
    }

    #[test]
    fn refusal_round_trips() {
        let r = SandboxRefusal::DependencyNotAllowed {
            dep: "tensorflow".into(),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: SandboxRefusal = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn check_workflow_dag_collects_refusals_per_node() {
        // DAG-wide sweep returns refusals keyed by
        // node id. Stable ordering for replay.
        use crate::workflow_contracts::task_node::WorkflowDag;
        let mut unreviewed = generated_node(ReviewStatus::Unreviewed);
        unreviewed.id = "unreviewed_node".into();
        let mut rejected = generated_node(ReviewStatus::Rejected);
        rejected.id = "rejected_node".into();
        let dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![rejected, unreviewed, TaskNode::skeleton("plain", "")],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };
        let refusals = check_workflow_dag(&dag, &SandboxPolicy::default_strict());
        assert_eq!(refusals.len(), 2);
        // Stable sorted-by-id order.
        assert_eq!(refusals[0].0, "rejected_node");
        assert_eq!(refusals[1].0, "unreviewed_node");
    }

    #[test]
    fn allow_envs_field_round_trips() {
        let mut policy = SandboxPolicy::default_strict();
        policy.allow_envs = vec!["PATH".into(), "LANG".into()];
        let json = serde_json::to_string(&policy).unwrap();
        let back: SandboxPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(back.allow_envs, vec!["PATH", "LANG"]);
    }

    #[test]
    fn allow_envs_empty_omitted_in_json() {
        let mut policy = SandboxPolicy::default_strict();
        policy.allow_envs = Vec::new();
        let json = serde_json::to_string(&policy).unwrap();
        // Empty vec must be skipped (skip_serializing_if = "Vec::is_empty").
        assert!(
            !json.contains("allow_envs"),
            "empty allow_envs must be omitted from JSON, got: {}",
            json
        );
    }

    #[test]
    fn default_strict_has_minimum_allowlist() {
        let policy = SandboxPolicy::default_strict();
        for name in &["PATH", "LANG", "LC_ALL", "TZ"] {
            assert!(
                policy.allow_envs.iter().any(|e| e == name),
                "default_strict().allow_envs must include {name}"
            );
        }
    }

    #[test]
    fn check_workflow_dag_empty_when_all_human_reviewed() {
        use crate::workflow_contracts::task_node::WorkflowDag;
        let mut a = generated_node(ReviewStatus::HumanReviewed);
        a.id = "a".into();
        let mut b = generated_node(ReviewStatus::HumanReviewed);
        b.id = "b".into();
        let dag = WorkflowDag {
            id: "ok".into(),
            nodes: vec![a, b],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };
        let refusals = check_workflow_dag(&dag, &SandboxPolicy::default_strict());
        assert!(refusals.is_empty());
    }

    #[test]
    fn every_refusal_kind_has_a_category() {
        // V4 total projection. Construct one instance of each
        // Variant; call.category(); assert the expected category. The
        // exhaustive list forces a compile error when a new
        // `SandboxRefusal` variant is added without updating
        // `category()` (since the impl uses an explicit match with no
        // catch-all).
        let cases = [
            (
                SandboxRefusal::NetworkDenied,
                SandboxRefusalCategory::Network,
            ),
            (
                SandboxRefusal::HostFsDenied,
                SandboxRefusalCategory::Filesystem,
            ),
            (
                SandboxRefusal::MemoryLimitExceeded {
                    requested_mb: 0,
                    limit_mb: 0,
                },
                SandboxRefusalCategory::Resource,
            ),
            (
                SandboxRefusal::WallTimeoutExceeded,
                SandboxRefusalCategory::Resource,
            ),
            (
                SandboxRefusal::SecretsDenied,
                SandboxRefusalCategory::Identity,
            ),
            (
                SandboxRefusal::HumanReviewRequired,
                SandboxRefusalCategory::Identity,
            ),
            (
                SandboxRefusal::ReviewStatusRejected,
                SandboxRefusalCategory::Identity,
            ),
            (
                SandboxRefusal::StaticAnalysisRequired,
                SandboxRefusalCategory::Capability,
            ),
            (
                SandboxRefusal::DependencyNotAllowed { dep: "x".into() },
                SandboxRefusalCategory::SupplyChain,
            ),
            (
                SandboxRefusal::ContainerRequired,
                SandboxRefusalCategory::SupplyChain,
            ),
            (
                SandboxRefusal::SignedArtifactRequired,
                SandboxRefusalCategory::SupplyChain,
            ),
            (
                SandboxRefusal::OutputSchemaValidationFailed { reason: "x".into() },
                SandboxRefusalCategory::OutputValidation,
            ),
        ];
        for (refusal, expected_category) in cases {
            assert_eq!(
                refusal.category(),
                expected_category,
                "unexpected category for {refusal:?}"
            );
        }
    }
}
