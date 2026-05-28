//! Backend capability report.
//!
//! The IR shape (`WorkflowDag` + sidecar proofs / assumptions / policy
//! decisions / per-port semantic types) is full-shape. Not every
//! backend can preserve every one of these constraints — CWL drops
//! per-edge typed-port proofs into untyped string-id ports; WDL
//! flattens parameterized resource estimates into scalar resource
//! requests; Nextflow projects assumption ledgers into free-text
//! comments. `BackendCapabilityReport` is the typed surface every
//! backend's `compile()` method returns alongside the artifact so the
//! conversation layer can either (a) accept the loss when the SME has
//! pre-authorized it via `EmitContext::authorized_losses` or (b)
//! refuse the emit with `CompileError::SemanticLossNotAuthorized`.
//!
//! Today only `WorkflowJsonEmitter` ships. The custom harness
//! consumes the WORKFLOW.json + sidecars natively so its
//! `BackendCapabilityReport` is always empty. R2-N21
//! deleted the single-impl `BackendEmitter` trait; the report shape
//! itself stays — it is what every future backend's inherent
//! `compile()` method returns alongside the artifact (per v3 §9.3 / F17).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// What semantic constraints from the `WorkflowDag` did the backend
/// emitter fail to represent in the emitted artifact? Returned from
/// every backend's `compile()` method alongside the artifact.
#[derive(
    Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
pub struct BackendCapabilityReport {
    /// Backend identifier (e.g. `"workflow_json"`, `"cwl"`,
    /// `"nextflow"`). Matches the emitter's `name()` method.
    pub backend: String,
    /// Per-constraint losses. Empty when the backend round-trips the
    /// full IR shape (today's `workflow_json` case).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub losses: Vec<UnsupportedConstraint>,
}

/// Discriminated union of the constraint classes a backend can lose.
/// The tag (`logical_template_scatter`, etc.) round-trips through
/// `ConstraintLossAck::constraint_kind` so an SME ack for a given
/// kind authorizes every loss of that kind on the same emit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UnsupportedConstraint {
    /// Logical-template-level scatter directive (e.g.
    /// `for_each(sample)`) couldn't be represented in the target
    /// backend's iteration model.
    LogicalTemplateScatter {
        /// DAG node id the scatter directive belongs to.
        node_id: String,
        /// Human-readable reason the scatter couldn't be represented.
        reason: String,
    },
    /// Parameterized resource estimate (`cpu = f(sample_count)`)
    /// projected to a scalar value the backend can request.
    ParameterizedResourceEstimate {
        /// DAG node id the estimate applies to.
        node_id: String,
        /// Original parameterized expression (e.g. `cpu = 2 * sample_count`).
        expression: String,
    },
    /// Typed semantic marker on a port dropped to an untyped string
    /// id in the artifact.
    SemanticTypeMarker {
        /// Port id whose type marker was dropped.
        port_id: String,
        /// EDAM or local-extension IRI that was dropped.
        type_iri: String,
    },
    /// Assumption ledger projected to a free-text comment block (or
    /// dropped entirely).
    AssumptionLedgerProjection {
        /// Number of ledger entries projected.
        entry_count: usize,
    },
    /// Per-edge policy-decision audit projected to a sidecar the
    /// backend doesn't natively consume.
    PolicyDecisionProjection {
        /// Number of policy decisions projected.
        decision_count: usize,
    },
    /// Per-edge compatibility-proof block projected to a sidecar.
    EdgeProofProjection {
        /// Number of edge proofs projected.
        proof_count: usize,
    },
}

/// SME-authored authorization for a single class of
/// `UnsupportedConstraint`. `constraint_kind` matches the snake-case
/// variant tag from `UnsupportedConstraint`; one ack authorizes
/// every loss of the matching kind on the same emit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ConstraintLossAck {
    /// Matches an `UnsupportedConstraint` variant tag (snake_case).
    pub constraint_kind: String,
    /// Free-text justification the SME provided.
    pub rationale: String,
    /// Authorizing party (SME initials, oncall, etc.).
    pub authorized_by: String,
}

