//! Promotion gates for `TaskNode` lifecycle advancement.
//!
//! Validates whether a `TaskNode` is allowed to advance to a
//! given `LifecycleState`. Each transition has a typed precondition
//! check; failures return an `IneligibleReason` enumerating
//! exactly which evidence was missing.
//!
//! The `propose_hypothesized_node` tool uses this to reject SME
//! promotion of an unverified node into production. Validator runs
//! feed the `LocallyValidated` / `BenchmarkValidated` gates; sandbox
//! proofs gate the `GeneratedCode` path.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::evidence::RiskClass;
use super::implementation::{Implementation, ReviewStatus};
use super::lifecycle::{LifecycleState, TrustLevel};
use super::task_node::TaskNode;

/// Why a node is ineligible for a target lifecycle state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IneligibleReason {
    /// Backward / no-op transition.
    InvalidTransition {
        /// From.
        from: LifecycleState,
        /// To.
        to: LifecycleState,
    },
    /// `LocallyValidatedNode` requires at least one passed validator.
    NoLocalValidators,
    /// `BenchmarkValidatedNode` requires at least one benchmark
    /// in the evidence set.
    NoBenchmarks,
    /// `ProductionNode` requires the implementation to be a
    /// container with a pinned digest.
    ContainerDigestMissing,
    /// `ProductionNode` requires `Reviewed` or stronger trust.
    InsufficientTrust {
        /// Required.
        required: TrustLevel,
        /// Current.
        current: TrustLevel,
    },
    /// `ProductionNode` for `RiskClass::Clinical` requires
    /// `DualReviewed`.
    DualReviewRequired,
    /// `GeneratedCode` implementation requires `HumanReviewed`
    /// review status before promotion.
    GeneratedCodeUnreviewed,
    /// `Deprecated` nodes can't be re-promoted.
    NodeIsDeprecated,
    /// `Unimplemented` implementations can't reach the
    /// `Implemented`+ band.
    ImplementationMissing,
}

/// Result of a promotion check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromotionDecision {
    /// Transition allowed.
    Eligible {
        /// From.
        from: LifecycleState,
        /// To.
        to: LifecycleState,
    },
    /// Transition blocked. `reasons` enumerates every missing
    /// piece of evidence so the SME can act on all of them at once.
    Blocked {
        /// From.
        from: LifecycleState,
        /// To.
        to: LifecycleState,
        /// Reasons.
        reasons: Vec<IneligibleReason>,
    },
}

impl PromotionDecision {
    /// Is eligible.
    pub fn is_eligible(&self) -> bool {
        matches!(self, PromotionDecision::Eligible { .. })
    }
}

