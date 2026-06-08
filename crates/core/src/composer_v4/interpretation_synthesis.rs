//! Inject a `biological_interpretation` node between the analytical
//! strands and the reporting terminal on a v4 [`WorkflowDag`].
//!
//! Gated by `ECAA_COMPOSE_INTERPRETATION` (default off). Invoked from
//! `planner::plan` in BOTH the archetype-seed and search branches,
//! AFTER `wire_dangling_analytical_atoms_to_reporting` (so the strand
//! wiring has already converged every analytical atom onto a reporting
//! terminal) and BEFORE `type_aggregator_fan_in_edges` (so the
//! interpretation node's fan-in is retyped along with every other
//! aggregator edge).
//!
//! # Shape
//!
//! The node is inserted as a consumer of the FIRST reporting-class
//! terminal (an intermediate `reporting` when present, else the
//! universal `final_reporting`) and a producer feeding the SAME
//! terminal's downstream consumer chain — concretely, it sits on the
//! edge `reporting -> final_reporting` becoming
//! `reporting -> biological_interpretation -> final_reporting`. The
//! original `reporting -> final_reporting` edge is NOT removed, so the
//! downstream sink aggregates BOTH the raw report AND the interpretation
//! (its fan-in stays a superset; the `wire_dangling` / `type_aggregator`
//! invariants are preserved). When only a single terminal exists,
//! interpretation is wired downstream of it. The synthesized
//! `validate_biological_interpretation` companion is added by
//! `companion_synthesis::synthesize_validate_companions` — NOT here.
//!
//! # Literature contextualization
//!
//! When `contextualize_findings_with_literature` is present in the DAG,
//! the pass ALSO adds an OPTIONAL edge from its `claims_evidence_matrix`
//! output into the interpretation's `literature_concordance` input port,
//! so the interpretation is literature-grounded. When that atom is
//! absent the interpretation composes from analysis results alone (no
//! literature edge).
//!
//! # Determinism + idempotency
//!
//! - The pass selects its anchor terminal deterministically by id.
//! - It is a no-op when a `biological_interpretation` node already
//!   exists (re-running is safe).
//! - New edges sort into `dag.edges` with the same
//!   `(from_node, from_port, to_node, to_port)` key the sibling passes
//!   use, keeping the WorkflowDag byte-stable.

use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
use crate::workflow_contracts::port::{Cardinality, PortContract};
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

const NODE_ID: &str = "biological_interpretation";
const LIT_ATOM_ID: &str = "contextualize_findings_with_literature";
const LIT_PORT: &str = "literature_concordance";

/// Inject a `biological_interpretation` node before the reporting
/// terminal. No-op when one already exists. See module docs.
pub fn inject_biological_interpretation_before_reporting(dag: &mut WorkflowDag) {
    // Idempotency: already injected.
    if dag.nodes.iter().any(|n| n.id == NODE_ID) {
        return;
    }

    // Anchor terminal: prefer an intermediate `reporting`, else the
    // universal `final_reporting`, else the alphabetically-first
    // reporting-class node. No reporting node → nothing to anchor to.
    let anchor = match pick_anchor_terminal(dag) {
        Some(a) => a,
        None => return,
    };

    // The downstream sink the anchor already feeds (if any). We splice
    // interpretation onto the anchor -> sink edge. When the anchor has
    // no downstream consumer, interpretation becomes the new sink-
    // adjacent node fed by the anchor.
    let downstream_sink: Option<String> = dag
        .edges
        .iter()
        .filter(|e| e.from_node == anchor && is_reporting_terminal(&e.to_node))
        .map(|e| e.to_node.clone())
        .min();

    // Build the interpretation node with both ports.
    let mut node = TaskNode::skeleton(
        NODE_ID,
        "Findings-first biological + method-justification interpretation",
    );
    node.attributes.insert(
        "role".into(),
        serde_json::to_value(crate::atom::AtomRole::Operation).unwrap_or(serde_json::Value::Null),
    );
    node.attributes.insert(
        "assignee".into(),
        serde_json::to_value(crate::atom::AtomAssignee::Agent).unwrap_or(serde_json::Value::Null),
    );
    node.attributes
        .insert("atom_id".into(), serde_json::Value::String(NODE_ID.into()));
    node.lifecycle_state = crate::workflow_contracts::lifecycle::LifecycleState::Production;
    // Primary analysis-bundle input + optional literature port.
    let lit_port = {
        let mut p = PortContract::from_edam(LIT_PORT, None, Some("format:3752"));
        p.semantic_type = crate::workflow_contracts::semantic_type::SemanticType::edam(
            "ecaax:claims_evidence_matrix",
            "",
        );
        p.cardinality = Cardinality::Optional;
        p
    };
    node.inputs = vec![
        PortContract::from_edam("analysis_bundle", Some("data:2048"), Some("format:3464")),
        lit_port,
    ];
    node.outputs = vec![PortContract::from_edam(
        "interpretation",
        Some("data:2048"),
        Some("format:1196"),
    )];

    let mut new_edges: Vec<EdgeContract> = Vec::new();

    // anchor -> biological_interpretation (analysis bundle).
    new_edges.push(ordering_edge(&anchor, "report", NODE_ID, "analysis_bundle"));

    // biological_interpretation -> downstream sink (when one exists),
    // and DROP nothing — the original anchor -> sink edge stays, so the
    // sink aggregates both the raw report AND the interpretation. This
    // keeps `wire_dangling` / `type_aggregator` invariants intact (the
    // sink's fan-in is a superset; no edge is removed).
    if let Some(sink) = &downstream_sink {
        new_edges.push(ordering_edge(NODE_ID, "interpretation", sink, "tributaries"));
    }

    // Literature contextualization: optional edge when the upstream
    // atom is present.
    if dag.nodes.iter().any(|n| n.id == LIT_ATOM_ID) {
        new_edges.push(ordering_edge(
            LIT_ATOM_ID,
            "claims_evidence_matrix",
            NODE_ID,
            LIT_PORT,
        ));
    }

    dag.nodes.push(node);
    dag.edges.extend(new_edges);

    // Re-sort to keep the WorkflowDag byte-stable. Same keys as the
    // sibling synthesis passes.
    dag.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    dag.edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
}

