//! V3+v4 residuals apply a `DagModification` payload to an
//! in-flight `WorkflowDag`. Used by the planner's auto-application path
//! for `LowAutoAttempt` repair proposals.
//!
//! Today the planner emits `VerifierDecision::RepairProposed` for every
//! repair-registry proposal but never splices the `DagModification`
//! back into the search-produced DAG, even when the proposal's
//! `risk_class == LowAutoAttempt`. This module closes the gap so safe
//! mechanical repairs (gzip decompression, sort/index regeneration)
//! flow end-to-end without an SME click.
//!
//! F20 invariant is preserved at the call site: the planner only
//! invokes `apply_dag_modification` for proposals whose `risk_class <=
//! ctx.auto_attempt_risk_threshold`. `MediumUserGated` and
//! `HighCredentialedReview` proposals continue to emit substrate
//! (`RepairProposed`) but never reach this helper.

use crate::repair::proposal::DagModification;
use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
use crate::workflow_contracts::evidence::AssumptionRef;
use crate::workflow_contracts::task_node::WorkflowDag;

/// Typed failure modes for `apply_dag_modification`. The planner's
/// auto-apply path translates an `ApplyError` into
/// `VerifierDecision::RepairRejected { reason }` so the failure shows
/// up in the audit trail rather than being swallowed.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApplyError {
    /// The modification references a node id that doesn't exist in the
    /// in-flight DAG. The strategy emitted a stale proposal (or the
    /// planner mutated the DAG between proposal-time and apply-time).
    #[error("node `{0}` referenced by modification not found in DAG")]
    UnknownNode(String),
    /// The modification's `kind` carries a payload the auto-apply path
    /// can't service (e.g. `RequestMissingMetadata` — that one's an
    /// SME-side action, not a mutation). The planner records a
    /// `RepairRejected` row and moves on.
    #[error("modification kind `{0}` is not auto-applicable")]
    NotAutoApplicable(&'static str),
}