/// Check whether `node` may transition from its current
/// `lifecycle_state` to `target`.
pub fn can_promote(node: &TaskNode, target: LifecycleState) -> PromotionDecision {
    let current = node.lifecycle_state;
    if current == target {
        return PromotionDecision::Blocked {
            from: current,
            to: target,
            reasons: vec![IneligibleReason::InvalidTransition {
                from: current,
                to: target,
            }],
        };
    }
    if current == LifecycleState::Deprecated {
        return PromotionDecision::Blocked {
            from: current,
            to: target,
            reasons: vec![IneligibleReason::NodeIsDeprecated],
        };
    }
    // Forward-only ordering — except Deprecated is reachable from
    // any non-Deprecated state.
    if target != LifecycleState::Deprecated && target.order() <= current.order() {
        return PromotionDecision::Blocked {
            from: current,
            to: target,
            reasons: vec![IneligibleReason::InvalidTransition {
                from: current,
                to: target,
            }],
        };
    }

    let mut reasons: Vec<IneligibleReason> = Vec::new();

    match target {
        LifecycleState::Hypothesized | LifecycleState::Contracted => {
            // Always reachable forward; nothing to check beyond
            // ordering above.
        }
        LifecycleState::Implemented => {
            if matches!(node.implementation, Implementation::Unimplemented) {
                reasons.push(IneligibleReason::ImplementationMissing);
            }
        }
        LifecycleState::LocallyValidated => {
            if matches!(node.implementation, Implementation::Unimplemented) {
                reasons.push(IneligibleReason::ImplementationMissing);
            }
            if node.evidence.passed_validators.is_empty() {
                reasons.push(IneligibleReason::NoLocalValidators);
            }
        }
        LifecycleState::BenchmarkValidated => {
            if matches!(node.implementation, Implementation::Unimplemented) {
                reasons.push(IneligibleReason::ImplementationMissing);
            }
            if node.evidence.passed_validators.is_empty() {
                reasons.push(IneligibleReason::NoLocalValidators);
            }
            if node.evidence.benchmarks.is_empty() {
                reasons.push(IneligibleReason::NoBenchmarks);
            }
        }
        LifecycleState::Production => {
            if matches!(node.implementation, Implementation::Unimplemented) {
                reasons.push(IneligibleReason::ImplementationMissing);
            }
            if node.evidence.passed_validators.is_empty() {
                reasons.push(IneligibleReason::NoLocalValidators);
            }
            // Container digest pin required for production.
            match &node.implementation {
                Implementation::ContainerCommand { image, .. } if image.digest.is_empty() => {
                    reasons.push(IneligibleReason::ContainerDigestMissing);
                }
                Implementation::GeneratedCode { review_status, .. } => {
                    if !matches!(review_status, ReviewStatus::HumanReviewed) {
                        reasons.push(IneligibleReason::GeneratedCodeUnreviewed);
                    }
                }
                _ => {}
            }
            // Trust level requirement: Reviewed for non-clinical,
            // DualReviewed for clinical.
            let required_trust = if matches!(node.risk, RiskClass::Clinical) {
                TrustLevel::DualReviewed
            } else {
                TrustLevel::Reviewed
            };
            if node.trust_level.order() < required_trust.order() {
                if matches!(node.risk, RiskClass::Clinical)
                    && !matches!(node.trust_level, TrustLevel::DualReviewed)
                {
                    reasons.push(IneligibleReason::DualReviewRequired);
                } else {
                    reasons.push(IneligibleReason::InsufficientTrust {
                        required: required_trust,
                        current: node.trust_level,
                    });
                }
            }
        }
        LifecycleState::Deprecated => {
            // Anything can become deprecated.
        }
    }

    if reasons.is_empty() {
        PromotionDecision::Eligible {
            from: current,
            to: target,
        }
    } else {
        PromotionDecision::Blocked {
            from: current,
            to: target,
            reasons,
        }
    }
}

/// Convenience trait for `TrustLevel` so the gate can compare
/// across levels deterministically.
pub trait TrustLevelOrder {
    /// Order.
    fn order(&self) -> u8;
}

