//! Audit F4 — classifier coverage backfill for previously uncovered modalities.
//!
//! Each test exercises a modality that had zero classifier-test coverage as of
//! The audit. Every prompt is chosen to include at least two
//! distinct keywords from the corresponding `config/modalities/<id>.yaml`
//! keyword list, making the test robust to minor keyword-list edits.
//!
//! Modalities covered here:
//! cut_tag, chip_exo, methylation, immunopeptidomics,
//! hi_chip, starr_seq, crispr_screen_scrnaseq, ribo_seq,
//! Gwas (added disambiguates GWAS summary-statistics prose
//! from variant_calling so PGC3 / coloc / MAGMA scenarios stop misrouting),
//! ehr_clinical_prediction.
//!
//! The gate test `modality_test_coverage.rs` will fail if additional
//! modalities later acquire zero coverage — add new entries here in that case.

use ecaa_workflow_core::classify::Classifier;
use std::path::PathBuf;

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

fn load_classifier() -> Classifier {
    Classifier::load(&config_root().join("modality-keywords.yaml"))
        .expect("Classifier must load from config/modality-keywords.yaml")
}

// ---------------------------------------------------------------------------
// cut_tag
// Keywords used: "CUT&Tag" (cut&tag), "SEACR" (seacr)
// Distinguishing from chip_seq: CUT&Tag uses pA-Tn5 transposase, not ChIP
// sonication; SEACR is the CUT&Tag-specific peak caller.
// ---------------------------------------------------------------------------
#[test]
fn cut_tag_routes_to_cut_tag() {
    let clf = load_classifier();
    let result = clf.classify(
        "CUT&Tag profiling of H3K27ac and H3K4me3 histone marks using \
         pA-Tn5 transposase. Peak calling with SEACR at stringent threshold \
         to identify active enhancers.",
    );
    assert_eq!(
        result.modality, "cut_tag",
        "CUT&Tag + SEACR prose must route to cut_tag, got {:?}",
        result.modality
    );
}

// ---------------------------------------------------------------------------
// chip_exo
// Keywords used: "ChIP-exo" (chip-exo), "ChExMix" (chexmix)
// Distinguishing from chip_seq: ChIP-exo adds lambda exonuclease trimming
// for near-nucleotide resolution; ChExMix is a chip_exo-specific tool.
// ---------------------------------------------------------------------------
#[test]
fn chip_exo_routes_to_chip_exo() {
    let clf = load_classifier();
    let result = clf.classify(
        "High-resolution transcription factor binding site mapping using \
         ChIP-exo with lambda exonuclease trimming to achieve near-single-\
         nucleotide resolution. Peak detection with ChExMix to identify \
         strand-specific footprints.",
    );
    assert_eq!(
        result.modality, "chip_exo",
        "ChIP-exo + ChExMix prose must route to chip_exo, got {:?}",
        result.modality
    );
}

// ---------------------------------------------------------------------------
// methylation
// Keywords used: "bisulfite sequencing" (bisulfite sequencing), "bismark"
// (bismark), "differentially methylated" (differentially methylated)
// ---------------------------------------------------------------------------
#[test]
fn methylation_routes_to_methylation() {
    let clf = load_classifier();
    let result = clf.classify(
        "Whole-genome bisulfite sequencing (WGBS) to profile DNA methylation \
         across tumor and normal samples. Alignment with Bismark; identification \
         of differentially methylated regions (DMRs) between groups.",
    );
    assert_eq!(
        result.modality, "methylation",
        "Bisulfite sequencing + Bismark + differentially methylated prose \
         must route to methylation, got {:?}",
        result.modality
    );
}

// ---------------------------------------------------------------------------
// immunopeptidomics
// Keywords used: "immunopeptidomics" (immunopeptidomics),
// "NetMHCpan" (netmhcpan), "MHC class I" (mhc class i)
// Distinguishing from proteomics: HLA/MHC immunopeptide pull-down context.
// ---------------------------------------------------------------------------
#[test]
fn immunopeptidomics_routes_to_immunopeptidomics() {
    let clf = load_classifier();
    let result = clf.classify(
        "Immunopeptidomics study profiling MHC class I bound peptides \
         isolated by W6/32 antibody immunoprecipitation. Neoantigen \
         discovery via NetMHCpan binding affinity predictions.",
    );
    assert_eq!(
        result.modality, "immunopeptidomics",
        "Immunopeptidomics + MHC class I + NetMHCpan prose must route to \
         immunopeptidomics, got {:?}",
        result.modality
    );
}

