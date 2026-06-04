//! Shared, pure rewire-or-drop helper consumed by both the
//! conversation crate's `prune_excluded_atoms` post-pass and the
//! upcoming `prune_unsourced_atoms` pass (atoms whose required inputs
//! have no surviving upstream source).
//!
//! The helper is **pure**: it only receives `(&mut WorkflowDag,
//! &BTreeSet<String>)` and carries no session state. Callers build the
//! `dropped` set with their own policy (exclusion list, unsourced
//! detection, etc.) and hand it to `rewire_or_drop` which applies a
//! single-sweep fixup:
//!
//!  1. Surviving nodes whose *every* incoming edge came from a dropped
//!     node are **rewired** to `data_acquisition` (the synthetic upstream
//!     anchor when the SME's data is post-pipeline).
//!  2. When `data_acquisition` was itself dropped (rare), those nodes are
//!     **cascade-dropped** instead — their ids are added to `dropped`.
//!  3. All edges referencing dropped nodes are removed.
//!  4. All dropped nodes are removed.
//!  5. New `OrderingOnly` rewire edges are added (idempotent).
//!
//! The single-sweep guarantee: after one pass the graph is stable.
//! Rewired nodes point to `data_acquisition` which (by construction)
//! has no incoming edges and thus cannot itself be made orphan by the
//! sweep. No fixpoint iteration needed.

use std::collections::BTreeSet;

use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
use crate::workflow_contracts::task_node::WorkflowDag;

/// The synthetic upstream fallback node id.  Any surviving node whose
/// every incoming edge came from a dropped node gets rewired here,
/// keeping it reachable without a cascade-drop.
const DATA_ACQ_ID: &str = "data_acquisition";

