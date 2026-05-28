//! Per-node policy checks wired into the v4 planner.
//!
//! The compatibility engine evaluates *edge-relevant* policies
//! (refuses risky adapters under clinical bundles, records
//! audit-trail decisions on edges). This module covers the
//! *per-node* check set: `ValidatedNodesOnly`,
//! `RequirePinnedContainers`, `NoGeneratedCode`, `NoNetwork`,
//! `PinnedReferenceDataOnly`. These run after the planner produces a
//! `WorkflowDag` and before the dispatch lowering.
//!
//! Output: a `PolicyEvaluation` carrying any per-node violations.
//! The planner consults this to decide whether to downgrade
//! `ValidatedExecutableDag` → `Refusal` / `DraftDag`.

use crate::policy_context::{PolicyCheck, PolicyCheckKind, PolicyContext};
use crate::promotion_gate_policy::{PassingClassCounts, PromotionDecision};
use crate::workflow_contracts::lifecycle::LifecycleState;
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

use super::PlanningContext;

/// Per-node policy violation.
#[derive(Debug, Clone)]
pub struct PolicyViolation {
    /// Node id the violation applies to.
    pub node_id: String,
    /// Policy check kind that failed.
    pub check_kind: PolicyCheckKind,
    /// Human-readable rationale.
    pub statement: String,
    /// Whether the failure blocks production execution. Today every
    /// per-node check is treated as blocking when present in an
    /// active bundle; soft checks (audit-trail / human-signoff) are
    /// recorded separately.
    pub blocking: bool,
}

/// Outcome of evaluating per-node policies against a `WorkflowDag`.
#[derive(Debug, Clone, Default)]
pub struct PolicyEvaluation {
    /// Violations.
    pub violations: Vec<PolicyViolation>,
    /// Decisions recorded as informational (audit-trail / human-
    /// signoff metadata). One entry per active bundle.
    pub recorded_decisions: Vec<String>,
}

impl PolicyEvaluation {
    /// Has blocking violations.
    pub fn has_blocking_violations(&self) -> bool {
        self.violations.iter().any(|v| v.blocking)
    }
}

/// Evaluate every per-node policy check declared in `policy` against
/// every node in `dag`. Returns one `PolicyViolation` per failure,
/// sorted by `(node_id, check_kind)` for byte-stable replay.
pub fn evaluate(policy: &PolicyContext, dag: &WorkflowDag) -> PolicyEvaluation {
    let mut violations: Vec<PolicyViolation> = Vec::new();
    let mut recorded: Vec<String> = Vec::new();

    for (bundle_id, check) in policy.iter_checks() {
        for node in &dag.nodes {
            if let Some(v) = evaluate_one(check, node) {
                violations.push(v);
            }
        }
        match check.kind() {
            PolicyCheckKind::AuditTrailRequired | PolicyCheckKind::HumanSignoffRequired => {
                let rec = format!("{}: {}", bundle_id, format_check_kind(&check.kind()));
                if !recorded.contains(&rec) {
                    recorded.push(rec);
                }
            }
            _ => {}
        }
    }
    violations.sort_by(|a, b| {
        a.node_id
            .cmp(&b.node_id)
            .then_with(|| format_check_kind(&a.check_kind).cmp(&format_check_kind(&b.check_kind)))
    });
    PolicyEvaluation {
        violations,
        recorded_decisions: recorded,
    }
}