/// Apply a `DagModification` to a `WorkflowDag` in place. Returns
/// `Ok(())` on success, `Err(ApplyError)` when the mutation cannot be
/// applied to the current DAG state.
///
/// Supported variants:
///
/// - `InsertConverter` — splices the converter node onto the offending
///   edge. The original `source_port → sink_port` edge is rewritten
///   to flow `source_port → converter → sink_port`. Compatibility
///   proofs on the new edges are a thin "lossless converter" stub so
///   the F05 invariant (every edge carries a proof) holds even before
///   the engine re-validates.
/// - `SubstituteProducer` — replaces the named producer node with a
///   new `TaskNode`. Edges that referenced the old producer are
///   rewired to the new one; the old node is dropped.
/// - `InsertLiftover` — not auto-applicable today: liftover is
///   `HighCredentialedReview` and never reaches this path. Returns
///   `NotAutoApplicable` defensively if it does.
/// - `RequestMissingMetadata` / `QueryRegistry` / `RewriteContract` —
///   all SME-side actions; not auto-applicable here.
///
/// The function intentionally only mutates `dag.nodes` + `dag.edges`.
/// `dag.assumptions` and `dag.source_template` are left untouched —
/// re-running the meet-in-the-middle pass after the mutation is the
/// place where assumption ledgering happens (the planner re-runs
/// search + companion synthesis when at least one auto-apply
/// succeeds).
pub fn apply_dag_modification(
    dag: &mut WorkflowDag,
    modification: &DagModification,
) -> Result<(), ApplyError> {
    match modification {
        DagModification::InsertConverter {
            converter_node,
            source_port,
            sink_port,
        } => {
            // Verify both endpoints exist. The converter node is the
            // strategy's payload — we don't require it to exist
            // pre-apply (it's being inserted).
            if !dag.nodes.iter().any(|n| n.id == source_port.node_id) {
                return Err(ApplyError::UnknownNode(source_port.node_id.clone()));
            }
            if !dag.nodes.iter().any(|n| n.id == sink_port.node_id) {
                return Err(ApplyError::UnknownNode(sink_port.node_id.clone()));
            }
            // Insert the converter node (idempotent on id).
            if !dag.nodes.iter().any(|n| n.id == converter_node.id) {
                dag.nodes.push(converter_node.clone());
            }
            // Rewire: locate the edge from source_port to sink_port and
            // rewrite it to flow producer → converter, then add a new
            // converter → sink edge. When the original edge is absent
            // (the meet result didn't build one because compatibility
            // failed), add both new edges directly.
            let edge_idx = dag.edges.iter().position(|e| {
                e.from_node == source_port.node_id
                    && e.from_port == source_port.port_name
                    && e.to_node == sink_port.node_id
                    && e.to_port == sink_port.port_name
            });
            let converter_in_port = converter_node
                .inputs
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| sink_port.port_name.clone());
            let converter_out_port = converter_node
                .outputs
                .first()
                .map(|p| p.name.clone())
                .unwrap_or_else(|| sink_port.port_name.clone());
            match edge_idx {
                Some(idx) => {
                    let original = dag.edges[idx].clone();
                    dag.edges[idx] = EdgeContract {
                        from_node: source_port.node_id.clone(),
                        from_port: source_port.port_name.clone(),
                        to_node: converter_node.id.clone(),
                        to_port: converter_in_port,
                        proof: lossless_converter_proof("repair_inserted_converter"),
                        chain_of_custody: None,
                    };
                    dag.edges.push(EdgeContract {
                        from_node: converter_node.id.clone(),
                        from_port: converter_out_port,
                        to_node: original.to_node,
                        to_port: original.to_port,
                        proof: lossless_converter_proof("repair_inserted_converter"),
                        chain_of_custody: None,
                    });
                }
                None => {
                    dag.edges.push(EdgeContract {
                        from_node: source_port.node_id.clone(),
                        from_port: source_port.port_name.clone(),
                        to_node: converter_node.id.clone(),
                        to_port: converter_in_port,
                        proof: lossless_converter_proof("repair_inserted_converter"),
                        chain_of_custody: None,
                    });
                    dag.edges.push(EdgeContract {
                        from_node: converter_node.id.clone(),
                        from_port: converter_out_port,
                        to_node: sink_port.node_id.clone(),
                        to_port: sink_port.port_name.clone(),
                        proof: lossless_converter_proof("repair_inserted_converter"),
                        chain_of_custody: None,
                    });
                }
            }
            Ok(())
        }
        DagModification::SubstituteProducer { remove, add } => {
            let pos = dag.nodes.iter().position(|n| n.id == *remove);
            let Some(pos) = pos else {
                return Err(ApplyError::UnknownNode(remove.clone()));
            };
            dag.nodes[pos] = add.clone();
            // Rewire every edge whose producer was the removed node.
            let new_id = add.id.clone();
            for edge in dag.edges.iter_mut() {
                if edge.from_node == *remove {
                    edge.from_node = new_id.clone();
                }
            }
            Ok(())
        }
        DagModification::InsertLiftover { .. } => {
            // Liftover is HighCredentialedReview by construction; it
            // never reaches the auto-apply path. Return a typed error
            // so a misconfiguration surfaces as a `RepairRejected`
            // substrate row rather than a silent splice.
            Err(ApplyError::NotAutoApplicable("insert_liftover"))
        }
        DagModification::RequestMissingMetadata { .. } => {
            Err(ApplyError::NotAutoApplicable("request_missing_metadata"))
        }
        DagModification::QueryRegistry { .. } => {
            Err(ApplyError::NotAutoApplicable("query_registry"))
        }
        DagModification::RewriteContract { .. } => {
            Err(ApplyError::NotAutoApplicable("rewrite_contract"))
        }
    }
}