/// Build an `OrderingOnly` edge; `type_aggregator_fan_in_edges`
/// re-proves the fan-in into reporting-class consumers afterward.
fn ordering_edge(from: &str, from_port: &str, to: &str, to_port: &str) -> EdgeContract {
    EdgeContract {
        from_node: from.into(),
        from_port: from_port.into(),
        to_node: to.into(),
        to_port: to_port.into(),
        proof: CompatibilityProof {
            rationale: Some(format!(
                "interpretation_synthesis: wired {from} -> {to} ({to_port})"
            )),
            ..Default::default()
        },
        kind: EdgeKind::OrderingOnly,
        chain_of_custody: None,
    }
}

/// Pick the anchor reporting terminal. Prefer an intermediate
/// `reporting`-class node (NOT the final family), then the universal
/// `final_reporting`, then the alphabetically-first reporting-class id.
fn pick_anchor_terminal(dag: &WorkflowDag) -> Option<String> {
    let mut intermediates: Vec<&str> = dag
        .nodes
        .iter()
        .map(|n| n.id.as_str())
        .filter(|id| is_intermediate_reporting(id))
        .collect();
    intermediates.sort();
    if let Some(first) = intermediates.first() {
        return Some((*first).to_string());
    }
    if dag.nodes.iter().any(|n| n.id == "final_reporting") {
        return Some("final_reporting".into());
    }
    let mut any: Vec<&str> = dag
        .nodes
        .iter()
        .map(|n| n.id.as_str())
        .filter(|id| is_reporting_terminal(id))
        .collect();
    any.sort();
    any.first().map(|s| (*s).to_string())
}

fn is_reporting_terminal(id: &str) -> bool {
    id == "reporting"
        || id == "final_reporting"
        || id == "generic_summary"
        || id.ends_with("_final_reporting")
        || id.ends_with("_reporting")
        || id.ends_with("_thematic_comparison")
}