// ---------------------------------------------------------------------------
// hi_chip
// Keywords used: "HiChIP" (hichip), "FitHiChIP" (fithichip)
// Distinguishing from chip_seq: HiChIP adds proximity ligation to capture
// chromatin loops; FitHiChIP is the hi_chip-specific loop-calling tool.
// ---------------------------------------------------------------------------
#[test]
fn hi_chip_routes_to_hi_chip() {
    let clf = load_classifier();
    let result = clf.classify(
        "HiChIP experiment targeting H3K27ac to map promoter-enhancer loops \
         in primary B cells. Loop calling with FitHiChIP to identify \
         significant chromatin interactions anchored at active regulatory elements.",
    );
    assert_eq!(
        result.modality, "hi_chip",
        "HiChIP + FitHiChIP prose must route to hi_chip, got {:?}",
        result.modality
    );
}

// ---------------------------------------------------------------------------
// starr_seq
// Keywords used: "STARR-seq" (starr-seq), "massively parallel reporter assay"
// (massively parallel reporter assay)
// Distinguishing from atac_seq: STARR-seq quantifies enhancer activity by
// self-transcription in a reporter assay, not accessibility per se.
// ---------------------------------------------------------------------------
#[test]
fn starr_seq_routes_to_starr_seq() {
    let clf = load_classifier();
    let result = clf.classify(
        "Genome-wide enhancer activity mapping using STARR-seq (massively \
         parallel reporter assay). Self-transcribing enhancer activity \
         quantification across 200,000 candidate regulatory elements to \
         identify cell-type-specific active enhancers.",
    );
    assert_eq!(
        result.modality, "starr_seq",
        "STARR-seq + massively parallel reporter assay prose must route to \
         starr_seq, got {:?}",
        result.modality
    );
}

// ---------------------------------------------------------------------------
// crispr_screen_scrnaseq
// Keywords used: "Perturb-seq" (perturb-seq), "pooled CRISPR screen"
// (pooled crispr screen), "MAGeCK" (mageck)
// Distinguishing from single_cell_rnaseq: the pooled perturbation context
// with sgRNA barcoding separates this from vanilla scRNA-seq.
// ---------------------------------------------------------------------------
#[test]
fn crispr_screen_scrnaseq_routes_to_crispr_screen_scrnaseq() {
    // Perturb-seq prose routes to `single_cell_rnaseq` rather than
    // `crispr_screen_scrnaseq` so the single_cell_de archetype is
    // picked and the protocol=perturb_seq slot expansion injects
    // sgrna_assignment. The crispr_screen_scrnaseq archetype is
    // consolidated into single_cell_de.
    let clf = load_classifier();
    let result = clf.classify(
        "Pooled CRISPR screen combined with single-cell RNA-seq (Perturb-seq) \
         to map transcriptional effects of 5,000 sgRNA perturbations. \
         MAGeCK for guide-level differential expression and feature barcoding \
         to link each cell to its sgRNA.",
    );
    assert_eq!(
        result.modality, "single_cell_rnaseq",
        "Perturb-seq + pooled CRISPR screen + MAGeCK prose routes to \
         single_cell_rnaseq (which then picks single_cell_de archetype \
         with protocol=perturb_seq slot), got {:?}",
        result.modality
    );
}

// ---------------------------------------------------------------------------
// ribo_seq
// Keywords used: "ribosome profiling" (ribosome profiling),
// "translation efficiency" (translation efficiency), "RiboWaltz" (ribowaltz)
// ---------------------------------------------------------------------------
#[test]
fn ribo_seq_routes_to_ribo_seq() {
    let clf = load_classifier();
    let result = clf.classify(
        "Ribosome profiling (Ribo-seq) to measure translation efficiency \
         genome-wide. Ribosome-protected fragments (RPFs) processed with \
         RiboWaltz for P-site offset correction and codon-level occupancy \
         analysis.",
    );
    assert_eq!(
        result.modality, "ribo_seq",
        "Ribosome profiling + RiboWaltz + translation efficiency prose \
         must route to ribo_seq, got {:?}",
        result.modality
    );
}