impl TrustLevelOrder for TrustLevel {
    fn order(&self) -> u8 {
        match self {
            TrustLevel::Unverified => 0,
            TrustLevel::StaticChecked => 1,
            TrustLevel::Reviewed => 2,
            TrustLevel::DualReviewed => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::evidence::ValidatorRef;
    use crate::workflow_contracts::implementation::OciImageRef;

    fn productionable_node() -> TaskNode {
        let mut n = TaskNode::skeleton("foo", "");
        n.implementation = Implementation::ContainerCommand {
            image: OciImageRef {
                image: "ghcr.io/x".into(),
                tag: "v1".into(),
                digest: "sha256:abc".into(),
                arch: vec!["amd64".into()],
                gpu: false,
            },
            command_template: vec![],
        };
        n.evidence.passed_validators.push(ValidatorRef {
            id: "v1".into(),
            version: None,
            parameters: None,
        });
        n.evidence.benchmarks.push("benchmark_x".into());
        n.trust_level = TrustLevel::Reviewed;
        n.lifecycle_state = LifecycleState::BenchmarkValidated;
        n
    }

    #[test]
    fn deprecated_blocks_re_promotion() {
        let mut n = productionable_node();
        n.lifecycle_state = LifecycleState::Deprecated;
        let d = can_promote(&n, LifecycleState::Production);
        match d {
            PromotionDecision::Blocked { reasons, .. } => {
                assert!(reasons.contains(&IneligibleReason::NodeIsDeprecated));
            }
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn backward_transition_blocked() {
        let mut n = productionable_node();
        n.lifecycle_state = LifecycleState::LocallyValidated;
        let d = can_promote(&n, LifecycleState::Contracted);
        match d {
            PromotionDecision::Blocked { reasons, .. } => assert!(reasons
                .iter()
                .any(|r| matches!(r, IneligibleReason::InvalidTransition { .. }))),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn unimplemented_blocks_implemented() {
        let n = TaskNode::skeleton("x", "");
        // Default lifecycle is Contracted, default implementation is
        // Unimplemented.
        let d = can_promote(&n, LifecycleState::Implemented);
        match d {
            PromotionDecision::Blocked { reasons, .. } => assert!(reasons
                .iter()
                .any(|r| matches!(r, IneligibleReason::ImplementationMissing))),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn no_validators_blocks_locally_validated() {
        let mut n = TaskNode::skeleton("x", "");
        n.implementation = Implementation::ContainerCommand {
            image: OciImageRef {
                image: "x".into(),
                tag: "y".into(),
                digest: "sha256:abc".into(),
                arch: vec![],
                gpu: false,
            },
            command_template: vec![],
        };
        n.lifecycle_state = LifecycleState::Implemented;
        let d = can_promote(&n, LifecycleState::LocallyValidated);
        match d {
            PromotionDecision::Blocked { reasons, .. } => assert!(reasons
                .iter()
                .any(|r| matches!(r, IneligibleReason::NoLocalValidators))),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn no_benchmarks_blocks_benchmark_validated() {
        let mut n = productionable_node();
        n.evidence.benchmarks.clear();
        n.lifecycle_state = LifecycleState::LocallyValidated;
        let d = can_promote(&n, LifecycleState::BenchmarkValidated);
        match d {
            PromotionDecision::Blocked { reasons, .. } => assert!(reasons
                .iter()
                .any(|r| matches!(r, IneligibleReason::NoBenchmarks))),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn empty_container_digest_blocks_production() {
        let mut n = productionable_node();
        n.implementation = Implementation::ContainerCommand {
            image: OciImageRef {
                image: "x".into(),
                tag: "y".into(),
                digest: String::new(),
                arch: vec![],
                gpu: false,
            },
            command_template: vec![],
        };
        let d = can_promote(&n, LifecycleState::Production);
        match d {
            PromotionDecision::Blocked { reasons, .. } => assert!(reasons
                .iter()
                .any(|r| matches!(r, IneligibleReason::ContainerDigestMissing))),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn unverified_trust_blocks_production() {
        let mut n = productionable_node();
        n.trust_level = TrustLevel::Unverified;
        let d = can_promote(&n, LifecycleState::Production);
        match d {
            PromotionDecision::Blocked { reasons, .. } => assert!(reasons
                .iter()
                .any(|r| matches!(r, IneligibleReason::InsufficientTrust { .. }))),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn clinical_risk_requires_dual_review() {
        let mut n = productionable_node();
        n.risk = RiskClass::Clinical;
        n.trust_level = TrustLevel::Reviewed;
        let d = can_promote(&n, LifecycleState::Production);
        match d {
            PromotionDecision::Blocked { reasons, .. } => assert!(reasons
                .iter()
                .any(|r| matches!(r, IneligibleReason::DualReviewRequired))),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn generated_unreviewed_blocks_production() {
        let mut n = productionable_node();
        n.implementation = Implementation::GeneratedCode {
            repository_ref: "git@example.com/repo".into(),
            review_status: ReviewStatus::Unreviewed,
            artifact_digest: Some("sha256:abc".into()),
        };
        let d = can_promote(&n, LifecycleState::Production);
        match d {
            PromotionDecision::Blocked { reasons, .. } => assert!(reasons
                .iter()
                .any(|r| matches!(r, IneligibleReason::GeneratedCodeUnreviewed))),
            other => panic!("expected Blocked, got {other:?}"),
        }
    }

    #[test]
    fn full_evidence_promotes_to_production() {
        let n = productionable_node();
        // n.lifecycle_state is BenchmarkValidated; targeting Production.
        let d = can_promote(&n, LifecycleState::Production);
        assert!(d.is_eligible(), "expected Eligible, got {d:?}");
    }

    #[test]
    fn deprecated_can_be_set_from_any_state() {
        let mut n = productionable_node();
        n.lifecycle_state = LifecycleState::Production;
        let d = can_promote(&n, LifecycleState::Deprecated);
        assert!(d.is_eligible());
    }
}
