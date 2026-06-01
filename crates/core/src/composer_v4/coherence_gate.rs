//! Coherence gate (Pillar D). A post-selection pass over the chosen
//! `WorkflowDag` that surfaces semantic-incoherence findings as
//! warn/surface signals — never a hard reject (conservative by default
//! so legitimate cross-omics work is not flagged).
//!
//! Phase 1 shipped the seam (a no-op invoked from `plan()`'s multi-branch
//! dispatch beside `policy_gate::evaluate`). This pass implements two
//! detectors, both warn/surface-only (never a hard reject):
//!
//! 1. **orphan-strand**: an analytical node whose output is never
//!    consumed (no outgoing edge) and which is not a legitimate terminal
//!    (the final report) or a leaf companion (a `validate_*` validator).
//!    Such a node represents analysis whose result never flows into the
//!    report — the "analytical atoms orphaned from the goal" case from
//!    the design. This detector reads only the DAG, so it cannot
//!    false-positive on legitimately-shared atoms (e.g.
//!    `data_acquisition`, `reporting`).
//!
//! 2. **modality-mismatch** (Pillar D): a node whose backing atom is
//!    affiliated — via the archetype catalog — ONLY with modality
//!    pipelines unrelated to the requested analysis. This reuses Pillar
//!    B's single source of truth for affiliation,
//!    [`crate::composer_v4::planner::off_modality_node_ids`] (the exact
//!    node set that drives `goal_relevance_penalty`), so it is calibrated
//!    by construction: it cannot false-positive where the penalty is 0
//!    (the 83 green composer_v4 tests prove that set is empty on coherent
//!    DAGs). When the requested modality set is empty (vague goal) it
//!    yields zero findings, so it never flags legitimate cross-omics or
//!    generic/shared atoms.

use crate::archetype_registry::ArchetypeRegistry;
use crate::composer_v4::PlanningContext;
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