fn is_intermediate_reporting(id: &str) -> bool {
    if id == "final_reporting" || id.ends_with("_final_reporting") {
        return false;
    }
    id == "reporting"
        || id == "generic_summary"
        || id.ends_with("_reporting")
        || id.ends_with("_thematic_comparison")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
    use crate::workflow_contracts::evidence::AssumptionLedger;
    use crate::workflow_contracts::port::PortContract;
    use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};
    use std::collections::BTreeSet;

    fn node(id: &str) -> TaskNode {
        let mut n = TaskNode::skeleton(id, format!("intent for {id}"));
        n.outputs = vec![PortContract::from_edam(
            "out",
            Some("data:2048"),
            Some("format:3464"),
        )];
        n.inputs = vec![PortContract::from_edam(
            "in",
            Some("data:2048"),
            Some("format:3464"),
        )];
        n
    }

    fn edge(from: &str, to: &str) -> EdgeContract {
        EdgeContract {
            from_node: from.into(),
            from_port: "out".into(),
            to_node: to.into(),
            to_port: "in".into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::OrderingOnly,
            chain_of_custody: None,
        }
    }

    fn dag_with(nodes: Vec<TaskNode>, edges: Vec<EdgeContract>) -> WorkflowDag {
        WorkflowDag {
            id: "t".into(),
            nodes,
            edges,
            assumptions: AssumptionLedger::default(),
            source_template: None,
        }
    }

    fn edge_pairs(dag: &WorkflowDag) -> Vec<(String, String)> {
        dag.edges
            .iter()
            .map(|e| (e.from_node.clone(), e.to_node.clone()))
            .collect()
    }

    /// No literature atom in the DAG: interpretation is injected and
    /// wired BEFORE the final terminal (reporting -> interpretation ->
    /// final_reporting), and NO literature_concordance edge exists.
    #[test]
    fn injects_interpretation_without_literature_edge() {
        let mut dag = dag_with(
            vec![node("reporting"), node("final_reporting")],
            vec![edge("reporting", "final_reporting")],
        );
        inject_biological_interpretation_before_reporting(&mut dag);

        let ids: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(
            ids.contains("biological_interpretation"),
            "node not injected: {ids:?}"
        );

        let into_interp = dag
            .edges
            .iter()
            .any(|e| e.from_node == "reporting" && e.to_node == "biological_interpretation");
        let into_final = dag
            .edges
            .iter()
            .any(|e| e.from_node == "biological_interpretation" && e.to_node == "final_reporting");
        assert!(
            into_interp,
            "reporting must feed interpretation; edges={:?}",
            edge_pairs(&dag)
        );
        assert!(
            into_final,
            "interpretation must feed final_reporting; edges={:?}",
            edge_pairs(&dag)
        );

        let has_lit = dag.edges.iter().any(|e| e.to_port == "literature_concordance");
        assert!(!has_lit, "no literature_concordance edge when the atom is absent");
    }

    /// Idempotency: a second pass adds nothing.
    #[test]
    fn injection_is_idempotent() {
        let mut dag = dag_with(
            vec![node("reporting"), node("final_reporting")],
            vec![edge("reporting", "final_reporting")],
        );
        inject_biological_interpretation_before_reporting(&mut dag);
        let n0 = dag.nodes.len();
        let e0 = dag.edges.len();
        inject_biological_interpretation_before_reporting(&mut dag);
        assert_eq!(dag.nodes.len(), n0, "second pass added nodes");
        assert_eq!(dag.edges.len(), e0, "second pass added edges");
    }

    /// When contextualize_findings_with_literature is present, the pass
    /// adds the OPTIONAL literature edge into the interpretation's
    /// literature_concordance port.
    #[test]
    fn injects_literature_edge_when_atom_present() {
        let mut lit = node(LIT_ATOM_ID);
        lit.outputs = vec![PortContract::from_edam(
            "claims_evidence_matrix",
            None,
            Some("format:3752"),
        )];
        let mut dag = dag_with(
            vec![lit, node("reporting"), node("final_reporting")],
            vec![edge("reporting", "final_reporting")],
        );
        inject_biological_interpretation_before_reporting(&mut dag);

        let lit_edge = dag.edges.iter().find(|e| {
            e.from_node == LIT_ATOM_ID
                && e.to_node == "biological_interpretation"
                && e.to_port == "literature_concordance"
        });
        assert!(
            lit_edge.is_some(),
            "literature edge missing; edges={:?}",
            edge_pairs(&dag)
        );
    }

    /// No reporting terminal at all → no anchor → no injection.
    #[test]
    fn no_op_without_reporting_terminal() {
        let mut dag = dag_with(vec![node("differential_expression")], vec![]);
        inject_biological_interpretation_before_reporting(&mut dag);
        assert!(
            !dag.nodes.iter().any(|n| n.id == "biological_interpretation"),
            "must not inject without a reporting terminal"
        );
    }

    /// Anchor preference: an intermediate `reporting` is chosen over the
    /// `final_reporting` family, so interpretation sits between them.
    #[test]
    fn anchors_on_intermediate_reporting() {
        let mut dag = dag_with(
            vec![node("reporting"), node("final_reporting")],
            vec![edge("reporting", "final_reporting")],
        );
        inject_biological_interpretation_before_reporting(&mut dag);
        // reporting -> interpretation must be present (intermediate anchor).
        assert!(
            dag.edges
                .iter()
                .any(|e| e.from_node == "reporting" && e.to_node == "biological_interpretation"),
            "interpretation must anchor on the intermediate reporting node; edges={:?}",
            edge_pairs(&dag)
        );
    }
}
