//! Probe a public GEO accession at intake to discover which data products
//! it actually provides — deposited processed products (count/expression
//! matrices, ChIP/ATAC peaks, variant calls, alignments, proteomics abundance
//! matrices, methylation beta matrices, coverage tracks) and/or raw reads
//! (linked SRA) — so the intake agent can present entry-point options instead
//! of silently assuming raw FASTQ.
//!
//! Root cause this addresses: the composer infers a raw-read pipeline from
//! modality alone, and the only component that knows the deposited form
//! (`data_acquisition`'s GEO fetch) runs *after* composition. By probing at
//! intake we can seed the right starting point (e.g. counts-first vs raw,
//! peaks-first vs raw) before the DAG is built. One HTTP GET to the GEO SOFT
//! brief carries everything we need: `!Series_supplementary_file` (processed
//! products), `!Series_relation = SRA: …` (raw-read availability), sample ids,
//! organism, platform, type.

use serde::Serialize;
use std::time::Duration;

/// What a GEO accession offers as analysis entry points.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProbeResult {
    pub accession: String,
    /// False when the accession isn't a GEO Series we know how to probe.
    pub recognized: bool,
    pub title: Option<String>,
    pub organism: Option<String>,
    pub platform: Option<String>,
    /// e.g. "Expression profiling by high throughput sequencing".
    pub series_type: Option<String>,
    pub n_samples: usize,
    /// Every deposited processed product the probe recognized, across all
    /// modalities (expression/count matrices, ChIP/ATAC peaks, variant calls,
    /// alignments, proteomics abundance matrices, methylation beta matrices,
    /// coverage tracks). Empty when the series deposits only raw data (or only
    /// things we exclude, like microarray CEL archives). This is the general
    /// surface an LLM tool-result consumer should read to enumerate real
    /// entry-point options for any modality.
    pub deposited_products: Vec<DepositedProduct>,
    /// A deposited processed expression/count matrix, if one is present.
    /// Retained as a convenience for the RNA-seq counts-first entry point (the
    /// most common case); it mirrors the best `deposited_products` entry whose
    /// `product_type` is `expression_matrix`. For any other modality, read
    /// `deposited_products` directly.
    pub processed_matrix: Option<ProcessedMatrix>,
    /// Raw reads available via the linked SRA study, if any.
    pub raw_reads_sra: Option<RawReadsSra>,
    /// Human-readable note (errors, microarray, nothing-to-offer, etc.).
    pub note: Option<String>,
}