/// Evaluate a chosen `WorkflowDag` for coherence. Surfaces two families
/// of warn/surface-only findings (never a hard reject):
///
/// - **orphan_strand**: a node with no outgoing edge that is neither the
///   final reporting terminal nor a validator leaf — analytical output
///   that never reaches the report.
/// - **modality_mismatch** (Pillar D): a node whose backing atom is
///   affiliated only with modality pipelines unrelated to the requested
///   analysis. Computed by reusing Pillar B's
///   [`crate::composer_v4::planner::off_modality_node_ids`] — the same
///   node set that drives `goal_relevance_penalty` — so it cannot
///   false-positive where the penalty is 0.
///
/// Conservative: a healthy DAG (every analytical terminal wired into the
/// join → report, every atom on-modality / generic / synthesis) yields
/// zero findings. The merged finding list is sorted/deterministic.
pub(crate) fn evaluate(
    dag: &WorkflowDag,
    ctx: &PlanningContext,
    archetype_reg: &ArchetypeRegistry,
) -> CoherenceEvaluation {
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
    // Modality-mismatch detector (Pillar D) — reuse Pillar B's affiliation
    // source of truth; one finding per off-modality node id.
    for id in crate::composer_v4::planner::off_modality_node_ids(dag, ctx, archetype_reg) {
        findings.push(format!(
            "modality_mismatch: node '{id}' draws on an atom affiliated only with \
             modality pipelines unrelated to the requested analysis"
        ));
    }
    findings.sort();
    CoherenceEvaluation { findings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
    use crate::workflow_contracts::task_node::TaskNode;
    use std::path::Path;

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
    /// Build a `TaskNode` whose backing atom resolves to `atom_id` via the
    /// lift-stamped `attributes["atom_id"]` key — mirrors the real
    /// lowering path's resolution order in `off_modality_node_ids`.
    fn atom_node(id: &str, atom_id: &str) -> TaskNode {
        let mut n = TaskNode::skeleton(id, "test node");
        n.machine_name = atom_id.to_string();
        n.attributes
            .insert("atom_id".into(), serde_json::Value::String(atom_id.into()));
        n
    }
    fn archetypes() -> ArchetypeRegistry {
        ArchetypeRegistry::load_from_dir(Path::new("../../config/archetypes")).unwrap()
    }

    #[test]
    fn empty_dag_is_coherent() {
        assert!(evaluate(
            &WorkflowDag::default(),
            &PlanningContext::default(),
            &archetypes(),
        )
        .findings
        .is_empty());
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
            evaluate(&dag, &PlanningContext::default(), &archetypes())
                .findings
                .is_empty(),
            "healthy DAG must be coherent: {:?}",
            evaluate(&dag, &PlanningContext::default(), &archetypes()).findings
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
        let findings = evaluate(&dag, &PlanningContext::default(), &archetypes()).findings;
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
            evaluate(&dag, &PlanningContext::default(), &archetypes())
                .findings
                .is_empty(),
            "validators + final report are expected leaves: {:?}",
            evaluate(&dag, &PlanningContext::default(), &archetypes()).findings
        );
    }

    /// Pillar D — a node whose backing atom (`vdj_reconstruction`) is
    /// affiliated only with the `single_cell_vdj` archetype must be
    /// flagged `modality_mismatch` when the request is `bulk_rnaseq`,
    /// while an on-modality companion (branch-prefixed
    /// `bulk_rnaseq_differential_expression`) is NOT flagged.
    #[test]
    fn modality_mismatch_flags_off_modality_atom() {
        use crate::workflow_contracts::workflow_intent::WorkflowIntent;

        let mut ctx = PlanningContext::default();
        ctx.intent = WorkflowIntent {
            modality: Some("bulk_rnaseq".into()),
            ..Default::default()
        };

        // On-modality (branch-prefixed) DE node + an off-modality
        // vdj_reconstruction node. Edge so neither is an orphan strand —
        // we want to isolate the modality detector.
        let dag = WorkflowDag {
            id: "mixed".into(),
            nodes: vec![
                atom_node(
                    "bulk_rnaseq_differential_expression",
                    "differential_expression",
                ),
                atom_node("vdj_reconstruction", "vdj_reconstruction"),
                node("final_reporting"),
            ],
            edges: vec![
                edge("bulk_rnaseq_differential_expression", "final_reporting"),
                edge("vdj_reconstruction", "final_reporting"),
            ],
            assumptions: Default::default(),
            source_template: None,
        };
        let findings = evaluate(&dag, &ctx, &archetypes()).findings;
        let modality: Vec<&String> = findings
            .iter()
            .filter(|f| f.starts_with("modality_mismatch:"))
            .collect();
        assert_eq!(
            modality.len(),
            1,
            "exactly one modality_mismatch finding expected: {findings:?}"
        );
        assert!(
            modality[0].contains("vdj_reconstruction"),
            "off-modality node must be the flagged one: {findings:?}"
        );
        assert!(
            !findings
                .iter()
                .any(|f| f.starts_with("modality_mismatch:")
                    && f.contains("bulk_rnaseq_differential_expression")),
            "on-modality branch node must NOT be flagged: {findings:?}"
        );
    }

    /// Coherent companion case — the same DAG with NO requested modality
    /// (empty R) yields ZERO modality findings (the detector cannot judge
    /// affiliation and never flags), matching Pillar B's penalty=0
    /// invariant. The orphan-strand path is also clean here.
    #[test]
    fn modality_mismatch_silent_when_no_modality_requested() {
        let dag = WorkflowDag {
            id: "coherent".into(),
            nodes: vec![
                atom_node("vdj_reconstruction", "vdj_reconstruction"),
                node("final_reporting"),
            ],
            edges: vec![edge("vdj_reconstruction", "final_reporting")],
            assumptions: Default::default(),
            source_template: None,
        };
        let findings = evaluate(&dag, &PlanningContext::default(), &archetypes()).findings;
        assert!(
            !findings.iter().any(|f| f.starts_with("modality_mismatch:")),
            "empty requested-modality set must produce zero modality findings: {findings:?}"
        );
    }
}