fn evaluate_one(check: &PolicyCheck, node: &TaskNode) -> Option<PolicyViolation> {
    let blocking = true;
    match check {
        PolicyCheck::ValidatedNodesOnly => {
            let ok = matches!(
                node.lifecycle_state,
                LifecycleState::Production
                    | LifecycleState::BenchmarkValidated
                    | LifecycleState::LocallyValidated
            );
            if !ok {
                return Some(PolicyViolation {
                    node_id: node.id.clone(),
                    check_kind: check.kind(),
                    statement: format!(
                        "node {} lifecycle is {:?} — policy requires Production / \
                         BenchmarkValidated / LocallyValidated",
                        node.id, node.lifecycle_state
                    ),
                    blocking,
                });
            }
        }
        PolicyCheck::RequirePinnedContainers => {
            if let crate::workflow_contracts::implementation::Implementation::ContainerCommand {
                image,
                ..
            } = &node.implementation
            {
                if image.digest.is_empty() {
                    return Some(PolicyViolation {
                        node_id: node.id.clone(),
                        check_kind: check.kind(),
                        statement: format!(
                            "node {} container image {}/{} has no digest pin",
                            node.id, image.image, image.tag
                        ),
                        blocking,
                    });
                }
            }
        }
        PolicyCheck::NoGeneratedCode => {
            if matches!(
                node.implementation,
                crate::workflow_contracts::implementation::Implementation::GeneratedCode { .. }
            ) {
                return Some(PolicyViolation {
                    node_id: node.id.clone(),
                    check_kind: check.kind(),
                    statement: format!(
                        "node {} uses GeneratedCode — refused under active policy",
                        node.id
                    ),
                    blocking,
                });
            }
        }
        PolicyCheck::NoNetwork => {
            let net_ok = node
                .attributes
                .get("preferred_container")
                .and_then(|c| c.get("network"))
                .and_then(|n| n.get("kind"))
                .and_then(|k| k.as_str())
                .map(|s| s == "none")
                .unwrap_or(false);
            if !net_ok
                && matches!(
                    node.implementation,
                    crate::workflow_contracts::implementation::Implementation::ContainerCommand { .. }
                )
            {
                return Some(PolicyViolation {
                    node_id: node.id.clone(),
                    check_kind: check.kind(),
                    statement: format!("node {} container does not declare network: none", node.id),
                    blocking,
                });
            }
        }
        PolicyCheck::PinnedReferenceDataOnly => {
            let pinned = node
                .attributes
                .get("pinned_reference")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !pinned {
                return Some(PolicyViolation {
                    node_id: node.id.clone(),
                    check_kind: check.kind(),
                    statement: format!("node {} does not declare pinned reference data", node.id),
                    blocking,
                });
            }
        }
        // Edge-relevant checks already evaluated by the
        // compatibility engine — no per-node component here.
        PolicyCheck::NoPolicyRestrictedAdapters
        | PolicyCheck::NoScientificallyRiskyAdapters
        | PolicyCheck::NoPrivacyWidening => {}
        // Soft / informational checks — surface recorded decisions
        // rather than per-node violations.
        PolicyCheck::AuditTrailRequired | PolicyCheck::HumanSignoffRequired => {}
        // Site-local: pass-through (composer treats as warning).
        PolicyCheck::SiteLocal { .. } => {}
    }
    None
}

fn format_check_kind(k: &PolicyCheckKind) -> String {
    match k {
        PolicyCheckKind::NoScientificallyRiskyAdapters => "no_scientifically_risky_adapters",
        PolicyCheckKind::NoPolicyRestrictedAdapters => "no_policy_restricted_adapters",
        PolicyCheckKind::NoPrivacyWidening => "no_privacy_widening",
        PolicyCheckKind::AuditTrailRequired => "audit_trail_required",
        PolicyCheckKind::HumanSignoffRequired => "human_signoff_required",
        PolicyCheckKind::ValidatedNodesOnly => "validated_nodes_only",
        PolicyCheckKind::RequirePinnedContainers => "require_pinned_containers",
        PolicyCheckKind::NoGeneratedCode => "no_generated_code",
        PolicyCheckKind::NoNetwork => "no_network",
        PolicyCheckKind::PinnedReferenceDataOnly => "pinned_reference_data_only",
        PolicyCheckKind::SiteLocal => "site_local",
    }
    .to_string()
}

/// V4 alignment replace the previously hard-coded
/// "node has full validation" check with a call against
/// `PlanningContext.promotion_gate` (loaded from
/// `config/promotion-gate-policy.yaml`).
///
/// Returns `PromotionDecision::Allow` when the grid is satisfied for
/// the candidate `(node, target)` pair, `PromotionDecision::Deny` (with
/// the per-class miss list + per-credential-class miss list) otherwise.
/// When `ctx.promotion_gate` is `None`, the gate is short-circuited to
/// `Allow` for back-compat with sessions that haven't migrated to the
/// policy-aware constructor (legacy ad-hoc behaviour); F19 callers wire
/// the policy explicitly so the short-circuit only fires for
/// historical-session deserialization.
///
/// Side-effect: every consult records a
/// `VerifierDecision::PromotionGateConsulted` row in the decision
/// substrate so the lookup is replayable from
/// `runtime/verifier-decisions.jsonl`.
pub fn consult_promotion_gate(
    node: &TaskNode,
    target: LifecycleState,
    ctx: &PlanningContext,
) -> PromotionDecision {
    let counts = count_passing_validators(node);
    let approvals: Vec<String> = node
        .evidence
        .passed_validators
        .iter()
        .filter(|v| v.id.starts_with("approval:"))
        .map(|v| v.id.trim_start_matches("approval:").to_string())
        .collect();

    let policy_arc = ctx.promotion_gate.clone();
    let decision = match policy_arc.as_ref() {
        Some(policy) => policy.consult(&target, &counts, &approvals),
        None => PromotionDecision::Allow,
    };

    let (result_str, required, passing, missing) = match policy_arc.as_ref() {
        Some(policy) => {
            let required = policy.required_class_names(&target);
            let passing = passing_class_names(&counts);
            match &decision {
                PromotionDecision::Allow => ("allow".to_string(), required, passing, Vec::new()),
                PromotionDecision::Deny {
                    missing_classes,
                    missing_approvals,
                } => {
                    let mut missing = missing_classes.clone();
                    for a in missing_approvals {
                        missing.push(format!("approval:{a}"));
                    }
                    ("deny".to_string(), required, passing, missing)
                }
            }
        }
        None => (
            "allow_no_policy".to_string(),
            Vec::new(),
            passing_class_names(&counts),
            Vec::new(),
        ),
    };

    crate::decision_substrate::record(
        crate::decision_substrate::VerifierDecision::PromotionGateConsulted {
            id: crate::decision_substrate::stable_id(
                "promotion_gate",
                &node.id,
                target.canonical_name(),
            ),
            timestamp: crate::decision_substrate::timestamp(),
            node_id: node.id.clone(),
            target_state: target.canonical_name().to_string(),
            result: result_str,
            required_classes: required,
            passing_classes: passing,
            missing_classes: missing,
        },
    );

    decision
}

