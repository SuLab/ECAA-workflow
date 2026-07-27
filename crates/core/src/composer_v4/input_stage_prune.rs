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
use std::collections::{BTreeMap, BTreeSet};

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

/// Does a producer OUTPUT port of semantic type `st` supply the
/// SME-supplied product typed by ontology IRI `iri`?
///
/// True when:
/// - the port is an `OntologyTerm` whose IRI equals `iri` (the exact-term
///   match that already drove counts/peaks/VCF/BAM/DE-results pruning), OR
/// - the port is a `LocalExtension` whose `proposed_parent_terms` CONTAINS
///   `iri` — i.e. the port is a local subtype of the supplied ontology term.
///   This is the proteomics case: `protein_quantification`'s output port is
///   `ecaax:protein_abundance_matrix` with `proposed_parent_terms: [data:2976]`,
///   so a seeded `data:2976` (protein abundance matrix) must match it to prune
///   the search→quantify chain, even though the port carries no bare IRI.
///
/// Deliberately NARROW: this subsumption is applied ONLY on the supplied-
/// product → producer-port direction used for input-stage pruning. It is a
/// one-directional "the supplied ontology term is (an ancestor of) what this
/// port produces" test — it does not touch `SemanticType`'s general
/// type-equality / edge-compatibility logic, and it never fires for the raw
/// default (`data:2044` is guarded out before any port match).
fn port_supplies_iri(st: &SemanticType, iri: &str) -> bool {
    match st {
        SemanticType::OntologyTerm { iri: port_iri, .. } => port_iri == iri,
        SemanticType::LocalExtension {
            proposed_parent_terms,
            ..
        } => proposed_parent_terms.iter().any(|p| p == iri),
        _ => false,
    }
}

/// Does node `n` expose an OUTPUT port that supplies ontology type `iri`
/// (exact ontology term, or a local extension proposing it as a parent)?
fn node_produces(n: &TaskNode, iri: &str) -> bool {
    n.outputs
        .iter()
        .any(|o| port_supplies_iri(&o.semantic_type, iri))
}

/// Tag prefix marking a `CompatibilityProof.warnings` row that records a
/// producer-port rename applied by the rewire below.
///
/// `EdgeContract` carries no dedicated alias field, and the emitted
/// `ecaax:PortAlias` entities (`crate::ro_crate`) copy `from_port` verbatim,
/// so the pre-rewire name would be lost the moment the rewire makes the edge
/// resolvable. The proof's non-blocking `warnings` channel is the existing
/// per-edge annotation surface for structural composer rewrites (see
/// `multi_branch_synthesis`, which tags its ordering joins the same way), so
/// the alias rides there as a parseable `key=value` row rather than growing
/// the wire-facing `EdgeContract` schema.
pub const PORT_ALIAS_TAG: &str = "input_stage_prune_port_alias";

/// One producer-port rename recorded on a rewired edge: which pruned producer
/// the edge used to name, the port name it carried before the rewire, and the
/// staging anchor's own port the rewire resolved it onto.
///
/// Both names are retained so a reviewer reading the emitted package can join
/// the anchor's canonical port back to the atom contract the edge originally
/// referenced (`quantification.count_matrix` /
/// `qc_preprocessing.filtered_count_matrix` → `data_acquisition.raw_count_matrix`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PortAlias {
    /// Id of the pruned producer task whose output port the edge named.
    pub pruned_producer: String,
    /// The pre-rewire `from_port` — a port the anchor does not declare.
    pub original_port: String,
    /// The anchor's own output port the rewire resolved the edge onto.
    pub canonical_port: String,
}

impl PortAlias {
    /// Render as the single `warnings` row carried on the rewired edge.
    pub fn encode(&self) -> String {
        format!(
            "{PORT_ALIAS_TAG}: pruned_producer={} original_port={} canonical_port={}",
            self.pruned_producer, self.original_port, self.canonical_port
        )
    }

    /// Inverse of [`PortAlias::encode`]. `None` for any row that is not a
    /// port-alias record or that is missing a field, so unrelated proof
    /// warnings pass through untouched.
    pub fn decode(row: &str) -> Option<Self> {
        let rest = row.strip_prefix(PORT_ALIAS_TAG)?.strip_prefix(':')?;
        let mut pruned_producer: Option<String> = None;
        let mut original_port: Option<String> = None;
        let mut canonical_port: Option<String> = None;
        for field in rest.split_whitespace() {
            let (key, value) = field.split_once('=')?;
            match key {
                "pruned_producer" => pruned_producer = Some(value.to_string()),
                "original_port" => original_port = Some(value.to_string()),
                "canonical_port" => canonical_port = Some(value.to_string()),
                _ => return None,
            }
        }
        Some(Self {
            pruned_producer: pruned_producer?,
            original_port: original_port?,
            canonical_port: canonical_port?,
        })
    }
}

