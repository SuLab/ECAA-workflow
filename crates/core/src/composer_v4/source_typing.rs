//! Surface SME-registered intake inputs as typed *source output ports*
//! on the composed DAG's ingest-root node (`data_acquisition` /
//! `data_import`).
//!
//! ## Why this exists
//!
//! A later pass (`prune_unsourced`) prunes an optional atom whose
//! REQUIRED input ports cannot be SOURCED — i.e. no in-DAG producer
//! emits a compatible output AND no registered intake input supplies
//! one. For that pruner to treat a registered external input (e.g. a
//! gene-set / GMT file the SME registered at intake) as a valid source,
//! the registered input must be visible to the type system as a TYPED
//! OUTPUT PORT on the synthetic ingest anchor. The archetype's
//! `data_acquisition` atom only authors the canonical sequencing-era
//! outputs (`cohort_manifest`, `raw_reads`); an SME-supplied gene-set
//! collection has nowhere to attach. This pass closes that gap by
//! appending one output port per confidently-typed registered input.
//!
//! ## Discipline
//!
//! * **Conservative.** `infer_source_port` returns `Some` only on a
//!   confident match. An ambiguous registration produces no port (the
//!   downstream pruner then treats the dependent atom as unsourced,
//!   which is the safe default — it surfaces a gap rather than silently
//!   sourcing from an unknown artifact).
//! * **Additive only.** This pass NEVER prunes, NEVER removes, NEVER
//!   rewires. It only appends output ports. De-dups against ports
//!   already present on the anchor (by semantic-type stable id) so a
//!   registered FASTQ does not double the atom's existing `raw_reads`
//!   output.
//! * **Deterministic.** Registered inputs are processed in a stable
//!   order (sorted by input id) and identical inferred ports are
//!   de-duped, so the appended-port set is byte-stable across replays.

use std::collections::BTreeSet;

use crate::workflow_contracts::data_product::DataProductContract;
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::semantic_type::SemanticType;
use crate::workflow_contracts::task_node::WorkflowDag;

/// Canonical semantic IRI for a gene-set / pathway gene-set collection
/// (the GMT-style input the SME registers for pathway / gene-set
/// enrichment: KEGG / Reactome / GO / MSigDB hallmark collections).
///
/// EDAM has no dedicated "gene set collection" / GMT data class (the
/// closest results-side term is `data:3953` "Pathway overrepresentation
/// data", which is enrichment OUTPUT, not the input collection). The
/// most defensible *input*-side EDAM data term is
/// `data:2600` "Pathway or network" — "Primary data about a specific
/// biological pathway or network (the nodes and connections within the
/// pathway or network)". A pathway gene-set collection is exactly that
/// primary pathway membership data, and it matches `pathway_enrichment`'s
/// declared candidate databases (KEGG / Reactome / GO / MSigDB).
///
/// This is the SHARED constant later tasks reuse: the pathway atom's
/// gene-set input port and the prune-compatibility check must reference
/// THIS const so producer/consumer types unify on a single IRI.
pub const GENE_SET_SEMANTIC_IRI: &str = "data:2600";

/// Human label paired with [`GENE_SET_SEMANTIC_IRI`] wherever a
/// `SemanticType::OntologyTerm` is minted for it.
pub const GENE_SET_SEMANTIC_LABEL: &str = "Pathway or network";

/// EDAM data IRI for a feature/gene count matrix. Matches the count-
/// matrix type used elsewhere in the codebase (`data:3917`).
const COUNTS_SEMANTIC_IRI: &str = "data:3917";
const COUNTS_SEMANTIC_LABEL: &str = "Count matrix";

/// EDAM data IRI for a PRE-COMPUTED differential-expression results table
/// (the SME already holds DE results — log-fold-change + p-value + FDR rows,
/// e.g. a limma / DESeq2 / edgeR / FragPipe `*_DE_results.tsv`). Matches the
/// `differential_expression` atom's `de_results` OUTPUT-PORT type so a
/// registered DE table unifies with the DE node's producing type for
/// input-stage pruning and downstream consumer sourcing (BiomniBench da-15-8).
/// This is the node output type, NOT the archetype goal type `data:0951`.
const DE_RESULTS_SEMANTIC_IRI: &str = "data:3134";
const DE_RESULTS_SEMANTIC_LABEL: &str = "Gene expression data";