// ---------------------------------------------------------------------------
// gwas
// Keywords used: "GWAS" (gwas), "summary statistics" (summary statistics),
// "PGC3" (pgc3), "Trubetskoy" (trubetskoy), "coloc" (coloc),
// "MAGMA" (magma), "GTEx" (gtex)
// Distinguishing from variant_calling: GWAS workflows operate on
// pre-computed effect-size summary statistics, not raw reads / VCFs.
// The bench-side variant_calling vocabulary ("bwa", "gatk", "deepvariant",
// "exome", "germline", "somatic") is absent.
// ---------------------------------------------------------------------------
#[test]
fn gwas_routes_to_gwas() {
    let clf = load_classifier();
    let result = clf.classify(
        "Public human GWAS locus prioritization reanalysis of schizophrenia \
         using the PGC3 wave-3 public summary statistics (Trubetskoy 2022) \
         and GTEx v8 cis-eQTL summary statistics. Bayesian colocalization \
         posteriors (coloc.abf and coloc.susie) for schizophrenia-associated \
         loci, triangulated with MAGMA gene-level p-values and stratified \
         LD-score regression tissue-enrichment.",
    );
    assert_eq!(
        result.modality, "gwas",
        "PGC3 + Trubetskoy + coloc + MAGMA + GTEx summary-statistics prose \
         must route to gwas (not variant_calling), got {:?}",
        result.modality
    );
}

// ---------------------------------------------------------------------------
// ehr_clinical_prediction
// Keywords used: "MIMIC-IV" (mimic-iv), "eICU-CRD" (eicu-crd),
// "Sepsis-3" (sepsis-3), "PhysioNet" (physionet), "ICU" (icu)
// Distinguishing from any sequencing modality: credentialed-access EHR
// dataset names + clinical-prediction vocabulary (sepsis early warning,
// ICU, physionet) are mutually exclusive with bench/sequencing keywords.
// ---------------------------------------------------------------------------
#[test]
fn ehr_clinical_prediction_routes_to_ehr_clinical_prediction() {
    let clf = load_classifier();
    let result = clf.classify(
        "Sepsis-3 early-warning prediction trained on MIMIC-IV ICU data \
         (PhysioNet credentialed access) and externally validated on \
         eICU-CRD. Windowed hourly vital-sign and lab features from the \
         mimic-code sepsis3 derived table; XGBoost + LSTM ensemble with \
         calibration and fairness audit across hospitals.",
    );
    assert_eq!(
        result.modality, "ehr_clinical_prediction",
        "MIMIC-IV + eICU-CRD + Sepsis-3 + PhysioNet ICU prose must route \
         to ehr_clinical_prediction, got {:?}",
        result.modality
    );
}

// ---------------------------------------------------------------------------
// Backfill: 10 modalities that needed a real classifier-result
// assertion under the strict modality_test_coverage gate. The gate
// requires `assert_eq!(result.modality, "<id>")` (or equivalent
// regex-matched form), not just a string mention.
// ---------------------------------------------------------------------------

#[test]
fn atac_seq_routes_to_atac_seq() {
    let clf = load_classifier();
    let result = clf.classify(
        "ATAC-seq chromatin accessibility profiling using Tn5 transposase \
         tagmentation. Open chromatin peaks called with MACS2; consensus \
         peak set across replicates; motif enrichment with HOMER.",
    );
    assert_eq!(
        result.modality, "atac_seq",
        "ATAC-seq + Tn5 transposase + MACS2 prose must route to atac_seq, got {:?}",
        result.modality
    );
}

#[test]
fn chip_seq_routes_to_chip_seq() {
    let clf = load_classifier();
    let result = clf.classify(
        "ChIP-seq for H3K27ac with chromatin immunoprecipitation, formaldehyde \
         crosslinking, and sonication. Peak calling with MACS2 against input \
         control; IDR replicate analysis.",
    );
    assert_eq!(
        result.modality, "chip_seq",
        "ChIP-seq + chromatin immunoprecipitation + MACS2 + IDR prose must \
         route to chip_seq, got {:?}",
        result.modality
    );
}

#[test]
fn generic_omics_routes_to_generic_omics() {
    let clf = load_classifier();
    let result = clf.classify(
        "Generic high-throughput sequencing data analysis with no specific \
         omics-modality designation. Multi-omics or unspecified-modality \
         workflow placeholder.",
    );
    assert_eq!(
        result.modality, "generic_omics",
        "generic omics fallback prose must route to generic_omics, got {:?}",
        result.modality
    );
}