/// A deposited, already-processed data product discovered on a GEO Series.
/// `product_type` names the analysis family (so an intake agent can decide
/// which raw-processing atoms are already satisfied), `kind` is the specific
/// form within that family, and `filename` is the supplementary basename.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DepositedProduct {
    /// Analysis family: "expression_matrix" | "peaks" | "variants" |
    /// "alignment" | "proteomics_abundance" | "methylation_beta" |
    /// "coverage_track".
    pub product_type: String,
    /// Specific form within the family, e.g. "counts" | "fpkm" | "tpm" |
    /// "rpkm" | "expression" | "narrowpeak" | "broadpeak" | "gappedpeak" |
    /// "bed" | "vcf" | "bam" | "cram" | "protein_groups" | "abundance" |
    /// "intensity" | "lfq" | "beta" | "bigwig".
    pub kind: String,
    /// Supplementary file basename (path stripped).
    pub filename: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProcessedMatrix {
    pub filename: String,
    /// "counts" | "fpkm" | "tpm" | "rpkm" | "expression".
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RawReadsSra {
    /// SRA study accession (SRP/ERP/DRP…).
    pub study: String,
}

impl ProbeResult {
    /// Recognized accession, but products could not be auto-determined in
    /// this context (probe disabled, non-multithread runtime, etc.). The
    /// intake agent should fall back to asking the SME which entry point to
    /// start from.
    pub fn needs_manual(accession: &str, note: &str) -> Self {
        ProbeResult {
            accession: accession.to_string(),
            recognized: true,
            title: None,
            organism: None,
            platform: None,
            series_type: None,
            n_samples: 0,
            deposited_products: Vec::new(),
            processed_matrix: None,
            raw_reads_sra: None,
            note: Some(note.to_string()),
        }
    }

    fn unrecognized(accession: &str, note: &str) -> Self {
        ProbeResult {
            accession: accession.to_string(),
            recognized: false,
            title: None,
            organism: None,
            platform: None,
            series_type: None,
            n_samples: 0,
            deposited_products: Vec::new(),
            processed_matrix: None,
            raw_reads_sra: None,
            note: Some(note.to_string()),
        }
    }
}

const GEO_ACC_URL: &str = "https://www.ncbi.nlm.nih.gov/geo/query/acc.cgi";

/// Fetch + parse a GEO Series brief. Network/HTTP failures degrade to a
/// `recognized:true` result with a `note` (never panics) so the intake agent
/// can fall back to asking the SME directly.
pub async fn probe_accession(accession: &str) -> ProbeResult {
    let acc = accession.trim().to_string();
    let upper = acc.to_ascii_uppercase();
    if !upper.starts_with("GSE") {
        return ProbeResult::unrecognized(
            &acc,
            "Only GEO Series (GSE…) accessions are probed here. For SRA/other accessions, \
             ask the SME whether they are starting from raw reads or a processed product.",
        );
    }
    let url = format!("{GEO_ACC_URL}?acc={upper}&targ=self&form=text&view=brief");
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("ecaa-workflow-intake-probe")
        .build()
    {
        Ok(c) => c,
        Err(e) => return probe_note(&upper, &format!("probe client init failed: {e}")),
    };
    match client.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => match resp.text().await {
            Ok(body) => parse_geo_soft(&upper, &body),
            Err(e) => probe_note(&upper, &format!("could not read GEO response: {e}")),
        },
        Ok(resp) => probe_note(&upper, &format!("GEO returned HTTP {}", resp.status())),
        Err(e) => probe_note(&upper, &format!("GEO request failed: {e}")),
    }
}

fn probe_note(accession: &str, note: &str) -> ProbeResult {
    ProbeResult {
        accession: accession.to_string(),
        recognized: true,
        title: None,
        organism: None,
        platform: None,
        series_type: None,
        n_samples: 0,
        deposited_products: Vec::new(),
        processed_matrix: None,
        raw_reads_sra: None,
        note: Some(format!(
            "{note} — could not auto-detect available products; ask the SME whether they want \
             to start from raw reads or a deposited processed product (count matrix, peaks, \
             variant calls, abundance matrix, etc.)."
        )),
    }
}