/// EDAM data IRI for raw sequence reads (FASTQ). Matches the
/// `data_acquisition` atom's existing `raw_reads` output (`data:2044`),
/// so a registered FASTQ de-dups against the anchor's authored output
/// rather than adding a redundant port.
const READS_SEMANTIC_IRI: &str = "data:2044";
const READS_SEMANTIC_LABEL: &str = "Sequence reads";

/// Ingest-root node ids that act as the synthetic upstream source
/// anchor. Mirrors `survey_method_landscape_synthesis`'s
/// `DATA_CHARACTERIZATION_PRODUCERS` ingest-root pair.
const INGEST_ROOT_IDS: &[&str] = &["data_acquisition", "data_import"];

/// Lower-case helper for substring role/name matching.
fn lc(s: &str) -> String {
    s.trim().to_ascii_lowercase()
}

/// Infer a typed *source* output port for a registered intake input,
/// or `None` when the registration cannot be confidently typed.
///
/// CONSERVATIVE — returns `Some` only on a confident signal:
///
/// * **Gene set** (canonical, the keystone for this feature):
///   `.gmt` extension on the file name, OR a role/name that contains
///   "gene_set" / "gene set" / "gene set collection". → a port typed
///   [`GENE_SET_SEMANTIC_IRI`].
/// * **Pre-computed DE results**: a `.tsv` / `.csv` file whose role/name
///   implies a differential-expression results table ("de_results" /
///   "differential expression" / "limma" / "deseq2" / "edger" /
///   "differential abundance" / a log-fold-change or adjusted-p column). →
///   a port typed [`DE_RESULTS_SEMANTIC_IRI`]. Checked BEFORE counts so a
///   DE table (whose columns may mention "count") is not mistyped as a
///   counts matrix.
/// * **Counts matrix**: a `.tsv` / `.csv` file whose role/name implies
///   counts ("count" / "counts" / "count_matrix" / "count matrix"). →
///   a port typed [`COUNTS_SEMANTIC_IRI`].
/// * **Raw reads**: a `.fastq` / `.fq` / `.fastq.gz` / `.fq.gz` file.
///   → a port typed [`READS_SEMANTIC_IRI`].
///
/// `mime` is accepted for symmetry and future tightening (e.g. an
/// `application/x-fastq` media type) but the file-name / role signals
/// are the load-bearing ones today; a clearly-commented widening point
/// is left below.
pub fn infer_source_port(
    file_name: &str,
    role: Option<&str>,
    mime: Option<&str>,
) -> Option<PortContract> {
    let name_lc = lc(file_name);
    let role_lc = role.map(lc).unwrap_or_default();
    let mime_lc = mime.map(lc).unwrap_or_default();

    let has_ext = |ext: &str| name_lc.ends_with(ext);
    let role_or_name_contains = |needle: &str| {
        role_lc.contains(needle) || name_lc.contains(needle)
    };

    // --- Gene set (canonical) ----------------------------------------
    // `.gmt` is the unambiguous GSEA/MSigDB gene-set collection format;
    // role/name "gene set" covers SME registrations that point at a
    // gene-set without the canonical extension.
    if has_ext(".gmt")
        || role_or_name_contains("gene_set")
        || role_or_name_contains("gene set")
        || role_or_name_contains("gene-set")
        || role_or_name_contains("geneset")
    {
        return Some(source_port(
            "registered_gene_set",
            GENE_SET_SEMANTIC_IRI,
            GENE_SET_SEMANTIC_LABEL,
        ));
    }

    // --- Pre-computed DE results -------------------------------------
    // Tabular extension PLUS a differential-expression signal in
    // role/name. Checked BEFORE counts because a DE results table
    // (log-fold-change + p-value + FDR per gene/protein) can carry
    // "count"-adjacent columns yet is a distinct, more-processed product;
    // matching DE first prevents mistyping the supplied DE table as a raw
    // counts matrix. INPUT-TYPE RECOGNITION ONLY — typing a held DE table
    // prescribes no DE method or threshold. The DE node's `de_results`
    // OUTPUT port is `data:3134`, so this typing lets the supplied table
    // unify with that producing type for pruning + downstream sourcing.
    if (has_ext(".tsv") || has_ext(".csv") || mime_lc == "text/tab-separated-values")
        && (role_or_name_contains("de_results")
            || role_or_name_contains("de-results")
            || role_or_name_contains("de results")
            || role_or_name_contains("differential expression")
            || role_or_name_contains("differential_expression")
            || role_or_name_contains("differential abundance")
            || role_or_name_contains("differential_abundance")
            || role_or_name_contains("limma")
            || role_or_name_contains("deseq2")
            || role_or_name_contains("edger")
            || role_or_name_contains("log_fc")
            || role_or_name_contains("logfc")
            || role_or_name_contains("log2fc")
            || role_or_name_contains("adj_p_val")
            || role_or_name_contains("adj.p.val")
            || role_or_name_contains("padj"))
    {
        return Some(source_port(
            "registered_de_results",
            DE_RESULTS_SEMANTIC_IRI,
            DE_RESULTS_SEMANTIC_LABEL,
        ));
    }

    // --- Counts matrix -----------------------------------------------
    // Tabular extension PLUS a counts signal in role/name. The tabular
    // gate prevents a non-tabular file from being mistyped as a matrix.
    if (has_ext(".tsv") || has_ext(".csv") || mime_lc == "text/tab-separated-values")
        && (role_or_name_contains("count_matrix")
            || role_or_name_contains("count matrix")
            || role_or_name_contains("counts")
            || role_or_name_contains("count"))
    {
        return Some(source_port(
            "registered_counts",
            COUNTS_SEMANTIC_IRI,
            COUNTS_SEMANTIC_LABEL,
        ));
    }

    // --- Raw reads ---------------------------------------------------
    if has_ext(".fastq")
        || has_ext(".fq")
        || has_ext(".fastq.gz")
        || has_ext(".fq.gz")
    {
        return Some(source_port(
            "registered_reads",
            READS_SEMANTIC_IRI,
            READS_SEMANTIC_LABEL,
        ));
    }

    // TODO(broader-feature): tighten on MIME alone (e.g.
    // `application/x-fastq`, `application/x-bam`) and add further
    // confident shapes (BAM/CRAM alignments → data:0863, VCF variants →
    // data:3498) once the prune-compatibility check needs them. Left out
    // here deliberately rather than guessing semantic types — an unknown
    // registration MUST stay untyped so the pruner surfaces a gap.
    None
}

