//! Coherence gate (Pillar D). A post-selection pass over the chosen
//! `WorkflowDag` that surfaces semantic-incoherence findings as
//! warn/surface signals — never a hard reject (conservative by default
//! so legitimate cross-omics work is not flagged).
//!
//! Phase 1 shipped the seam (a no-op invoked from `plan()`'s multi-branch
//! dispatch beside `policy_gate::evaluate`). This pass implements the
//! **orphan-strand** detector: an analytical node whose output is never
//! consumed (no outgoing edge) and which is not a legitimate terminal
//! (the final report) or a leaf companion (a `validate_*` validator).
//! Such a node represents analysis whose result never flows into the
//! report — the "analytical atoms orphaned from the goal" case from the
//! design. This detector reads only the DAG, so it cannot false-positive
//! on legitimately-shared atoms (e.g. `data_acquisition`, `reporting`)
//! the way an atom-modality-affiliation check could.
//!
//! Deliberately NOT implemented here (documented follow-up): the
//! "unrelated-modality mixing" detector — flagging a node whose atom is
//! affiliated with a different modality's archetype than the branch it
//! sits in. That needs the atom + archetype registries and careful
//! calibration against the fixture corpus to avoid flagging shared atoms;
//! shipping it half-calibrated would violate the spec's "must not flag
//! legitimate cross-omics" constraint. The orphan-strand detector below
//! is registry-free and false-positive-free, so it ships now.

use crate::workflow_contracts::task_node::WorkflowDag;

#[derive(Debug, Clone, Default)]
pub(crate) struct CoherenceEvaluation {
    /// Human-readable incoherence findings. Empty = coherent. Surfaced as
    /// a `tracing::warn!` by the caller; never a hard reject.
    pub findings: Vec<String>,
}

/// True when `id` names a synthesized validator companion. Validators are
/// expected DAG leaves (they assert on an upstream node's output and
/// produce no consumed artifact), so they are NOT orphan strands. Matches
/// both the bare `validate_<stage>` and the branch-prefixed
/// `<modality>_validate_<stage>` shapes produced by companion synthesis.
fn is_validator(id: &str) -> bool {
    id.starts_with("validate_") || id.contains("_validate_")
}

/// True when `id` is a legitimate reporting terminal — the project-level
/// `final_reporting` (the intended DAG sink), bare or branch-prefixed. The
/// `multi_modal_thematic_comparison` join is NOT a terminal (it feeds
/// `final_reporting`), so it always has an outgoing edge and never reaches
/// the orphan check.
fn is_final_terminal(id: &str) -> bool {
    id == "final_reporting" || id.ends_with("_final_reporting")
}

/// Evaluate a chosen `WorkflowDag` for coherence. Returns findings for
/// each orphan strand: a node with no outgoing edge that is neither the
/// final reporting terminal nor a validator leaf — i.e. analytical output
/// that never reaches the report. Conservative: a healthy DAG (every
/// analytical terminal wired into the join → report) yields zero findings.
pub(crate) fn evaluate(dag: &WorkflowDag) -> CoherenceEvaluation {
    use std::collections::BTreeSet;
    let has_outgoing: BTreeSet<&str> = dag.edges.iter().map(|e| e.from_node.as_str()).collect();
    let mut findings: Vec<String> = dag
        .nodes
        .iter()
        .filter(|n| !has_outgoing.contains(n.id.as_str()))
        .filter(|n| !is_final_terminal(&n.id) && !is_validator(&n.id))
        .map(|n| {
            format!(
                "orphan_strand: node '{}' produces output that is never consumed \
                 (no outgoing edge) and is not the final report — its analysis does \
                 not reach the SME report",
                n.id
            )
        })
        .collect();
    findings.sort();
    CoherenceEvaluation { findings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
    use crate::workflow_contracts::task_node::TaskNode;

    fn node(id: &str) -> TaskNode {
        TaskNode::skeleton(id, "test node")
    }
    fn edge(from: &str, to: &str) -> EdgeContract {
        EdgeContract {
            from_node: from.into(),
            from_port: String::new(),
            to_node: to.into(),
            to_port: String::new(),
            proof: CompatibilityProof::default(),
            chain_of_custody: None,
        }
    }

    #[test]
    fn empty_dag_is_coherent() {
        assert!(evaluate(&WorkflowDag::default()).findings.is_empty());
    }

    #[test]
    fn healthy_multi_branch_shape_has_no_findings() {
        // de -> comparison -> final_reporting; validator is a leaf.
        let dag = WorkflowDag {
            id: "ok".into(),
            nodes: vec![
                node("bulk_rnaseq_differential_expression"),
                node("bulk_rnaseq_validate_differential_expression"),
                node("multi_modal_thematic_comparison"),
                node("final_reporting"),
            ],
            edges: vec![
                edge(
                    "bulk_rnaseq_differential_expression",
                    "multi_modal_thematic_comparison",
                ),
                edge("multi_modal_thematic_comparison", "final_reporting"),
            ],
            assumptions: Default::default(),
            source_template: None,
        };
        assert!(
            evaluate(&dag).findings.is_empty(),
            "healthy DAG must be coherent: {:?}",
            evaluate(&dag).findings
        );
    }

    #[test]
    fn stranded_analytical_node_is_flagged() {
        // `orphan_de` produces output nothing consumes, and it is neither
        // the final terminal nor a validator → flagged.
        let dag = WorkflowDag {
            id: "bad".into(),
            nodes: vec![
                node("orphan_de"),
                node("multi_modal_thematic_comparison"),
                node("final_reporting"),
            ],
            edges: vec![edge("multi_modal_thematic_comparison", "final_reporting")],
            assumptions: Default::default(),
            source_template: None,
        };
        let findings = evaluate(&dag).findings;
        assert_eq!(findings.len(), 1, "expected one orphan finding: {findings:?}");
        assert!(findings[0].contains("orphan_de"), "{findings:?}");
    }

    #[test]
    fn validators_and_final_report_are_not_orphans() {
        // A validator leaf + final_reporting leaf must NOT be flagged.
        let dag = WorkflowDag {
            id: "leaves".into(),
            nodes: vec![
                node("proteomics_validate_peptide_search"),
                node("validate_final_reporting"),
                node("final_reporting"),
            ],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };
        assert!(
            evaluate(&dag).findings.is_empty(),
            "validators + final report are expected leaves: {:?}",
            evaluate(&dag).findings
        );
    }
}