/// Classify each `passed_validator` on the node into one of the six
/// §18 validation classes by id prefix. The id convention is
/// `<class>:<rest>` (e.g. `contract:p_value_in_unit_interval`,
/// `golden:rnaseq_grch38_v1`). Validators that don't match a class
/// prefix are not counted toward any bucket (and the existing
/// `claim_verifier` validators continue to be counted as `contract`
/// validators per the migration default).
fn count_passing_validators(node: &TaskNode) -> PassingClassCounts {
    let mut c = PassingClassCounts::default();
    for v in &node.evidence.passed_validators {
        match classify_validator_id(&v.id) {
            Some("contract") => c.contract += 1,
            Some("golden") => c.golden += 1,
            Some("metamorphic") => c.metamorphic += 1,
            Some("biological_invariant") => c.biological_invariant += 1,
            Some("statistical_sanity") => c.statistical_sanity += 1,
            Some("reproducibility") => c.reproducibility += 1,
            _ => {
                // Pre-F19 validators (no class prefix on the id) keep
                // working: they count as `contract` validators since
                // claim_verifier ran at compose time. Skip the
                // `approval:*` prefix which is a credential record.
                if !v.id.starts_with("approval:") {
                    c.contract += 1;
                }
            }
        }
    }
    c
}

fn classify_validator_id(id: &str) -> Option<&str> {
    let class = id.split(':').next()?;
    match class {
        "contract"
        | "golden"
        | "metamorphic"
        | "biological_invariant"
        | "statistical_sanity"
        | "reproducibility" => Some(class),
        _ => None,
    }
}

fn passing_class_names(counts: &PassingClassCounts) -> Vec<String> {
    let mut names = Vec::new();
    if counts.contract > 0 {
        names.push("contract".into());
    }
    if counts.golden > 0 {
        names.push("golden".into());
    }
    if counts.metamorphic > 0 {
        names.push("metamorphic".into());
    }
    if counts.biological_invariant > 0 {
        names.push("biological_invariant".into());
    }
    if counts.statistical_sanity > 0 {
        names.push("statistical_sanity".into());
    }
    if counts.reproducibility > 0 {
        names.push("reproducibility".into());
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy_context::PolicyContext;
    use crate::workflow_contracts::implementation::{Implementation, OciImageRef};

    fn unpinned_node() -> TaskNode {
        let mut n = TaskNode::skeleton("align_reads", "Align");
        n.implementation = Implementation::ContainerCommand {
            image: OciImageRef {
                image: "ghcr.io/x/y".into(),
                tag: "latest".into(),
                digest: "".into(),
                arch: vec!["amd64".into()],
                gpu: false,
            },
            command_template: vec![],
        };
        n
    }

    #[test]
    fn unpinned_container_violates_clinical_bundle() {
        let policy = PolicyContext::empty().with_bundle(PolicyContext::clinical_trial_bundle());
        let dag = WorkflowDag {
            id: "x".into(),
            nodes: vec![unpinned_node()],
            ..Default::default()
        };
        let eval = evaluate(&policy, &dag);
        // ValidatedNodesOnly + RequirePinnedContainers + NoNetwork +
        // PinnedReferenceDataOnly all fail for a default skeleton
        // node; assert at least the expected check fired.
        assert!(eval
            .violations
            .iter()
            .any(|v| v.check_kind == PolicyCheckKind::RequirePinnedContainers));
        assert!(eval.has_blocking_violations());
    }
}
