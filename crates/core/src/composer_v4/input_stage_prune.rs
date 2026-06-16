//! Input-stage-aware pruning.
//!
//! When the SME supplies a processed data product directly ("counts matrix
//! already prepared… no raw FASTQs"), the declared product is surfaced on
//! `intent.available_data`. The lifted archetype scaffold, however, still
//! contains the full upstream chain that would *produce* that product
//! (`raw_qc → sequence_trimming → alignment → quantification` for counts). Those
//! tasks have no inputs (the SME has counts, not reads), so they block at
//! runtime and strand everything downstream.
//!
//! This pass drops the redundant producing-chain and rewires the first real
//! consumer to the data-staging anchor (`data_acquisition` / `data_import`),
//! which now stages the supplied product. It runs immediately after
//! `lift_to_workflow_dag` and BEFORE companion synthesis, so no
//! `discover_*`/`validate_*` companions are ever created for pruned tasks.

use crate::workflow_contracts::data_product::DataProductContract;
use crate::workflow_contracts::edge::EdgeContract;
use crate::workflow_contracts::semantic_type::SemanticType;
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};
use std::collections::BTreeSet;

/// Staging anchors that ingest SME-supplied data. Never pruned; the rewire
/// target for a supplied product.
const SUPPLY_ANCHORS: &[&str] = &["data_acquisition", "data_import"];

/// FASTQ / raw sequence reads — the default raw input seed, never a "supplied
/// processed product" worth pruning toward.
const RAW_INPUT_IRI: &str = "data:2044";

fn type_iri(st: &SemanticType) -> Option<&str> {
    match st {
        SemanticType::OntologyTerm { iri, .. } => Some(iri.as_str()),
        _ => None,
    }
}

fn product_iri(p: &DataProductContract) -> Option<&str> {
    type_iri(&p.semantic_type)
}

/// Does node `n` expose an OUTPUT port of semantic type `iri`?
fn node_produces(n: &TaskNode, iri: &str) -> bool {
    n.outputs
        .iter()
        .any(|o| type_iri(&o.semantic_type) == Some(iri))
}

/// Transitive ancestors of `target` (every node with a forward path to it).
fn ancestors(target: &str, edges: &[EdgeContract]) -> BTreeSet<String> {
    let mut anc = BTreeSet::new();
    let mut stack = vec![target.to_string()];
    while let Some(cur) = stack.pop() {
        for e in edges.iter().filter(|e| e.to_node == cur) {
            if anc.insert(e.from_node.clone()) {
                stack.push(e.from_node.clone());
            }
        }
    }
    anc
}