impl BackendCapabilityReport {
    /// True when the backend reports zero losses — the artifact
    /// round-trips the full IR shape.
    pub fn is_empty(&self) -> bool {
        self.losses.is_empty()
    }

    /// Are all losses authorized by the supplied acks? An empty
    /// report is trivially fully authorized.
    pub fn fully_authorized(&self, acks: &[ConstraintLossAck]) -> bool {
        self.losses.iter().all(|loss| {
            let tag = loss_tag(loss);
            acks.iter().any(|a| a.constraint_kind == tag)
        })
    }
}

/// Snake-case tag for an `UnsupportedConstraint` variant. Public so
/// downstream tooling (UI surface, audit log) can build the same
/// `constraint_kind` string when authoring a `ConstraintLossAck`.
pub fn loss_tag(loss: &UnsupportedConstraint) -> &'static str {
    match loss {
        UnsupportedConstraint::LogicalTemplateScatter { .. } => "logical_template_scatter",
        UnsupportedConstraint::ParameterizedResourceEstimate { .. } => {
            "parameterized_resource_estimate"
        }
        UnsupportedConstraint::SemanticTypeMarker { .. } => "semantic_type_marker",
        UnsupportedConstraint::AssumptionLedgerProjection { .. } => "assumption_ledger_projection",
        UnsupportedConstraint::PolicyDecisionProjection { .. } => "policy_decision_projection",
        UnsupportedConstraint::EdgeProofProjection { .. } => "edge_proof_projection",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report_is_fully_authorized() {
        let report = BackendCapabilityReport {
            backend: "workflow_json".into(),
            losses: vec![],
        };
        assert!(report.is_empty());
        assert!(report.fully_authorized(&[]));
    }

    #[test]
    fn unmatched_loss_blocks_authorization() {
        let report = BackendCapabilityReport {
            backend: "cwl".into(),
            losses: vec![UnsupportedConstraint::SemanticTypeMarker {
                port_id: "align_reads.bam".into(),
                type_iri: "data:0863".into(),
            }],
        };
        assert!(!report.fully_authorized(&[]));
    }

    #[test]
    fn matching_ack_authorizes_loss() {
        let report = BackendCapabilityReport {
            backend: "cwl".into(),
            losses: vec![UnsupportedConstraint::SemanticTypeMarker {
                port_id: "align_reads.bam".into(),
                type_iri: "data:0863".into(),
            }],
        };
        let acks = vec![ConstraintLossAck {
            constraint_kind: "semantic_type_marker".into(),
            rationale: "Backend strips port types; SME accepts.".into(),
            authorized_by: "AH".into(),
        }];
        assert!(report.fully_authorized(&acks));
    }

    #[test]
    fn loss_tag_is_snake_case() {
        let loss = UnsupportedConstraint::EdgeProofProjection { proof_count: 3 };
        assert_eq!(loss_tag(&loss), "edge_proof_projection");
        // Round-trip via serde tag.
        let json = serde_json::to_value(&loss).unwrap();
        assert_eq!(json["kind"].as_str(), Some("edge_proof_projection"));
    }

    #[test]
    fn ack_partial_coverage_fails() {
        let report = BackendCapabilityReport {
            backend: "cwl".into(),
            losses: vec![
                UnsupportedConstraint::SemanticTypeMarker {
                    port_id: "p1".into(),
                    type_iri: "data:0863".into(),
                },
                UnsupportedConstraint::EdgeProofProjection { proof_count: 2 },
            ],
        };
        let acks = vec![ConstraintLossAck {
            constraint_kind: "semantic_type_marker".into(),
            rationale: "ok".into(),
            authorized_by: "AH".into(),
        }];
        // semantic_type_marker authorized, edge_proof_projection not.
        assert!(!report.fully_authorized(&acks));
    }
}