/// Build a minimal source output `PortContract` for an inferred input.
fn source_port(name: &str, iri: &str, label: &str) -> PortContract {
    PortContract::with_semantic_type(
        name.to_string(),
        SemanticType::edam(iri.to_string(), label.to_string()),
    )
}

/// Reduce a `DataProductContract` to the `(file_name, role, mime)`
/// triple `infer_source_port` consumes.
///
/// * `file_name` — basename of the physical URI when present, else the
///   contract id (the dispatcher stamps ids like
///   `intake_dataset_descriptor_0`).
/// * `role` — the semantic type's stable id (e.g. an
///   `ecaax:gene_set` local extension) so a typed registration unifies
///   even without a recognizable extension.
/// * `mime` — `physical.media_type`.
fn triple_for(dp: &DataProductContract) -> (String, String, Option<String>) {
    let file_name = dp
        .physical
        .uri
        .as_deref()
        .map(|u| {
            // basename: split on both separators so `s3://b/x.gmt` and
            // `file:///p/x.gmt` reduce to `x.gmt`.
            u.rsplit(['/', '\\']).next().unwrap_or(u).to_string()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| dp.id.clone());
    let role = dp.semantic_type.stable_id();
    let mime = dp.physical.media_type.clone();
    (file_name, role, mime)
}

/// Append a typed source output port to the DAG's ingest-root anchor
/// for every registered intake input that `infer_source_port` types
/// confidently.
///
/// Additive only — never prunes / removes / rewires. De-dups inferred
/// ports against each other AND against the anchor's existing output
/// ports (by semantic-type stable id), so a registered FASTQ does not
/// duplicate the anchor's authored `raw_reads` output. Deterministic:
/// registered inputs are processed sorted by id; identical ports are
/// de-duped.
///
/// No-op when there is no ingest-root node or no registered inputs.
pub fn surface_registered_source_ports(
    dag: &mut WorkflowDag,
    registered_inputs: &[DataProductContract],
) {
    if registered_inputs.is_empty() {
        return;
    }
    // Find the ingest-root anchor. `data_acquisition` first, then
    // `data_import`; the two never coexist in a composed DAG.
    let Some(anchor_idx) = dag
        .nodes
        .iter()
        .position(|n| INGEST_ROOT_IDS.contains(&n.id.as_str()))
    else {
        return;
    };

    // Stable iteration order over registered inputs (sorted by id).
    let mut sorted: Vec<&DataProductContract> = registered_inputs.iter().collect();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    // Semantic-type stable ids already present on the anchor's outputs,
    // plus the ones we add this pass — used for de-dup.
    let mut present: BTreeSet<String> = dag.nodes[anchor_idx]
        .outputs
        .iter()
        .map(|p| p.semantic_type.stable_id())
        .collect();

    let mut to_add: Vec<PortContract> = Vec::new();
    for dp in sorted {
        let (file_name, role, mime) = triple_for(dp);
        if let Some(port) = infer_source_port(&file_name, Some(&role), mime.as_deref()) {
            let key = port.semantic_type.stable_id();
            if present.insert(key) {
                to_add.push(port);
            }
        }
    }

    dag.nodes[anchor_idx].outputs.extend(to_add);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::data_product::{DataProductContract, PhysicalRepresentation};
    use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

    fn dag_with_anchor(anchor_id: &str) -> WorkflowDag {
        let mut anchor = TaskNode::skeleton(anchor_id, "ingest root");
        // Mirror the real `data_acquisition` atom's authored outputs so
        // the de-dup behaviour is exercised against a realistic anchor.
        anchor
            .outputs
            .push(PortContract::with_semantic_type(
                "cohort_manifest",
                SemanticType::edam("data:2531", "Experiment annotation"),
            ));
        anchor
            .outputs
            .push(PortContract::with_semantic_type(
                "raw_reads",
                SemanticType::edam("data:2044", "Sequence reads"),
            ));
        WorkflowDag {
            id: "test".into(),
            nodes: vec![anchor],
            ..Default::default()
        }
    }

    fn registered_gmt() -> DataProductContract {
        let mut dp = DataProductContract::skeleton(
            "intake_gene_set_0",
            SemanticType::opaque("unprofiled gene-set registration"),
        );
        dp.physical = PhysicalRepresentation {
            uri: Some("file:///inputs/hallmark.sets.gmt".into()),
            media_type: Some("text/plain".into()),
            ..Default::default()
        };
        dp
    }

    // ---- pure inference rules --------------------------------------

    #[test]
    fn infer_gene_set_from_gmt_extension() {
        let port = infer_source_port("hallmark.sets.gmt", None, None)
            .expect("a .gmt file should infer as a gene-set source");
        assert_eq!(port.semantic_type.stable_id(), GENE_SET_SEMANTIC_IRI);
    }

    #[test]
    fn infer_gene_set_from_role() {
        let port = infer_source_port("collection.txt", Some("gene_set"), None)
            .expect("role gene_set should infer as a gene-set source");
        assert_eq!(port.semantic_type.stable_id(), GENE_SET_SEMANTIC_IRI);
    }

    #[test]
    fn infer_counts_from_tsv_and_role() {
        let port = infer_source_port("study.counts.tsv", Some("count_matrix"), None)
            .expect("a counts tsv should infer as a counts source");
        assert_eq!(port.semantic_type.stable_id(), COUNTS_SEMANTIC_IRI);
    }

    #[test]
    fn infer_de_results_from_filename_and_role() {
        // D4 (BiomniBench da-15-8): a pre-computed DE results table must type
        // to the DE node OUTPUT-PORT IRI (`data:3134`) so it unifies with the
        // `differential_expression` producing type for pruning + downstream
        // sourcing — NOT the goal type `data:0951`.
        for (name, role) in [
            ("study_DE_results.tsv", None),
            ("limma_output.tsv", None),
            ("proteins.csv", Some("differential expression")),
            ("table.tsv", Some("de_results")),
            ("abundance.tsv", Some("differential abundance")),
        ] {
            let port = infer_source_port(name, role, None).unwrap_or_else(|| {
                panic!("a DE-results table should infer as a DE source: {name:?}/{role:?}")
            });
            assert_eq!(port.semantic_type.stable_id(), DE_RESULTS_SEMANTIC_IRI);
        }
    }

    #[test]
    fn de_results_signal_wins_over_counts() {
        // A DE table whose name mentions both "de_results" and "count" must
        // type as DE results, NOT as a counts matrix (DE checked first).
        let port = infer_source_port("gene_count_DE_results.tsv", None, None)
            .expect("DE-results signal should win");
        assert_eq!(port.semantic_type.stable_id(), DE_RESULTS_SEMANTIC_IRI);
    }

    #[test]
    fn infer_reads_from_fastq() {
        let port = infer_source_port("sample_R1.fastq.gz", None, None)
            .expect("a fastq should infer as a reads source");
        assert_eq!(port.semantic_type.stable_id(), READS_SEMANTIC_IRI);
    }

    #[test]
    fn infer_returns_none_for_unknown() {
        // An unlabeled .txt with no role signal must stay untyped.
        assert!(infer_source_port("notes.txt", None, None).is_none());
        // A plain tabular file with no counts signal must NOT be guessed
        // as a counts matrix.
        assert!(infer_source_port("table.tsv", Some("metadata"), None).is_none());
    }

    // ---- DAG post-pass ---------------------------------------------

    #[test]
    fn surfaces_gene_set_output_on_data_acquisition() {
        let mut dag = dag_with_anchor("data_acquisition");
        let before = dag.nodes[0].outputs.len();
        surface_registered_source_ports(&mut dag, &[registered_gmt()]);
        let da = dag
            .nodes
            .iter()
            .find(|n| n.id == "data_acquisition")
            .unwrap();
        assert_eq!(
            da.outputs.len(),
            before + 1,
            "exactly one gene-set output port should be appended"
        );
        assert!(
            da.outputs
                .iter()
                .any(|p| p.semantic_type.stable_id() == GENE_SET_SEMANTIC_IRI),
            "data_acquisition must expose a gene-set source output port; outputs={:?}",
            da.outputs.iter().map(|p| p.name.clone()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn no_gene_set_output_without_gene_set_input() {
        let mut dag = dag_with_anchor("data_acquisition");
        let before = dag.nodes[0].outputs.len();
        // Register a plain FASTQ (reads) — must NOT add a gene-set port,
        // and must de-dup against the anchor's existing `raw_reads`
        // (data:2044) output so nothing is appended.
        let mut fastq = DataProductContract::skeleton(
            "intake_fastq_0",
            SemanticType::opaque("unprofiled fastq"),
        );
        fastq.physical = PhysicalRepresentation {
            uri: Some("file:///inputs/sample_R1.fastq.gz".into()),
            ..Default::default()
        };
        surface_registered_source_ports(&mut dag, &[fastq]);
        let da = dag
            .nodes
            .iter()
            .find(|n| n.id == "data_acquisition")
            .unwrap();
        assert!(
            !da.outputs
                .iter()
                .any(|p| p.semantic_type.stable_id() == GENE_SET_SEMANTIC_IRI),
            "no gene-set input was registered; no gene-set output port should appear"
        );
        assert_eq!(
            da.outputs.len(),
            before,
            "registered reads de-dup against the anchor's existing raw_reads output"
        );
    }

    #[test]
    fn dedup_appends_gene_set_once_for_two_gmt_inputs() {
        let mut dag = dag_with_anchor("data_acquisition");
        let before = dag.nodes[0].outputs.len();
        let mut a = registered_gmt();
        a.id = "intake_gene_set_a".into();
        let mut b = registered_gmt();
        b.id = "intake_gene_set_b".into();
        surface_registered_source_ports(&mut dag, &[a, b]);
        let da = dag.nodes.iter().find(|n| n.id == "data_acquisition").unwrap();
        assert_eq!(
            da.outputs.len(),
            before + 1,
            "two gene-set registrations must de-dup to a single appended port"
        );
    }

    #[test]
    fn noop_without_ingest_anchor() {
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![TaskNode::skeleton("differential_expression", "no anchor")],
            ..Default::default()
        };
        surface_registered_source_ports(&mut dag, &[registered_gmt()]);
        let de = &dag.nodes[0];
        assert!(
            de.outputs.is_empty(),
            "no ingest anchor → nothing surfaced"
        );
    }
}