/// Apply a single rewire-or-drop sweep to `dag` given a pre-computed
/// set of node ids to drop.
///
/// # Semantics
///
/// * **Rewire** — a surviving node that lost all its incoming edges
///   because every upstream was in `dropped` is reconnected to
///   `data_acquisition` via an `OrderingOnly` edge rather than
///   cascade-dropped.  The rationale on the proof reads:
///   `"rewired to data_acquisition because upstream atom(s) were excluded by SME"`.
/// * **Cascade-drop** — when `data_acquisition` was itself dropped (or
///   does not exist), the orphaned surviving node is added to `dropped`
///   and removed with the rest.
/// * **Idempotency** — rewire edges are only added when no edge with
///   the same `(from_node, to_node)` pair already exists.
///
/// # Determinism
///
/// `dropped` is a `BTreeSet` so iteration order is stable and the
/// resulting node + edge vecs are produced in the same order on every
/// call with identical inputs.
///
/// # Modifies
///
/// `dag.nodes`, `dag.edges`.  `dag.assumptions` and
/// `dag.source_template` are left untouched.
pub fn rewire_or_drop(dag: &mut WorkflowDag, dropped: &BTreeSet<String>) {
    // Nothing to do — fast-path avoids even scanning nodes/edges.
    if dropped.is_empty() {
        return;
    }

    // `data_acquisition` must both exist as a node AND not itself be in
    // the drop set for rewiring to be possible.
    let data_acq_present =
        dag.nodes.iter().any(|n| n.id == DATA_ACQ_ID) && !dropped.contains(DATA_ACQ_ID);

    // Rewire-or-drop pass (single sweep; no fixpoint needed — see module
    // doc for the stability argument).
    let mut rewires: Vec<(String, String)> = Vec::new();
    // We need to *extend* `dropped` in the cascade-drop branch, but we
    // also need to keep iterating `dag.nodes`.  Collect cascade-drops
    // separately and merge afterwards.
    let mut cascade: Vec<String> = Vec::new();
    for node in dag.nodes.iter() {
        if dropped.contains(&node.id) {
            continue;
        }
        let incoming: Vec<&str> = dag
            .edges
            .iter()
            .filter(|e| e.to_node == node.id)
            .map(|e| e.from_node.as_str())
            .collect();
        if incoming.is_empty() {
            // No incoming edges at all — node is a source; not affected.
            continue;
        }
        let all_dropped = incoming.iter().all(|src| dropped.contains(*src));
        if !all_dropped {
            continue;
        }
        if data_acq_present && node.id != DATA_ACQ_ID {
            rewires.push((DATA_ACQ_ID.to_string(), node.id.clone()));
        } else {
            cascade.push(node.id.clone());
        }
    }

    // Build the effective drop set (original + cascade) without mutating
    // the caller's set.
    let effective_drop: BTreeSet<&str> = dropped
        .iter()
        .map(String::as_str)
        .chain(cascade.iter().map(String::as_str))
        .collect();

    // Drop edges referencing any dropped node, then drop the nodes.
    dag.edges
        .retain(|e| !effective_drop.contains(e.from_node.as_str()) && !effective_drop.contains(e.to_node.as_str()));
    dag.nodes
        .retain(|n| !effective_drop.contains(n.id.as_str()));

    // Add rewire edges (after dropping the now-dead originals).
    // Minimal EdgeContract with sentinel ports + a rationale so the
    // downstream lowering path can pick up the edge.
    for (from_node, to_node) in rewires {
        let already = dag
            .edges
            .iter()
            .any(|e| e.from_node == from_node && e.to_node == to_node);
        if already {
            continue;
        }
        let proof = CompatibilityProof {
            rationale: Some(
                "rewired to data_acquisition because upstream atom(s) were excluded by SME"
                    .to_string(),
            ),
            ..Default::default()
        };
        dag.edges.push(EdgeContract {
            from_node,
            from_port: "_excluded_rewire".into(),
            to_node,
            to_port: "_excluded_rewire".into(),
            proof,
            // Structural re-wire after an SME atom exclusion: an
            // ordering edge, not a port-typed data flow.
            kind: EdgeKind::OrderingOnly,
            chain_of_custody: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
    use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

    /// Build a minimal typed edge for test DAGs.
    fn typed_edge(from: &str, to: &str) -> EdgeContract {
        EdgeContract {
            from_node: from.into(),
            from_port: "out".into(),
            to_node: to.into(),
            to_port: "in".into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::TypedDataFlow,
            chain_of_custody: None,
        }
    }

    /// `A → B → C`.  Drop B.  Assert: B gone, A→C rewire edge added.
    ///
    /// This is the primary use-case the upcoming `prune_unsourced_atoms`
    /// caller needs: middle-node removal with downstream preserved.
    #[test]
    fn rewire_or_drop_middle_node_rewires_c_to_data_acquisition() {
        let a = TaskNode::skeleton("data_acquisition", "source");
        let b = TaskNode::skeleton("B", "middle");
        let c = TaskNode::skeleton("C", "downstream");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![a.clone(), b.clone(), c.clone()],
            edges: vec![
                typed_edge("data_acquisition", "B"),
                typed_edge("B", "C"),
            ],
            ..Default::default()
        };

        let dropped: BTreeSet<String> = ["B".to_string()].into_iter().collect();
        rewire_or_drop(&mut dag, &dropped);

        // B must be gone.
        let ids: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(!ids.contains(&"B"), "B must be dropped; nodes={ids:?}");
        assert!(
            ids.contains(&"data_acquisition"),
            "data_acquisition must survive"
        );
        assert!(ids.contains(&"C"), "C must survive");

        // There must be a rewire edge data_acquisition → C.
        let has_rewire = dag
            .edges
            .iter()
            .any(|e| e.from_node == "data_acquisition" && e.to_node == "C");
        assert!(
            has_rewire,
            "expected a rewire edge data_acquisition→C; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.as_str(), e.to_node.as_str()))
                .collect::<Vec<_>>()
        );

        // The old B→C edge must be gone.
        let b_to_c = dag.edges.iter().any(|e| e.from_node == "B");
        assert!(!b_to_c, "stale B→C edge must be removed");

        // The rewire edge must be OrderingOnly.
        let rewire_edge = dag
            .edges
            .iter()
            .find(|e| e.from_node == "data_acquisition" && e.to_node == "C")
            .unwrap();
        assert_eq!(
            rewire_edge.kind,
            EdgeKind::OrderingOnly,
            "rewire edge must be OrderingOnly"
        );
    }

    /// When `data_acquisition` itself is dropped, orphaned downstream
    /// nodes must be cascade-dropped rather than rewired.
    #[test]
    fn rewire_or_drop_cascade_drops_when_data_acq_is_gone() {
        let da = TaskNode::skeleton("data_acquisition", "source");
        let b = TaskNode::skeleton("B", "only node");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![da, b],
            edges: vec![typed_edge("data_acquisition", "B")],
            ..Default::default()
        };

        let dropped: BTreeSet<String> = ["data_acquisition".to_string(), "B".to_string()]
            .into_iter()
            .collect();
        rewire_or_drop(&mut dag, &dropped);

        assert!(dag.nodes.is_empty(), "both nodes must be dropped");
        assert!(dag.edges.is_empty(), "all edges must be dropped");
    }

    /// Empty `dropped` set is a fast-path no-op — dag unchanged.
    #[test]
    fn rewire_or_drop_empty_dropped_is_noop() {
        let a = TaskNode::skeleton("data_acquisition", "source");
        let b = TaskNode::skeleton("B", "node");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![a, b],
            edges: vec![typed_edge("data_acquisition", "B")],
            ..Default::default()
        };
        let before_nodes = dag.nodes.len();
        let before_edges = dag.edges.len();

        rewire_or_drop(&mut dag, &BTreeSet::new());

        assert_eq!(dag.nodes.len(), before_nodes, "nodes unchanged");
        assert_eq!(dag.edges.len(), before_edges, "edges unchanged");
    }

    /// Rewire is idempotent — calling twice doesn't add duplicate edges.
    #[test]
    fn rewire_or_drop_rewire_is_idempotent() {
        let a = TaskNode::skeleton("data_acquisition", "source");
        let b = TaskNode::skeleton("B", "middle");
        let c = TaskNode::skeleton("C", "downstream");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![a, b, c],
            edges: vec![
                typed_edge("data_acquisition", "B"),
                typed_edge("B", "C"),
            ],
            ..Default::default()
        };

        let dropped: BTreeSet<String> = ["B".to_string()].into_iter().collect();
        rewire_or_drop(&mut dag, &dropped);
        // second call: B is already gone, C already rewired — should be a no-op
        let dropped2: BTreeSet<String> = BTreeSet::new();
        rewire_or_drop(&mut dag, &dropped2);

        let rewire_count = dag
            .edges
            .iter()
            .filter(|e| e.from_node == "data_acquisition" && e.to_node == "C")
            .count();
        assert_eq!(rewire_count, 1, "exactly one rewire edge; got {rewire_count}");
    }
}