/// Stub compatibility proof for repair-inserted converter edges. The
/// post-apply meet-in-the-middle re-run replaces this with a real
/// engine-proven proof. We mint a non-empty placeholder so F05 ("every
/// edge carries a proof") holds during the brief window between
/// apply-time and re-run.
fn lossless_converter_proof(label: &str) -> CompatibilityProof {
    CompatibilityProof {
        rationale: Some(format!(
            "Edge inserted by repair auto-apply ({label}); \
             meet-in-the-middle re-run reproves with full facet unification."
        )),
        assumptions: vec![AssumptionRef {
            id: format!("{label}:reprove_pending"),
        }],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair::proposal::PortRef;
    use crate::workflow_contracts::edge::EdgeContract;
    use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

    fn skeleton_edge(producer: &str, p_port: &str, consumer: &str, c_port: &str) -> EdgeContract {
        EdgeContract {
            from_node: producer.into(),
            from_port: p_port.into(),
            to_node: consumer.into(),
            to_port: c_port.into(),
            proof: lossless_converter_proof("test"),
            chain_of_custody: None,
        }
    }

    #[test]
    fn insert_converter_splices_node_and_rewires_edge() {
        let producer = TaskNode::skeleton("upstream", "upstream node");
        let consumer = TaskNode::skeleton("downstream", "downstream node");
        let converter = TaskNode::skeleton("gunzip_downstream", "decompress gzip input");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![producer.clone(), consumer.clone()],
            edges: vec![skeleton_edge("upstream", "out_bam", "downstream", "in_bam")],
            ..Default::default()
        };
        let modification = DagModification::InsertConverter {
            converter_node: converter.clone(),
            source_port: PortRef {
                node_id: "upstream".into(),
                port_name: "out_bam".into(),
            },
            sink_port: PortRef {
                node_id: "downstream".into(),
                port_name: "in_bam".into(),
            },
        };
        apply_dag_modification(&mut dag, &modification).expect("apply must succeed");
        assert!(dag.nodes.iter().any(|n| n.id == "gunzip_downstream"));
        // Original edge rewritten to producer → converter; new edge
        // converter → consumer added.
        assert_eq!(dag.edges.len(), 2);
        let to_converter = dag.edges.iter().find(|e| e.to_node == "gunzip_downstream");
        let from_converter = dag
            .edges
            .iter()
            .find(|e| e.from_node == "gunzip_downstream");
        assert!(
            to_converter.is_some(),
            "expected an edge into the converter"
        );
        assert!(
            from_converter.is_some(),
            "expected an edge out of the converter"
        );
        assert_eq!(from_converter.unwrap().to_node, "downstream");
    }

    #[test]
    fn insert_converter_fails_when_source_node_missing() {
        let consumer = TaskNode::skeleton("downstream", "downstream node");
        let converter = TaskNode::skeleton("gunzip_downstream", "decompress");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![consumer.clone()],
            edges: vec![],
            ..Default::default()
        };
        let modification = DagModification::InsertConverter {
            converter_node: converter.clone(),
            source_port: PortRef {
                node_id: "absent".into(),
                port_name: "out".into(),
            },
            sink_port: PortRef {
                node_id: "downstream".into(),
                port_name: "in".into(),
            },
        };
        let err = apply_dag_modification(&mut dag, &modification).unwrap_err();
        assert_eq!(err, ApplyError::UnknownNode("absent".into()));
    }

    #[test]
    fn substitute_producer_replaces_node_and_rewires_edges() {
        let producer = TaskNode::skeleton("old_producer", "old");
        let consumer = TaskNode::skeleton("downstream", "downstream");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![producer.clone(), consumer.clone()],
            edges: vec![skeleton_edge("old_producer", "out", "downstream", "in")],
            ..Default::default()
        };
        let new_producer = TaskNode::skeleton("new_producer", "alt");
        let modification = DagModification::SubstituteProducer {
            remove: "old_producer".into(),
            add: new_producer.clone(),
        };
        apply_dag_modification(&mut dag, &modification).expect("substitute must succeed");
        assert!(dag.nodes.iter().any(|n| n.id == "new_producer"));
        assert!(!dag.nodes.iter().any(|n| n.id == "old_producer"));
        assert_eq!(dag.edges.len(), 1);
        assert_eq!(dag.edges[0].from_node, "new_producer");
        assert_eq!(dag.edges[0].to_node, "downstream");
    }

    #[test]
    fn substitute_producer_fails_when_old_node_missing() {
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![],
            edges: vec![],
            ..Default::default()
        };
        let modification = DagModification::SubstituteProducer {
            remove: "absent".into(),
            add: TaskNode::skeleton("new", "alt"),
        };
        let err = apply_dag_modification(&mut dag, &modification).unwrap_err();
        assert_eq!(err, ApplyError::UnknownNode("absent".into()));
    }

    #[test]
    fn liftover_is_not_auto_applicable() {
        let mut dag = WorkflowDag::default();
        let modification = DagModification::InsertLiftover {
            from_build: "GRCh37".into(),
            to_build: "GRCh38".into(),
            target_edge: crate::repair::proposal::EdgeRef {
                from: PortRef {
                    node_id: "u".into(),
                    port_name: "p".into(),
                },
                to: PortRef {
                    node_id: "d".into(),
                    port_name: "p".into(),
                },
            },
        };
        let err = apply_dag_modification(&mut dag, &modification).unwrap_err();
        assert!(matches!(
            err,
            ApplyError::NotAutoApplicable("insert_liftover")
        ));
    }

    #[test]
    fn request_metadata_is_not_auto_applicable() {
        let mut dag = WorkflowDag::default();
        let modification = DagModification::RequestMissingMetadata {
            field: "genome_build".into(),
            applies_to_node: "x".into(),
        };
        let err = apply_dag_modification(&mut dag, &modification).unwrap_err();
        assert!(matches!(err, ApplyError::NotAutoApplicable(_)));
    }
}