/// Every port-alias record retained on `edge`; empty for an edge this pass
/// never rewired. The order mirrors the order the aliases were recorded.
pub fn port_aliases(edge: &EdgeContract) -> Vec<PortAlias> {
    edge.proof
        .warnings
        .iter()
        .filter_map(|w| PortAlias::decode(w))
        .collect()
}

/// Resolve the staging anchor's OWN output port for an edge being rewired off
/// the pruned producer.
///
/// Moving `from_node` alone is not enough: the edge still names the
/// PRODUCER's port. When the port-copy above was suppressed — the anchor
/// already produces the supplied IRI under a different name
/// (`data_acquisition.raw_count_matrix` for `data:3917`) — the rewired edge is
/// left pointing at a port its new producer does not declare, which nothing
/// downstream can resolve.
///
/// Resolution order, first match wins:
/// 1. the anchor already declares a port with that exact name (authored, or
///    just pushed by the port-copy) — nothing to rewrite;
/// 2. an anchor port whose semantic type is identical to the producer port's;
/// 3. an anchor port supplying the pruned product type `iri` — exact
///    ontology-term match preferred over local-extension subsumption.
///
/// Returns `None` — leaving `from_port` untouched — when the name is not a
/// declared output of the producer at all. Those are the composer's synthetic
/// ordering-edge sentinels (`report` / `literature` / `_excluded_rewire` /
/// `splice` / the empty string), which are workflow-ordering relations rather
/// than port-typed data flows and must not be coerced onto a data port. Step 3
/// is likewise skipped for a producer port that does not supply `iri`, so an
/// unrelated side output is never aliased to the supplied product.
///
/// Ties break on the anchor's declaration order — the YAML order the registry
/// loads — so the choice is deterministic.
fn resolve_anchor_port(
    anchor: &TaskNode,
    producer: &TaskNode,
    original_port: &str,
    iri: &str,
) -> Option<String> {
    if anchor.outputs.iter().any(|o| o.name == original_port) {
        return Some(original_port.to_string());
    }
    let producer_port = producer.outputs.iter().find(|o| o.name == original_port)?;
    let producer_type = producer_port.semantic_type.stable_id();
    if let Some(same_type) = anchor
        .outputs
        .iter()
        .find(|o| o.semantic_type.stable_id() == producer_type)
    {
        return Some(same_type.name.clone());
    }
    if !port_supplies_iri(&producer_port.semantic_type, iri) {
        return None;
    }
    anchor
        .outputs
        .iter()
        .find(|o| matches!(&o.semantic_type, SemanticType::OntologyTerm { iri: i, .. } if i == iri))
        .or_else(|| {
            anchor
                .outputs
                .iter()
                .find(|o| port_supplies_iri(&o.semantic_type, iri))
        })
        .map(|o| o.name.clone())
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
                .find(|o| port_supplies_iri(&o.semantic_type, iri))
                .cloned()
        }) {
            if let Some(anchor_node) = dag.nodes.iter_mut().find(|n| n.id == anchor) {
                if !node_produces(anchor_node, iri) {
                    anchor_node.outputs.push(port);
                }
            }
        }

        // Rewire Q's consumers onto the anchor. Reassigning `from_node` alone
        // is not enough: the edge still names Q's OWN output port, which the
        // anchor generally does not declare once the port-copy above was
        // suppressed (`data_acquisition` already produces `data:3917` as
        // `raw_count_matrix`, so a rewired edge kept `quantification`'s
        // `count_matrix` / `qc_preprocessing`'s `filtered_count_matrix` and
        // resolved against nothing downstream). Resolve the anchor's own
        // canonical port per distinct producer-port name, then retain the
        // pre-rewire name as a `PortAlias` on the edge's proof so reviewer
        // traceability back to the pruned atom contract survives.
        //
        // Keyed in a `BTreeMap` so the resolution is computed once per port
        // name in a deterministic order, and resolved BEFORE the mutable edge
        // pass because both endpoints are read off `dag.nodes`.
        let renames: BTreeMap<String, String> = match (
            dag.nodes.iter().find(|n| n.id == q),
            dag.nodes.iter().find(|n| n.id == anchor),
        ) {
            (Some(q_node), Some(anchor_node)) => dag
                .edges
                .iter()
                .filter(|e| e.from_node == q)
                .map(|e| e.from_port.clone())
                .collect::<BTreeSet<String>>()
                .into_iter()
                .filter_map(|original| {
                    let canonical = resolve_anchor_port(anchor_node, q_node, &original, iri)?;
                    (canonical != original).then_some((original, canonical))
                })
                .collect(),
            _ => BTreeMap::new(),
        };
        for e in dag.edges.iter_mut() {
            if e.from_node != q {
                continue;
            }
            let canonical = renames.get(&e.from_port).cloned();
            if let Some(canonical) = canonical {
                let row = PortAlias {
                    pruned_producer: q.clone(),
                    original_port: e.from_port.clone(),
                    canonical_port: canonical.clone(),
                }
                .encode();
                // Idempotent: the pass runs twice (seed lift + primary
                // finalize), and a re-resolved edge must not stack duplicates.
                if !e.proof.warnings.contains(&row) {
                    e.proof.warnings.push(row);
                }
                e.from_port = canonical;
            }
            e.from_node = anchor.clone();
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
    /// A `local_extension` output port proposing `parent_iri` as a parent
    /// term — mirrors the `protein_quantification` atom's
    /// `ecaax:protein_abundance_matrix` output (`proposed_parent_terms:
    /// [data:2976]`).
    fn out_local_ext(name: &str, ext_id: &str, parent_iri: &str) -> PortContract {
        PortContract::with_semantic_type(
            name,
            SemanticType::LocalExtension {
                namespace: "ecaax".into(),
                id: ext_id.into(),
                proposed_parent_terms: vec![parent_iri.into()],
                definition: String::new(),
                maturity: crate::workflow_contracts::semantic_type::default_minted(),
            },
        )
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
            mutually_exclusive_group: None,
        }
    }
    /// An edge naming a REAL producer output port, as the archetype-lift path
    /// builds them (`planner::pick_best_port_pair` binds the producer's own
    /// port name). The bare `edge()` helper above uses a `"out"` placeholder
    /// no fixture node declares.
    fn edge_ports(from: &str, from_port: &str, to: &str, to_port: &str) -> EdgeContract {
        EdgeContract {
            from_port: from_port.into(),
            to_port: to_port.into(),
            ..edge(from, to)
        }
    }
    /// The real `data_acquisition` output shape
    /// (`config/stage-atoms/data_acquisition.yaml`): a manifest, an OPTIONAL
    /// raw count matrix typed `data:3917`, and raw reads. The middle port is
    /// what makes the port-copy in `prune_supplied_upstream` a no-op on the
    /// counts-first path.
    fn data_acquisition_node() -> TaskNode {
        node(
            "data_acquisition",
            vec![],
            vec![
                out("cohort_manifest", "data:2531"),
                out("raw_count_matrix", COUNTS),
                out("raw_reads", FASTQ),
            ],
        )
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

    /// REGRESSION (Task B guard): the new local-extension subsumption must NOT
    /// change ontology-term pruning. Supplying counts (`data:3917`, a bare
    /// OntologyTerm) prunes an ONTOLOGY-TERM `quantification` producer exactly
    /// as before, and a proteomics `data:2976` seed must NOT prune this
    /// counts chain (its output ports carry no `data:2976` proposed parent).
    #[test]
    fn ontology_term_pruning_unchanged_by_local_extension_extension() {
        let build = || WorkflowDag {
            id: "t".into(),
            nodes: vec![
                node("data_acquisition", vec![], vec![out("staged", "data:2531")]),
                node(
                    "quantification",
                    vec![inp("bam", BAM)],
                    vec![out("counts", COUNTS)],
                ),
                node(
                    "differential_expression",
                    vec![inp("counts", COUNTS)],
                    vec![out("de", DE)],
                ),
            ],
            edges: vec![
                edge("data_acquisition", "quantification"),
                edge("quantification", "differential_expression"),
            ],
            ..Default::default()
        };
        // Counts (bare ontology term) still prunes the ontology-term producer.
        let mut dag = build();
        let removed =
            prune_supplied_upstream(&mut dag, &[DataProductContract::gene_count_matrix()]);
        assert!(
            removed.iter().any(|r| r == "quantification"),
            "ontology-term counts seed must still prune quantification; got {removed:?}"
        );
        // A proteomics data:2976 seed must NOT touch a chain whose ports never
        // propose data:2976 as a parent — the subsumption is IRI-scoped.
        let mut dag2 = build();
        let supplied = DataProductContract::skeleton(
            "intake_supplied_data_2976",
            SemanticType::edam("data:2976", "Protein abundance matrix"),
        );
        let removed2 = prune_supplied_upstream(&mut dag2, &[supplied]);
        assert!(
            removed2.is_empty(),
            "data:2976 must not prune a counts chain lacking that parent term; got {removed2:?}"
        );
        assert_eq!(dag2.nodes.len(), 3, "no counts node may be removed");
    }

    /// TASK B: supplying a PROTEOMICS abundance matrix (`data:2976`) must prune
    /// the search→quantify chain (`peptide_search → protein_quantification`)
    /// even though `protein_quantification`'s output port is a
    /// `local_extension` (`ecaax:protein_abundance_matrix`) proposing
    /// `data:2976` as its parent — NOT a bare ontology term. Before the
    /// local-extension subsumption, `node_produces` returned `None` for that
    /// port, so seeding `data:2976` was a silent no-op and the search→quantify
    /// chain stranded. The surviving consumer (`differential_expression`)
    /// rewires onto data_acquisition, which now exposes the supplied product.
    #[test]
    fn supplied_proteomics_matrix_prunes_search_quantify_chain_and_rewires() {
        const PROTEIN_ABUNDANCE: &str = "data:2976";
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                node("data_acquisition", vec![], vec![out("staged", "data:2531")]),
                node(
                    "peptide_search",
                    vec![inp("spectra", "data:2536")],
                    vec![out("psms", "data:2537")],
                ),
                node(
                    "protein_quantification",
                    vec![inp("psms", "data:2537")],
                    // The load-bearing shape: output is a local_extension whose
                    // proposed parent is the supplied data:2976 term.
                    vec![out_local_ext(
                        "protein_abundance",
                        "protein_abundance_matrix",
                        PROTEIN_ABUNDANCE,
                    )],
                ),
                node(
                    "differential_expression",
                    vec![inp("abundance", PROTEIN_ABUNDANCE)],
                    vec![out("de", DE)],
                ),
            ],
            edges: vec![
                edge("data_acquisition", "peptide_search"),
                edge("peptide_search", "protein_quantification"),
                edge("protein_quantification", "differential_expression"),
            ],
            ..Default::default()
        };
        // The supplied product is typed as the bare ontology term data:2976 (as
        // the dispatcher seeds it), matching the producer's local_extension
        // parent.
        let supplied = DataProductContract::skeleton(
            "intake_supplied_data_2976",
            SemanticType::edam(PROTEIN_ABUNDANCE, "Protein abundance matrix"),
        );
        let removed = prune_supplied_upstream(&mut dag, &[supplied]);
        let ids: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        for gone in ["peptide_search", "protein_quantification"] {
            assert!(!ids.contains(gone), "{gone} must be pruned; got {ids:?}");
            assert!(
                removed.iter().any(|r| r == gone),
                "removed should list {gone}; got {removed:?}"
            );
        }
        for kept in ["data_acquisition", "differential_expression"] {
            assert!(ids.contains(kept), "{kept} must survive; got {ids:?}");
        }
        assert!(
            dag.edges.iter().any(|e| e.from_node == "data_acquisition"
                && e.to_node == "differential_expression"),
            "differential_expression must be rewired onto data_acquisition; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.clone(), e.to_node.clone()))
                .collect::<Vec<_>>()
        );
        assert!(
            !dag.edges.iter().any(|e| e.from_node == "protein_quantification"
                || e.to_node == "protein_quantification"),
            "no edges may reference the pruned protein_quantification node"
        );
        // data_acquisition now exposes the supplied local-extension product port
        // so the rewired edge keeps a typed source.
        let da = dag
            .nodes
            .iter()
            .find(|n| n.id == "data_acquisition")
            .unwrap();
        assert!(
            node_produces(da, PROTEIN_ABUNDANCE),
            "data_acquisition must expose the supplied protein-abundance type"
        );
    }

    /// The counts-first shape that produced an unresolvable `from_port` in a
    /// real deposit: the anchor ALREADY produces `data:3917` under its own
    /// name (`raw_count_matrix`), so the port-copy is suppressed — and the
    /// rewired edge used to keep `quantification`'s `count_matrix`, a port
    /// `data_acquisition` does not declare. The rewire must resolve the
    /// anchor's canonical port instead, and retain the original name.
    #[test]
    fn rewire_resolves_from_port_to_anchor_canonical_port_when_copy_is_suppressed() {
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                data_acquisition_node(),
                node(
                    "alignment",
                    vec![inp("reads", FASTQ)],
                    vec![out("bam", BAM)],
                ),
                node(
                    "quantification",
                    vec![inp("bam", BAM)],
                    vec![out("count_matrix", COUNTS)],
                ),
                node(
                    "differential_expression",
                    vec![inp("counts", COUNTS)],
                    vec![out("de", DE)],
                ),
            ],
            edges: vec![
                edge_ports("data_acquisition", "raw_reads", "alignment", "reads"),
                edge_ports("alignment", "bam", "quantification", "bam"),
                edge_ports(
                    "quantification",
                    "count_matrix",
                    "differential_expression",
                    "counts",
                ),
            ],
            ..Default::default()
        };
        // Pre-condition: the anchor already supplies the counts type, so the
        // port-copy branch is a no-op and only the rename can fix the edge.
        let da_before = dag
            .nodes
            .iter()
            .find(|n| n.id == "data_acquisition")
            .expect("anchor present");
        assert!(
            node_produces(da_before, COUNTS),
            "fixture must reproduce the anchor-already-produces-counts case"
        );

        prune_supplied_upstream(&mut dag, &[DataProductContract::gene_count_matrix()]);

        let rewired = dag
            .edges
            .iter()
            .find(|e| e.from_node == "data_acquisition" && e.to_node == "differential_expression")
            .expect("differential_expression must be rewired onto data_acquisition");
        assert_eq!(
            rewired.from_port, "raw_count_matrix",
            "rewired edge must name the anchor's OWN counts port, not the pruned \
             producer's `count_matrix`; got {rewired:?}"
        );

        let declared: BTreeSet<&str> = dag
            .nodes
            .iter()
            .find(|n| n.id == "data_acquisition")
            .expect("anchor survives")
            .outputs
            .iter()
            .map(|o| o.name.as_str())
            .collect();
        let anchored = dag
            .edges
            .iter()
            .filter(|e| e.from_node == "data_acquisition");
        for e in anchored {
            assert!(
                declared.contains(e.from_port.as_str()),
                "edge {e:?} names a from_port data_acquisition does not declare; declared={declared:?}"
            );
        }

        let aliases = port_aliases(rewired);
        assert_eq!(
            aliases,
            vec![PortAlias {
                pruned_producer: "quantification".into(),
                original_port: "count_matrix".into(),
                canonical_port: "raw_count_matrix".into(),
            }],
            "the rewire must retain BOTH the original and the resolved port"
        );
    }

    /// When the anchor does NOT already produce the supplied type, the
    /// port-copy pushes the producer's port verbatim — so the name already
    /// resolves and no rename (and no alias) is recorded. Guards the
    /// long-standing copy path from picking up spurious alias rows.
    #[test]
    fn rewire_leaves_from_port_alone_when_the_port_copy_supplies_it() {
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                node("data_acquisition", vec![], vec![out("staged", "data:2531")]),
                node(
                    "quantification",
                    vec![inp("bam", BAM)],
                    vec![out("count_matrix", COUNTS)],
                ),
                node(
                    "differential_expression",
                    vec![inp("counts", COUNTS)],
                    vec![out("de", DE)],
                ),
            ],
            edges: vec![
                edge_ports("data_acquisition", "staged", "quantification", "bam"),
                edge_ports(
                    "quantification",
                    "count_matrix",
                    "differential_expression",
                    "counts",
                ),
            ],
            ..Default::default()
        };
        prune_supplied_upstream(&mut dag, &[DataProductContract::gene_count_matrix()]);

        let rewired = dag
            .edges
            .iter()
            .find(|e| e.from_node == "data_acquisition" && e.to_node == "differential_expression")
            .expect("differential_expression must be rewired onto data_acquisition");
        assert_eq!(
            rewired.from_port, "count_matrix",
            "the copied port already carries this name — no rename is warranted"
        );
        assert!(
            port_aliases(rewired).is_empty(),
            "no alias may be recorded when the port name is unchanged: {:?}",
            rewired.proof.warnings
        );
    }

    /// A synthesized ordering-edge sentinel (`report`, wired by
    /// `report_data_synthesis`) is not a declared producer port, so the rewire
    /// must leave it alone rather than coerce it onto the anchor's data port.
    /// The pass runs twice (seed lift + primary finalize) and by the second
    /// run these structural edges exist.
    #[test]
    fn rewire_does_not_coerce_synthetic_ordering_ports_onto_a_data_port() {
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                data_acquisition_node(),
                node(
                    "quantification",
                    vec![inp("bam", BAM)],
                    vec![out("count_matrix", COUNTS)],
                ),
                node(
                    "assemble_report_data",
                    vec![inp("analysis_result", "data:2048")],
                    vec![out("report_data", "data:2048")],
                ),
            ],
            edges: vec![
                edge_ports("data_acquisition", "raw_reads", "quantification", "bam"),
                edge_ports(
                    "quantification",
                    "report",
                    "assemble_report_data",
                    "analysis_result",
                ),
            ],
            ..Default::default()
        };
        prune_supplied_upstream(&mut dag, &[DataProductContract::gene_count_matrix()]);

        let structural = dag
            .edges
            .iter()
            .find(|e| e.to_node == "assemble_report_data")
            .expect("the ordering edge survives the rewire");
        assert_eq!(
            structural.from_node, "data_acquisition",
            "the ordering edge is still rewired onto the anchor"
        );
        assert_eq!(
            structural.from_port, "report",
            "a synthetic ordering sentinel must not be renamed to a data port"
        );
        assert!(
            port_aliases(structural).is_empty(),
            "no alias for an untouched sentinel port: {:?}",
            structural.proof.warnings
        );
    }

    /// Running the pass twice (the real planner does: once at archetype-seed
    /// lift, once in `finalize_primary_dag`) must not stack duplicate alias
    /// rows or re-rename an already-canonical port.
    #[test]
    fn repeated_passes_are_idempotent_on_the_alias_record() {
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                data_acquisition_node(),
                node(
                    "quantification",
                    vec![inp("bam", BAM)],
                    vec![out("count_matrix", COUNTS)],
                ),
                node(
                    "differential_expression",
                    vec![inp("counts", COUNTS)],
                    vec![out("de", DE)],
                ),
            ],
            edges: vec![
                edge_ports("data_acquisition", "raw_reads", "quantification", "bam"),
                edge_ports(
                    "quantification",
                    "count_matrix",
                    "differential_expression",
                    "counts",
                ),
            ],
            ..Default::default()
        };
        prune_supplied_upstream(&mut dag, &[DataProductContract::gene_count_matrix()]);
        prune_supplied_upstream(&mut dag, &[DataProductContract::gene_count_matrix()]);

        let rewired = dag
            .edges
            .iter()
            .find(|e| e.to_node == "differential_expression")
            .expect("rewired edge survives both passes");
        assert_eq!(
            rewired.from_port, "raw_count_matrix",
            "second pass must not re-rename an already-canonical port"
        );
        assert_eq!(
            port_aliases(rewired).len(),
            1,
            "alias rows must not stack across passes: {:?}",
            rewired.proof.warnings
        );
    }

    /// `PortAlias` is carried as a `key=value` proof-warning row; the encoding
    /// must round-trip and must ignore unrelated warnings.
    #[test]
    fn port_alias_round_trips_through_the_proof_warning_row() {
        let alias = PortAlias {
            pruned_producer: "qc_preprocessing".into(),
            original_port: "filtered_count_matrix".into(),
            canonical_port: "raw_count_matrix".into(),
        };
        let row = alias.encode();
        assert!(
            row.starts_with(PORT_ALIAS_TAG),
            "encoded row must be tagged: {row}"
        );
        assert_eq!(
            PortAlias::decode(&row),
            Some(alias),
            "decode must invert encode"
        );
        assert_eq!(
            PortAlias::decode("genome build differs"),
            None,
            "an unrelated proof warning must not decode as an alias"
        );
        assert_eq!(
            PortAlias::decode(&format!("{PORT_ALIAS_TAG}: original_port=a")),
            None,
            "a row missing fields must not decode"
        );
    }
}