/// Prune the redundant producing-chain for every supplied product in
/// `available`. Returns the ids removed (for assumption-ledger logging).
pub fn prune_supplied_upstream(
    dag: &mut WorkflowDag,
    available: &[DataProductContract],
) -> Vec<String> {
    let mut removed_all = Vec::new();
    for product in available {
        let Some(iri) = product_iri(product) else {
            continue;
        };
        if iri == RAW_INPUT_IRI {
            continue;
        }
        // Producer candidates: non-anchor nodes exposing this output type.
        let producers: Vec<String> = dag
            .nodes
            .iter()
            .filter(|n| node_produces(n, iri) && !SUPPLY_ANCHORS.contains(&n.id.as_str()))
            .map(|n| n.id.clone())
            .collect();
        if producers.is_empty() {
            continue;
        }
        // Pick the MOST-UPSTREAM producer: the one with no other same-type
        // producer among its ancestors. (A downstream re-producer like a
        // filtered-counts step must not be chosen, or we'd prune the consumer
        // we mean to keep.) Deterministic tie-break by id.
        let mut roots: Vec<String> = producers
            .iter()
            .filter(|c| {
                let anc = ancestors(c, &dag.edges);
                !producers.iter().any(|p| p != *c && anc.contains(p))
            })
            .cloned()
            .collect();
        roots.sort();
        let Some(q) = roots.into_iter().next() else {
            continue;
        };

        // Prune set = Q ∪ ancestors(Q), minus the supply anchors.
        let anc = ancestors(&q, &dag.edges);
        let prune: BTreeSet<String> = anc
            .into_iter()
            .chain(std::iter::once(q.clone()))
            .filter(|id| !SUPPLY_ANCHORS.contains(&id.as_str()))
            .collect();

        // The supply anchor we rewire onto (must exist in the DAG).
        let Some(anchor) = SUPPLY_ANCHORS
            .iter()
            .find(|a| dag.nodes.iter().any(|n| &n.id == *a))
            .map(|a| a.to_string())
        else {
            continue;
        };

        // SAFETY: never over-prune. Every pruned node OTHER than Q must feed
        // only the prune set (Q's own consumers are exempt — they get rewired).
        // If any pruned ancestor branches out to a KEPT task, abort this product.
        let clean = prune.iter().filter(|id| **id != q).all(|id| {
            dag.edges
                .iter()
                .filter(|e| &e.from_node == id)
                .all(|e| prune.contains(&e.to_node))
        });
        if !clean {
            continue;
        }

        // Copy Q's producing output port onto the anchor so the rewired
        // consumer edges keep a typed source.
        if let Some(port) = dag.nodes.iter().find(|n| n.id == q).and_then(|n| {
            n.outputs
                .iter()
                .find(|o| type_iri(&o.semantic_type) == Some(iri))
                .cloned()
        }) {
            if let Some(anchor_node) = dag.nodes.iter_mut().find(|n| n.id == anchor) {
                if !node_produces(anchor_node, iri) {
                    anchor_node.outputs.push(port);
                }
            }
        }

        // Rewire Q's consumers onto the anchor.
        for e in dag.edges.iter_mut() {
            if e.from_node == q {
                e.from_node = anchor.clone();
            }
        }

        // Remove the prune set + any `discover_*`/`validate_*` companions for
        // pruned ids (defensive — at the planner call site none exist yet).
        let mut remove = prune.clone();
        for p in &prune {
            for prefix in ["discover_", "validate_"] {
                let companion = format!("{prefix}{p}");
                if dag.nodes.iter().any(|n| n.id == companion) {
                    remove.insert(companion);
                }
            }
        }
        dag.nodes.retain(|n| !remove.contains(&n.id));
        dag.edges
            .retain(|e| !remove.contains(&e.from_node) && !remove.contains(&e.to_node));
        removed_all.extend(remove);
    }
    removed_all.sort();
    removed_all.dedup();
    removed_all
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::edge::{CompatibilityProof, EdgeKind};
    use crate::workflow_contracts::port::PortContract;

    fn out(name: &str, iri: &str) -> PortContract {
        PortContract::with_semantic_type(name, SemanticType::edam(iri, ""))
    }
    fn inp(name: &str, iri: &str) -> PortContract {
        PortContract::with_semantic_type(name, SemanticType::edam(iri, ""))
    }
    fn node(id: &str, inputs: Vec<PortContract>, outputs: Vec<PortContract>) -> TaskNode {
        let mut n = TaskNode::skeleton(id, id);
        n.inputs = inputs;
        n.outputs = outputs;
        n
    }
    fn edge(from: &str, to: &str) -> EdgeContract {
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

    const FASTQ: &str = "data:2044";
    const BAM: &str = "data:2572";
    const COUNTS: &str = "data:3917";
    const DE: &str = "data:0951";
    // The `differential_expression` atom's `de_results` OUTPUT-PORT type
    // (config/stage-atoms/differential_expression.yaml) — the type a supplied
    // pre-computed DE table must carry to match the DE node for pruning.
    const DE_RESULTS: &str = "data:3134";

    /// A full bulk-RNA-seq chain. Supplying counts must drop
    /// raw_qc/alignment/quantification, keep + rewire qc_preprocessing onto
    /// data_acquisition, and leave differential_expression intact.
    #[test]
    fn supplied_counts_prunes_fastq_chain_and_rewires() {
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                node("data_acquisition", vec![], vec![out("staged", "data:2531")]),
                node(
                    "raw_qc",
                    vec![inp("reads", FASTQ)],
                    vec![out("qc", "data:2914")],
                ),
                node(
                    "alignment",
                    vec![inp("reads", FASTQ)],
                    vec![out("bam", BAM)],
                ),
                node(
                    "quantification",
                    vec![inp("bam", BAM)],
                    vec![out("counts", COUNTS)],
                ),
                node(
                    "qc_preprocessing",
                    vec![inp("counts", COUNTS)],
                    vec![out("filtered", COUNTS)],
                ),
                node(
                    "differential_expression",
                    vec![inp("filt", COUNTS)],
                    vec![out("de", DE)],
                ),
            ],
            edges: vec![
                edge("data_acquisition", "raw_qc"),
                edge("raw_qc", "alignment"),
                edge("alignment", "quantification"),
                edge("quantification", "qc_preprocessing"),
                edge("qc_preprocessing", "differential_expression"),
            ],
            ..Default::default()
        };
        let removed =
            prune_supplied_upstream(&mut dag, &[DataProductContract::gene_count_matrix()]);

        let ids: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        // FASTQ chain dropped.
        for gone in ["raw_qc", "alignment", "quantification"] {
            assert!(!ids.contains(gone), "{gone} must be pruned; got {ids:?}");
            assert!(
                removed.iter().any(|r| r == gone),
                "removed should list {gone}"
            );
        }
        // Consumer + analysis + anchor kept.
        for kept in [
            "data_acquisition",
            "qc_preprocessing",
            "differential_expression",
        ] {
            assert!(ids.contains(kept), "{kept} must survive; got {ids:?}");
        }
        // qc_preprocessing rewired onto data_acquisition; the old quantification
        // edge is gone.
        assert!(
            dag.edges
                .iter()
                .any(|e| e.from_node == "data_acquisition" && e.to_node == "qc_preprocessing"),
            "qc_preprocessing must be rewired onto data_acquisition; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.clone(), e.to_node.clone()))
                .collect::<Vec<_>>()
        );
        assert!(
            !dag.edges
                .iter()
                .any(|e| e.from_node == "quantification" || e.to_node == "quantification"),
            "no edges may reference the pruned quantification node"
        );
        // data_acquisition now exposes the counts output type so the rewired
        // edge keeps a typed source.
        let da = dag
            .nodes
            .iter()
            .find(|n| n.id == "data_acquisition")
            .unwrap();
        assert!(
            node_produces(da, COUNTS),
            "data_acquisition must expose the supplied counts type"
        );
    }

    /// No supplied stage (FASTQ default) → no pruning.
    #[test]
    fn raw_fastq_input_is_not_pruned() {
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                node("data_acquisition", vec![], vec![out("reads", FASTQ)]),
                node(
                    "raw_qc",
                    vec![inp("reads", FASTQ)],
                    vec![out("qc", "data:2914")],
                ),
                node(
                    "quantification",
                    vec![inp("bam", BAM)],
                    vec![out("counts", COUNTS)],
                ),
            ],
            edges: vec![
                edge("data_acquisition", "raw_qc"),
                edge("raw_qc", "quantification"),
            ],
            ..Default::default()
        };
        // FASTQ seed (data:2044) must be a no-op even though it's "available".
        let removed =
            prune_supplied_upstream(&mut dag, &[DataProductContract::sample_paired_fastq()]);
        assert!(
            removed.is_empty(),
            "FASTQ default seed must not prune anything"
        );
        assert_eq!(dag.nodes.len(), 3);
    }

    /// A pruned ancestor that ALSO feeds a kept task must abort the prune
    /// (never over-prune a branch).
    #[test]
    fn branch_escape_aborts_prune() {
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                node("data_acquisition", vec![], vec![out("staged", "data:2531")]),
                node(
                    "alignment",
                    vec![inp("reads", FASTQ)],
                    vec![out("bam", BAM)],
                ),
                node(
                    "quantification",
                    vec![inp("bam", BAM)],
                    vec![out("counts", COUNTS)],
                ),
                node(
                    "qc_preprocessing",
                    vec![inp("counts", COUNTS)],
                    vec![out("filt", COUNTS)],
                ),
                // alignment ALSO feeds a kept structural-variant branch → not a clean chain.
                node(
                    "sv_calling",
                    vec![inp("bam", BAM)],
                    vec![out("sv", "data:3498")],
                ),
                node("reporting", vec![inp("sv", "data:3498")], vec![]),
            ],
            edges: vec![
                edge("data_acquisition", "alignment"),
                edge("alignment", "quantification"),
                edge("quantification", "qc_preprocessing"),
                edge("alignment", "sv_calling"),
                edge("sv_calling", "reporting"),
            ],
            ..Default::default()
        };
        let removed =
            prune_supplied_upstream(&mut dag, &[DataProductContract::gene_count_matrix()]);
        assert!(
            removed.is_empty(),
            "alignment feeds a kept SV branch — prune must abort, got {removed:?}"
        );
    }

    /// Supplying CALLED PEAKS (data:1255, the type `detect_input_data_stage`
    /// emits for chip_seq/atac_seq/cut_tag/chip_exo) must drop the
    /// raw_qc→alignment→peak_calling producing chain and rewire the first real
    /// consumer (differential_accessibility) onto data_acquisition. Locks the
    /// COMPOSER-PRUNE class for the PEAKS branch — the same generic
    /// `prune_supplied_upstream` logic the counts test covers, exercised on a
    /// peak-calling-shaped DAG.
    #[test]
    fn supplied_peaks_prunes_peak_calling_chain_and_rewires() {
        const PEAKS: &str = "data:1255";
        const ALIGNED: &str = "data:0863";
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                node("data_acquisition", vec![], vec![out("staged", "data:2531")]),
                node("raw_qc", vec![inp("reads", FASTQ)], vec![out("qc", "data:2914")]),
                node("alignment", vec![inp("reads", FASTQ)], vec![out("bam", ALIGNED)]),
                node("peak_calling", vec![inp("bam", ALIGNED)], vec![out("peaks", PEAKS)]),
                node(
                    "differential_accessibility",
                    vec![inp("peaks", PEAKS)],
                    vec![out("acc", "data:3753")],
                ),
            ],
            edges: vec![
                edge("data_acquisition", "raw_qc"),
                edge("raw_qc", "alignment"),
                edge("alignment", "peak_calling"),
                edge("peak_calling", "differential_accessibility"),
            ],
            ..Default::default()
        };
        let supplied =
            DataProductContract::skeleton("intake_peaks_0", SemanticType::edam(PEAKS, "Called peaks"));
        let removed = prune_supplied_upstream(&mut dag, &[supplied]);
        let ids: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        for gone in ["raw_qc", "alignment", "peak_calling"] {
            assert!(!ids.contains(gone), "{gone} must be pruned; got {ids:?}");
            assert!(removed.iter().any(|r| r == gone), "removed should list {gone}");
        }
        for kept in ["data_acquisition", "differential_accessibility"] {
            assert!(ids.contains(kept), "{kept} must survive; got {ids:?}");
        }
        assert!(
            dag.edges.iter().any(|e| e.from_node == "data_acquisition"
                && e.to_node == "differential_accessibility"),
            "differential_accessibility must be rewired onto data_acquisition; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.clone(), e.to_node.clone()))
                .collect::<Vec<_>>()
        );
        let da = dag.nodes.iter().find(|n| n.id == "data_acquisition").unwrap();
        assert!(
            node_produces(da, PEAKS),
            "data_acquisition must expose the supplied called-peaks type"
        );
    }

    /// Supplying ALIGNED READS (data:0863 BAM, the modality-independent type
    /// `detect_input_data_stage` emits for any read-based pipeline) must drop the
    /// raw_qc→sequence_trimming→alignment chain and rewire the first real
    /// consumer (variant_calling) onto data_acquisition. Locks the COMPOSER-PRUNE
    /// class for the BAM branch.
    #[test]
    fn supplied_bam_prunes_alignment_chain_and_rewires() {
        const ALIGNED: &str = "data:0863";
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                node("data_acquisition", vec![], vec![out("staged", "data:2531")]),
                node("raw_qc", vec![inp("reads", FASTQ)], vec![out("qc", "data:2914")]),
                node(
                    "sequence_trimming",
                    vec![inp("reads", FASTQ)],
                    vec![out("trimmed", FASTQ)],
                ),
                node("alignment", vec![inp("reads", FASTQ)], vec![out("bam", ALIGNED)]),
                node(
                    "variant_calling",
                    vec![inp("bam", ALIGNED)],
                    vec![out("vcf", "data:3498")],
                ),
            ],
            edges: vec![
                edge("data_acquisition", "raw_qc"),
                edge("raw_qc", "sequence_trimming"),
                edge("sequence_trimming", "alignment"),
                edge("alignment", "variant_calling"),
            ],
            ..Default::default()
        };
        let supplied = DataProductContract::skeleton(
            "intake_alignment_0",
            SemanticType::edam(ALIGNED, "Sequence alignment"),
        );
        let removed = prune_supplied_upstream(&mut dag, &[supplied]);
        let ids: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        for gone in ["raw_qc", "sequence_trimming", "alignment"] {
            assert!(!ids.contains(gone), "{gone} must be pruned; got {ids:?}");
            assert!(removed.iter().any(|r| r == gone), "removed should list {gone}");
        }
        for kept in ["data_acquisition", "variant_calling"] {
            assert!(ids.contains(kept), "{kept} must survive; got {ids:?}");
        }
        assert!(
            dag.edges
                .iter()
                .any(|e| e.from_node == "data_acquisition" && e.to_node == "variant_calling"),
            "variant_calling must be rewired onto data_acquisition; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.clone(), e.to_node.clone()))
                .collect::<Vec<_>>()
        );
        let da = dag.nodes.iter().find(|n| n.id == "data_acquisition").unwrap();
        assert!(
            node_produces(da, ALIGNED),
            "data_acquisition must expose the supplied alignment type"
        );
    }

    /// D4 (BiomniBench da-15-8): the SME supplies a PRE-COMPUTED
    /// differential-expression results table (proteomics XLSX + a DE results
    /// TSV, NO FASTQ) that misroutes into the bulk_rnaseq raw-read pipeline.
    /// Supplying DE results (`data:3134`, the `differential_expression` node
    /// OUTPUT-PORT type) must drop the ENTIRE raw-read chain THROUGH
    /// differential_expression (`rnaseq_raw_qc → alignment → quantification →
    /// differential_expression`) and rewire the surviving consumer
    /// (`pathway_enrichment`) onto data_acquisition — so nothing strands on
    /// NoUpstreamSequencingSubstrate / the differential_expression validation
    /// contract. Locks the COMPOSER-PRUNE class for the DE-RESULTS branch.
    #[test]
    fn supplied_de_results_prunes_rawread_chain_through_de_and_rewires() {
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                node("data_acquisition", vec![], vec![out("staged", "data:2531")]),
                node(
                    "rnaseq_raw_qc",
                    vec![inp("reads", FASTQ)],
                    vec![out("qc", "data:2914")],
                ),
                node(
                    "alignment",
                    vec![inp("reads", FASTQ)],
                    vec![out("bam", BAM)],
                ),
                node(
                    "quantification",
                    vec![inp("bam", BAM)],
                    vec![out("counts", COUNTS)],
                ),
                node(
                    "differential_expression",
                    vec![inp("counts", COUNTS)],
                    vec![out("de", DE_RESULTS)],
                ),
                node(
                    "pathway_enrichment",
                    vec![inp("de", DE_RESULTS)],
                    vec![out("pathways", "data:3753")],
                ),
            ],
            edges: vec![
                edge("data_acquisition", "rnaseq_raw_qc"),
                edge("rnaseq_raw_qc", "alignment"),
                edge("alignment", "quantification"),
                edge("quantification", "differential_expression"),
                edge("differential_expression", "pathway_enrichment"),
            ],
            ..Default::default()
        };
        // The supplied DE table is typed `data:3134` (the DE node OUTPUT port,
        // the prune-match target), NOT the archetype goal type `data:0951`.
        let supplied = DataProductContract::skeleton(
            "intake_de_results_0",
            SemanticType::edam(DE_RESULTS, "Gene expression data"),
        );
        let removed = prune_supplied_upstream(&mut dag, &[supplied]);
        let ids: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        // The whole raw-read chain THROUGH differential_expression is dropped.
        for gone in [
            "rnaseq_raw_qc",
            "alignment",
            "quantification",
            "differential_expression",
        ] {
            assert!(!ids.contains(gone), "{gone} must be pruned; got {ids:?}");
            assert!(
                removed.iter().any(|r| r == gone),
                "removed should list {gone}"
            );
        }
        // Anchor + surviving DE consumer kept.
        for kept in ["data_acquisition", "pathway_enrichment"] {
            assert!(ids.contains(kept), "{kept} must survive; got {ids:?}");
        }
        // pathway_enrichment rewired onto data_acquisition (which now stages
        // the supplied DE table); no edge references the pruned DE node.
        assert!(
            dag.edges
                .iter()
                .any(|e| e.from_node == "data_acquisition" && e.to_node == "pathway_enrichment"),
            "pathway_enrichment must be rewired onto data_acquisition; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.clone(), e.to_node.clone()))
                .collect::<Vec<_>>()
        );
        assert!(
            !dag.edges.iter().any(|e| e.from_node == "differential_expression"
                || e.to_node == "differential_expression"),
            "no edges may reference the pruned differential_expression node"
        );
        let da = dag
            .nodes
            .iter()
            .find(|n| n.id == "data_acquisition")
            .unwrap();
        assert!(
            node_produces(da, DE_RESULTS),
            "data_acquisition must expose the supplied DE-results type"
        );
    }
}