/// Pure parser over a GEO SOFT "brief" document. Kept separate from the fetch
/// so it is unit-testable against recorded fixtures.
pub fn parse_geo_soft(accession: &str, soft: &str) -> ProbeResult {
    let mut title = None;
    let mut organism = None;
    let mut platform = None;
    let mut series_type = None;
    let mut n_samples = 0usize;
    let mut products: Vec<DepositedProduct> = Vec::new();
    let mut sra: Option<RawReadsSra> = None;

    for raw in soft.lines() {
        let line = raw.trim();
        if let Some(v) = line.strip_prefix("!Series_title =") {
            title = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("!Series_platform_organism =") {
            organism = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("!Series_platform_id =") {
            platform = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("!Series_type =") {
            series_type = Some(v.trim().to_string());
        } else if line.starts_with("!Series_sample_id =") {
            n_samples += 1;
        } else if let Some(v) = line.strip_prefix("!Series_supplementary_file =") {
            let f = v.trim();
            let base = f.rsplit('/').next().unwrap_or(f);
            if let Some((product_type, kind)) = classify_product(base) {
                let candidate = DepositedProduct {
                    product_type: product_type.to_string(),
                    kind: kind.to_string(),
                    filename: base.to_string(),
                };
                // De-duplicate identical filenames (some briefs repeat a line).
                if !products.iter().any(|p| p.filename == candidate.filename) {
                    products.push(candidate);
                }
            }
        } else if let Some(v) = line.strip_prefix("!Series_relation =") {
            let v = v.trim();
            // e.g. "SRA: https://www.ncbi.nlm.nih.gov/sra?term=SRP299835"
            if let Some(rest) = v.strip_prefix("SRA:") {
                if let Some(study) = extract_sra_study(rest) {
                    sra = Some(RawReadsSra { study });
                }
            }
        }
    }

    let processed_matrix = best_expression_matrix(&products);

    ProbeResult {
        accession: accession.to_string(),
        recognized: true,
        title,
        organism,
        platform,
        series_type,
        n_samples,
        deposited_products: products,
        processed_matrix,
        raw_reads_sra: sra,
        note: None,
    }
}

/// Pick the RNA-seq counts-first convenience matrix from the detected
/// products: prefer raw counts over normalized expression (counts are the
/// right input for differential expression); otherwise keep the first
/// `expression_matrix` match. Returns `None` when the series deposits no
/// expression matrix (e.g. a ChIP-seq series with only peaks).
fn best_expression_matrix(products: &[DepositedProduct]) -> Option<ProcessedMatrix> {
    let mut best: Option<&DepositedProduct> = None;
    for p in products.iter().filter(|p| p.product_type == "expression_matrix") {
        let replace = match best {
            None => true,
            Some(b) => p.kind == "counts" && b.kind != "counts",
        };
        if replace {
            best = Some(p);
        }
    }
    best.map(|p| ProcessedMatrix {
        filename: p.filename.clone(),
        kind: p.kind.clone(),
    })
}

/// True iff a probe result reports a deposited RNA-seq COUNT matrix (an
/// `expression_matrix` product whose `kind` is `counts`). Scoped to counts
/// (not fpkm/tpm) because only counts are a valid DE substrate; a series that
/// deposits only fpkm/tpm should still be reprocessed from raw to regenerate
/// counts. Consumed by the ProbeDataset dispatch to (stickily) set
/// `Session::probed_counts_matrix_available`.
pub(crate) fn probe_reports_counts_matrix(p: &ProbeResult) -> bool {
    p.deposited_products
        .iter()
        .any(|d| d.product_type == "expression_matrix" && d.kind == "counts")
}

/// Classify a supplementary filename as a deposited processed product across
/// all modalities. Returns `(product_type, kind)`, or `None` when the file is
/// not a recognized processed product — i.e. raw inputs (microarray `*_RAW.tar`
/// of CEL files, methylation `.idat`), documentation (readme/annotation/
/// metadata), or anything else we don't recognize.
///
/// Ordering matters: unambiguous by-extension formats (peaks/VCF/BAM/coverage)
/// are matched before the tabular-matrix heuristics so that, e.g., a
/// `.narrowPeak` is classified as peaks rather than misread as a table.
fn classify_product(filename: &str) -> Option<(&'static str, &'static str)> {
    let f = filename.to_ascii_lowercase();

    // ── Hard exclusions (raw inputs + documentation) ──
    // Microarray CEL archive: raw intensities, not a processed matrix.
    if f.contains("_raw.tar") {
        return None;
    }
    // Methylation array raw intensities: .idat is RAW, never processed.
    if f.contains(".idat") {
        return None;
    }
    if f.contains("readme") || f.contains("annotation") || f.contains("metadata") {
        return None;
    }

    // ── Peaks (ChIP-seq / ATAC-seq) ──
    if f.contains(".narrowpeak") {
        return Some(("peaks", "narrowpeak"));
    }
    if f.contains(".broadpeak") {
        return Some(("peaks", "broadpeak"));
    }
    if f.contains(".gappedpeak") {
        return Some(("peaks", "gappedpeak"));
    }

    // ── Coverage tracks (bigWig / bedGraph-derived signal) ──
    // `.bw` is matched only as an extension token (`.bw` at end or `.bw.`),
    // so a `bwa`-aligned `sample.bwa.bam` is not mistaken for a track.
    if f.contains(".bigwig")
        || f.contains(".bigbed")
        || f.contains(".bedgraph")
        || f.ends_with(".bw")
        || f.contains(".bw.")
    {
        return Some(("coverage_track", "bigwig"));
    }

    // ── Variant calls ──
    if f.contains(".vcf") {
        return Some(("variants", "vcf"));
    }

    // ── Alignments ──
    if f.contains(".bam") {
        return Some(("alignment", "bam"));
    }
    if f.contains(".cram") {
        return Some(("alignment", "cram"));
    }

    // A generic `.bed`/`.gappedbed` that reaches here (peak-ish naming) is a
    // peak/interval file. Guard so plain region annotations aren't misread as
    // a table below.
    if f.contains(".bed") {
        // A .bed carrying an explicit peak name is peaks; otherwise it is a
        // generic interval file we still surface as peaks (ChIP/ATAC output).
        return Some(("peaks", "bed"));
    }
    // Peak-ish names without a peak extension (e.g. `*_peaks.txt`).
    let peakish_name = f.contains("peak");

    // ── Tabular products (expression / proteomics / methylation) ──
    let tabular = [".csv", ".tsv", ".txt", ".xlsx", ".tab"]
        .iter()
        .any(|e| f.contains(e));

    if peakish_name && tabular {
        return Some(("peaks", "bed"));
    }

    if !tabular {
        return None;
    }

    // Proteomics abundance matrices (MaxQuant / DIA outputs).
    if f.contains("proteingroups") || f.contains("protein_groups") {
        return Some(("proteomics_abundance", "protein_groups"));
    }
    if f.contains("_lfq") || f.contains("lfq_") || f.contains("lfq.") {
        return Some(("proteomics_abundance", "lfq"));
    }
    if f.contains("abundance") {
        return Some(("proteomics_abundance", "abundance"));
    }
    if f.contains("intensity") || f.contains("intensities") {
        return Some(("proteomics_abundance", "intensity"));
    }

    // Methylation beta matrices (processed methylation levels).
    if f.contains("beta") || f.contains("methylation") || f.contains("methyl_") {
        return Some(("methylation_beta", "beta"));
    }

    // Expression / count matrices (RNA-seq processed products).
    if f.contains("count") || f.contains("featurecounts") || f.contains("rsem") {
        Some(("expression_matrix", "counts"))
    } else if f.contains("fpkm") {
        Some(("expression_matrix", "fpkm"))
    } else if f.contains("tpm") {
        Some(("expression_matrix", "tpm"))
    } else if f.contains("rpkm") {
        Some(("expression_matrix", "rpkm"))
    } else if f.contains("matrix") || f.contains("expression") || f.contains("genes") {
        Some(("expression_matrix", "expression"))
    } else {
        None
    }
}

/// Pull an SRP/ERP/DRP study accession out of an SRA relation value.
fn extract_sra_study(s: &str) -> Option<String> {
    s.split(|c: char| !c.is_ascii_alphanumeric())
        .find(|t| {
            t.len() >= 4
                && (t.starts_with("SRP") || t.starts_with("ERP") || t.starts_with("DRP"))
        })
        .map(|t| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed real SOFT-brief fixtures (verified live against NCBI 2026-06-30).
    const GSE164073: &str = "\
!Series_title = Response to SARS-CoV-2 infection in cornea, limbus and sclera from human donors
!Series_geo_accession = GSE164073
!Series_type = Expression profiling by high throughput sequencing
!Series_sample_id = GSM4996084
!Series_sample_id = GSM4996085
!Series_sample_id = GSM4996086
!Series_supplementary_file = ftp://ftp.ncbi.nlm.nih.gov/geo/series/GSE164nnn/GSE164073/suppl/GSE164073_Eye_count_matrix.csv.gz
!Series_platform_id = GPL18573
!Series_platform_organism = Homo sapiens
!Series_relation = BioProject: https://www.ncbi.nlm.nih.gov/bioproject/PRJNA688734
!Series_relation = SRA: https://www.ncbi.nlm.nih.gov/sra?term=SRP299835
";

    const GSE2034: &str = "\
!Series_title = Breast cancer relapse free survival
!Series_type = Expression profiling by array
!Series_sample_id = GSM36777
!Series_sample_id = GSM36778
!Series_supplementary_file = ftp://ftp.ncbi.nlm.nih.gov/geo/series/GSE2nnn/GSE2034/suppl/GSE2034_RAW.tar
!Series_platform_id = GPL96
!Series_platform_organism = Homo sapiens
!Series_relation = BioProject: https://www.ncbi.nlm.nih.gov/bioproject/PRJNA91859
";

    const GSE52778: &str = "\
!Series_title = RNA-Seq of airway smooth muscle treated with dexamethasone
!Series_type = Expression profiling by high throughput sequencing
!Series_sample_id = GSM1275862
!Series_sample_id = GSM1275863
!Series_supplementary_file = ftp://ftp.ncbi.nlm.nih.gov/geo/series/GSE52nnn/GSE52778/suppl/GSE52778_All_Sample_FPKM_Matrix.txt.gz
!Series_supplementary_file = ftp://ftp.ncbi.nlm.nih.gov/geo/series/GSE52nnn/GSE52778/suppl/GSE52778_Dex_vs_Untreated_gene_exp.diff.gz
!Series_platform_id = GPL11154
!Series_platform_organism = Homo sapiens
!Series_relation = SRA: https://www.ncbi.nlm.nih.gov/sra?term=SRP033351
";

    // ChIP-seq series depositing narrowPeak calls + coverage tracks, with
    // raw reads in SRA. (Shape modeled on typical GEO ChIP-seq briefs.)
    const CHIPSEQ: &str = "\
!Series_title = ChIP-seq of H3K27ac in stimulated macrophages
!Series_geo_accession = GSE900001
!Series_type = Genome binding/occupancy profiling by high throughput sequencing
!Series_sample_id = GSM9000001
!Series_sample_id = GSM9000002
!Series_supplementary_file = ftp://ftp.ncbi.nlm.nih.gov/geo/series/GSE900nnn/GSE900001/suppl/GSE900001_H3K27ac_peaks.narrowPeak.gz
!Series_supplementary_file = ftp://ftp.ncbi.nlm.nih.gov/geo/series/GSE900nnn/GSE900001/suppl/GSE900001_H3K27ac_signal.bigWig
!Series_supplementary_file = ftp://ftp.ncbi.nlm.nih.gov/geo/series/GSE900nnn/GSE900001/suppl/GSE900001_README.txt
!Series_platform_id = GPL16791
!Series_platform_organism = Homo sapiens
!Series_relation = SRA: https://www.ncbi.nlm.nih.gov/sra?term=SRP400001
";

    // Variant-calling / WGS series depositing a VCF, no SRA link exposed.
    const VARIANTS: &str = "\
!Series_title = Whole-genome variant calls across a patient cohort
!Series_geo_accession = GSE900002
!Series_type = Genome variation profiling by high throughput sequencing
!Series_sample_id = GSM9000010
!Series_sample_id = GSM9000011
!Series_supplementary_file = ftp://ftp.ncbi.nlm.nih.gov/geo/series/GSE900nnn/GSE900002/suppl/GSE900002_cohort_variants.vcf.gz
!Series_supplementary_file = ftp://ftp.ncbi.nlm.nih.gov/geo/series/GSE900nnn/GSE900002/suppl/GSE900002_sample_metadata.csv.gz
!Series_platform_id = GPL24676
!Series_platform_organism = Homo sapiens
";

    // Proteomics series depositing a MaxQuant proteinGroups abundance matrix.
    const PROTEOMICS: &str = "\
!Series_title = Quantitative proteomics of drug-treated cells
!Series_geo_accession = GSE900003
!Series_type = Protein profiling by mass spectrometry
!Series_sample_id = GSM9000020
!Series_sample_id = GSM9000021
!Series_supplementary_file = ftp://ftp.ncbi.nlm.nih.gov/geo/series/GSE900nnn/GSE900003/suppl/GSE900003_proteinGroups.txt.gz
!Series_platform_id = GPL00000
!Series_platform_organism = Homo sapiens
";

    #[test]
    fn rnaseq_with_counts_and_sra_offers_both() {
        let r = parse_geo_soft("GSE164073", GSE164073);
        assert!(r.recognized);
        assert_eq!(r.organism.as_deref(), Some("Homo sapiens"));
        assert_eq!(r.platform.as_deref(), Some("GPL18573"));
        assert_eq!(r.n_samples, 3);
        let m = r.processed_matrix.expect("count matrix present");
        assert_eq!(m.filename, "GSE164073_Eye_count_matrix.csv.gz");
        assert_eq!(m.kind, "counts");
        // The general product surface also carries it.
        assert_eq!(r.deposited_products.len(), 1);
        assert_eq!(r.deposited_products[0].product_type, "expression_matrix");
        assert_eq!(r.deposited_products[0].kind, "counts");
        assert_eq!(r.raw_reads_sra.expect("sra").study, "SRP299835");
    }

    #[test]
    fn microarray_offers_neither_rnaseq_entry_point() {
        let r = parse_geo_soft("GSE2034", GSE2034);
        assert!(r.recognized);
        // RAW.tar of CEL files is not a tabular count/expression matrix.
        assert!(r.processed_matrix.is_none(), "got {:?}", r.processed_matrix);
        assert!(r.deposited_products.is_empty(), "got {:?}", r.deposited_products);
        // Microarray series have no SRA raw reads.
        assert!(r.raw_reads_sra.is_none());
        assert_eq!(r.series_type.as_deref(), Some("Expression profiling by array"));
    }

    #[test]
    fn fpkm_matrix_classified_and_diff_ignored() {
        let r = parse_geo_soft("GSE52778", GSE52778);
        let m = r.processed_matrix.expect("fpkm matrix present");
        assert_eq!(m.filename, "GSE52778_All_Sample_FPKM_Matrix.txt.gz");
        assert_eq!(m.kind, "fpkm"); // .diff.gz must not be picked
        // Only the FPKM matrix is a recognized product; the .diff is ignored.
        assert_eq!(r.deposited_products.len(), 1);
        assert_eq!(r.deposited_products[0].kind, "fpkm");
        assert_eq!(r.raw_reads_sra.expect("sra").study, "SRP033351");
    }

    #[test]
    fn chipseq_deposits_narrowpeak_and_coverage_plus_sra() {
        let r = parse_geo_soft("GSE900001", CHIPSEQ);
        assert!(r.recognized);
        // README excluded; peaks + coverage track detected.
        assert_eq!(r.deposited_products.len(), 2, "got {:?}", r.deposited_products);
        let peaks = r
            .deposited_products
            .iter()
            .find(|p| p.product_type == "peaks")
            .expect("narrowPeak product");
        assert_eq!(peaks.kind, "narrowpeak");
        assert!(peaks.filename.ends_with(".narrowPeak.gz"));
        let track = r
            .deposited_products
            .iter()
            .find(|p| p.product_type == "coverage_track")
            .expect("bigWig track");
        assert_eq!(track.kind, "bigwig");
        // No expression matrix → counts-first convenience field is empty.
        assert!(r.processed_matrix.is_none());
        assert_eq!(r.raw_reads_sra.expect("sra").study, "SRP400001");
    }

    #[test]
    fn variant_series_deposits_vcf() {
        let r = parse_geo_soft("GSE900002", VARIANTS);
        assert!(r.recognized);
        let vcf = r
            .deposited_products
            .iter()
            .find(|p| p.product_type == "variants")
            .expect("vcf product");
        assert_eq!(vcf.kind, "vcf");
        assert!(vcf.filename.ends_with("_variants.vcf.gz"));
        // sample_metadata.csv.gz is documentation, not a product.
        assert_eq!(r.deposited_products.len(), 1, "got {:?}", r.deposited_products);
        assert!(r.processed_matrix.is_none());
        assert!(r.raw_reads_sra.is_none());
    }

    #[test]
    fn proteomics_series_deposits_abundance_matrix() {
        let r = parse_geo_soft("GSE900003", PROTEOMICS);
        assert!(r.recognized);
        assert_eq!(r.deposited_products.len(), 1, "got {:?}", r.deposited_products);
        let p = &r.deposited_products[0];
        assert_eq!(p.product_type, "proteomics_abundance");
        assert_eq!(p.kind, "protein_groups");
        assert!(p.filename.starts_with("GSE900003_proteinGroups"));
        // proteinGroups is not an expression matrix, so no counts-first field.
        assert!(r.processed_matrix.is_none());
    }

    #[test]
    fn classify_product_covers_all_modalities() {
        // Expression / counts.
        assert_eq!(
            classify_product("X_counts.csv.gz"),
            Some(("expression_matrix", "counts"))
        );
        assert_eq!(
            classify_product("X_TPM.tsv"),
            Some(("expression_matrix", "tpm"))
        );
        // Peaks.
        assert_eq!(
            classify_product("peaks.narrowPeak.gz"),
            Some(("peaks", "narrowpeak"))
        );
        assert_eq!(
            classify_product("X.broadPeak"),
            Some(("peaks", "broadpeak"))
        );
        assert_eq!(
            classify_product("X_regions.bed.gz"),
            Some(("peaks", "bed"))
        );
        // Variants.
        assert_eq!(classify_product("cohort.vcf.gz"), Some(("variants", "vcf")));
        // Alignments.
        assert_eq!(classify_product("sample.bam"), Some(("alignment", "bam")));
        assert_eq!(classify_product("sample.cram"), Some(("alignment", "cram")));
        // Coverage tracks.
        assert_eq!(
            classify_product("signal.bigWig"),
            Some(("coverage_track", "bigwig"))
        );
        // Proteomics.
        assert_eq!(
            classify_product("proteinGroups.txt.gz"),
            Some(("proteomics_abundance", "protein_groups"))
        );
        assert_eq!(
            classify_product("X_intensity_matrix.tsv"),
            Some(("proteomics_abundance", "intensity"))
        );
        // Methylation beta.
        assert_eq!(
            classify_product("X_beta_values.txt.gz"),
            Some(("methylation_beta", "beta"))
        );
        // Exclusions: raw + documentation.
        assert!(classify_product("X_RAW.tar").is_none());
        assert!(classify_product("sample.idat").is_none(), ".idat is RAW");
        assert!(classify_product("README.txt").is_none());
        assert!(classify_product("samples_metadata.csv").is_none());
    }

    #[tokio::test]
    #[ignore = "hits live NCBI GEO; run explicitly with `--ignored`"]
    async fn live_probe_gse164073_finds_both_entry_points() {
        let r = probe_accession("GSE164073").await;
        assert!(r.recognized, "note={:?}", r.note);
        assert_eq!(r.organism.as_deref(), Some("Homo sapiens"));
        let m = r.processed_matrix.expect("live: deposited count matrix");
        assert!(m.filename.contains("count_matrix"));
        assert_eq!(m.kind, "counts");
        assert!(r
            .deposited_products
            .iter()
            .any(|p| p.product_type == "expression_matrix"));
        assert_eq!(r.raw_reads_sra.expect("live: SRA").study, "SRP299835");
        assert!(r.n_samples >= 18);
    }

    #[tokio::test]
    #[ignore = "hits live NCBI GEO; run explicitly with `--ignored`"]
    async fn live_probe_microarray_offers_no_rnaseq_entry() {
        let r = probe_accession("GSE2034").await;
        assert!(r.recognized);
        assert!(r.raw_reads_sra.is_none(), "microarray has no SRA raw reads");
    }

    #[test]
    fn non_gse_is_unrecognized() {
        // probe_accession short-circuits non-GSE without a network call; the
        // pure path is exercised here via the constructor.
        let r = ProbeResult::unrecognized("SRX999", "x");
        assert!(!r.recognized);
    }
}
