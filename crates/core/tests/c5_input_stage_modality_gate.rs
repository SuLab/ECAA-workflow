//! C5 (M7): input-stage detection must be MODALITY-GATED. A supplied-product
//! phrase only seeds when the requested modality actually has a producer for
//! that product. A counts phrase on a non-RNA modality (ChIP/ATAC) must NOT
//! seed `data:3917`; an unknown modality must seed nothing (fail-safe to raw).
use ecaa_workflow_core::intake_facts::IntakeFacts;
use ecaa_workflow_core::workflow_contracts::semantic_type::SemanticType;

fn iri_of(p: &ecaa_workflow_core::workflow_contracts::data_product::DataProductContract) -> String {
    match &p.semantic_type {
        SemanticType::OntologyTerm { iri, .. } => iri.clone(),
        other => panic!("expected ontology term, got {other:?}"),
    }
}

#[test]
fn supplied_counts_seeds_for_bulk_rnaseq() {
    let got = IntakeFacts::detect_input_data_stage(
        "we already have a counts matrix prepared, no raw FASTQs",
        Some("bulk_rnaseq"),
    );
    let p = got.expect("RNA-counts modality must honor a supplied counts matrix");
    assert_eq!(iri_of(&p), "data:3917");
}

#[test]
fn supplied_counts_seeds_for_single_cell_rnaseq() {
    let got = IntakeFacts::detect_input_data_stage(
        "start from the counts matrix already prepared",
        Some("single_cell_rnaseq"),
    );
    assert!(got.is_some(), "scRNA-seq must honor a supplied counts matrix");
}

#[test]
fn supplied_counts_ignored_for_chipseq() {
    let got = IntakeFacts::detect_input_data_stage(
        "peak calling, counts matrix already prepared samples",
        Some("chip_seq"),
    );
    assert!(
        got.is_none(),
        "ChIP-seq has no counts consumer; a counts phrase must not seed data:3917"
    );
}

#[test]
fn supplied_counts_ignored_for_atacseq() {
    let got = IntakeFacts::detect_input_data_stage(
        "counts matrix already prepared",
        Some("atac_seq"),
    );
    assert!(got.is_none(), "ATAC-seq must not seed counts");
}

#[test]
fn supplied_counts_ignored_when_modality_unknown() {
    let got = IntakeFacts::detect_input_data_stage("counts matrix already prepared", None);
    assert!(
        got.is_none(),
        "without a known RNA-counts modality, do not seed (fail-safe to raw)"
    );
}

#[test]
fn bare_quantify_verb_does_not_seed() {
    for prose in ["we already quantified the samples", "samples already counted"] {
        assert!(
            IntakeFacts::detect_input_data_stage(prose, Some("bulk_rnaseq")).is_none(),
            "bare quantify/count verb must not seed: {prose:?}"
        );
    }
}

#[test]
fn supplied_peaks_seed_for_chipseq() {
    let got = IntakeFacts::detect_input_data_stage(
        "called peaks already prepared, run differential binding",
        Some("chip_seq"),
    );
    let p = got.expect("ChIP-seq must honor supplied called peaks");
    assert_eq!(iri_of(&p), "data:1255");
}

#[test]
fn supplied_peaks_ignored_for_bulk_rnaseq() {
    let got = IntakeFacts::detect_input_data_stage(
        "called peaks already prepared",
        Some("bulk_rnaseq"),
    );
    assert!(
        got.is_none(),
        "bulk RNA-seq has no peak consumer; a peak phrase must not seed data:1255"
    );
}

#[test]
fn supplied_vcf_seeds_for_variant_calling() {
    let got = IntakeFacts::detect_input_data_stage(
        "we already have a VCF of called variants, annotate them",
        Some("variant_calling"),
    );
    let p = got.expect("variant_calling must honor a supplied VCF");
    assert_eq!(iri_of(&p), "data:3498");
}

#[test]
fn supplied_bam_seeds_when_modality_known() {
    let got = IntakeFacts::detect_input_data_stage(
        "BAM files already prepared, skip alignment",
        Some("bulk_rnaseq"),
    );
    let p = got.expect("a supplied BAM must seed an alignment product");
    assert_eq!(iri_of(&p), "data:0863");
}

#[test]
fn fastq_pipeline_prose_does_not_seed() {
    // A full FASTQ-pipeline description that names "gene-level counts" as a
    // produced STEP must not be mistaken for a supplied input (no possession
    // marker co-occurs with the counts noun).
    let prose = "bulk RNA-seq FASTQs; FastQC and adapter trimming, splice-aware \
                 alignment, gene-level counts, DESeq2 normalization, and a DE test";
    assert!(
        IntakeFacts::detect_input_data_stage(prose, Some("bulk_rnaseq")).is_none(),
        "FASTQ-pipeline prose must not seed an input stage"
    );
}