#[test]
fn long_read_rnaseq_routes_to_long_read_rnaseq() {
    let clf = load_classifier();
    let result = clf.classify(
        "Nanopore long-read RNA-seq (Oxford Nanopore PromethION direct cDNA \
         protocol) for full-length isoform discovery and quantification. \
         Alignment with minimap2 spliced mode; isoform assembly with \
         FLAIR; differential transcript usage.",
    );
    assert_eq!(
        result.modality, "long_read_rnaseq",
        "Nanopore long-read RNA-seq + minimap2 + FLAIR + isoform prose must \
         route to long_read_rnaseq, got {:?}",
        result.modality
    );
}

#[test]
fn metagenomics_routes_to_metagenomics() {
    let clf = load_classifier();
    let result = clf.classify(
        "Shotgun metagenomics of stool microbiome samples. Taxonomic \
         classification with Kraken2 against the standard plus-fungi \
         database; species abundance estimation with MetaPhlAn; functional \
         profiling with HUMAnN.",
    );
    assert_eq!(
        result.modality, "metagenomics",
        "shotgun metagenomics + Kraken2 + MetaPhlAn prose must route to \
         metagenomics, got {:?}",
        result.modality
    );
}

#[test]
fn proteomics_routes_to_proteomics() {
    let clf = load_classifier();
    let result = clf.classify(
        "Label-free quantitative proteomics by LC-MS/MS on a Thermo Orbitrap. \
         Peptide spectral matching with MaxQuant against the human reference \
         proteome; protein-level differential abundance analysis.",
    );
    assert_eq!(
        result.modality, "proteomics",
        "LC-MS/MS + MaxQuant + peptide spectral matching prose must route \
         to proteomics, got {:?}",
        result.modality
    );
}

#[test]
fn single_cell_rnaseq_routes_to_single_cell_rnaseq() {
    let clf = load_classifier();
    let result = clf.classify(
        "Single-cell RNA-seq of dissociated PBMC samples using the 10x \
         Genomics Chromium platform. Alignment + counting with Cell Ranger; \
         downstream analysis (QC, normalisation, clustering, marker-gene \
         identification) in Seurat.",
    );
    assert_eq!(
        result.modality, "single_cell_rnaseq",
        "single-cell RNA-seq + 10x Chromium + Cell Ranger + Seurat prose \
         must route to single_cell_rnaseq, got {:?}",
        result.modality
    );
}

#[test]
fn single_cell_vdj_routes_to_single_cell_vdj() {
    let clf = load_classifier();
    let result = clf.classify(
        "TCR repertoire + BCR repertoire profiling via vdj sequencing on the \
         10x v(d)j Chromium assay; cellranger vdj for clonotype assembly with \
         CDR3 + igblast annotation; downstream repertoire diversity (MiXCR / \
         immcantation pipeline).",
    );
    assert_eq!(
        result.modality, "single_cell_vdj",
        "tcr repertoire + bcr repertoire + vdj sequencing + cellranger vdj \
         prose must route to single_cell_vdj, got {:?}",
        result.modality
    );
}

#[test]
fn spatial_transcriptomics_routes_to_spatial_transcriptomics() {
    let clf = load_classifier();
    let result = clf.classify(
        "Spatial transcriptomics on 10x Genomics Visium platform with FFPE \
         tissue sections. Spot-level gene-expression deconvolution into cell \
         types with cell2location; spatially-variable gene detection.",
    );
    assert_eq!(
        result.modality, "spatial_transcriptomics",
        "Visium spatial transcriptomics + cell2location deconvolution prose \
         must route to spatial_transcriptomics, got {:?}",
        result.modality
    );
}

#[test]
fn variant_calling_routes_to_variant_calling() {
    let clf = load_classifier();
    let result = clf.classify(
        "Germline variant calling from whole-genome sequencing data using \
         GATK HaplotypeCaller with the GATK Best Practices pipeline. \
         Joint genotyping across the cohort; VCF annotation with VEP; \
         filtering against gnomAD population frequencies.",
    );
    assert_eq!(
        result.modality, "variant_calling",
        "GATK HaplotypeCaller + VCF + gnomAD prose must route to \
         variant_calling, got {:?}",
        result.modality
    );
}
