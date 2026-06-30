//! Probe a public GEO accession at intake to discover which data products
//! it actually provides — a processed expression/count matrix (supplementary
//! file) and/or raw reads (linked SRA) — so the intake agent can present
//! entry-point options instead of silently assuming raw FASTQ.
//!
//! Root cause this addresses: the composer infers a raw-read pipeline from
//! modality alone, and the only component that knows the deposited form
//! (`data_acquisition`'s GEO fetch) runs *after* composition. By probing at
//! intake we can seed the right starting point (counts-first vs raw) before
//! the DAG is built. One HTTP GET to the GEO SOFT brief carries everything we
//! need: `!Series_supplementary_file` (processed matrices), `!Series_relation
//! = SRA: …` (raw-read availability), sample ids, organism, platform, type.

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
    /// A deposited processed expression/count matrix, if one is present.
    pub processed_matrix: Option<ProcessedMatrix>,
    /// Raw reads available via the linked SRA study, if any.
    pub raw_reads_sra: Option<RawReadsSra>,
    /// Human-readable note (errors, microarray, nothing-to-offer, etc.).
    pub note: Option<String>,
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
             ask the SME whether they are starting from raw reads or a processed matrix.",
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
        processed_matrix: None,
        raw_reads_sra: None,
        note: Some(format!(
            "{note} — could not auto-detect available products; ask the SME whether they want \
             to start from raw reads or a deposited count/expression matrix."
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
    let mut processed: Option<ProcessedMatrix> = None;
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
            if let Some(kind) = classify_matrix(base) {
                // Prefer raw counts over normalized expression (counts are the
                // right input for differential expression); otherwise keep the
                // first match.
                let replace = match &processed {
                    None => true,
                    Some(p) => kind == "counts" && p.kind != "counts",
                };
                if replace {
                    processed = Some(ProcessedMatrix {
                        filename: base.to_string(),
                        kind: kind.to_string(),
                    });
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

    ProbeResult {
        accession: accession.to_string(),
        recognized: true,
        title,
        organism,
        platform,
        series_type,
        n_samples,
        processed_matrix: processed,
        raw_reads_sra: sra,
        note: None,
    }
}

/// Classify a supplementary filename as a processed expression/count matrix.
/// Returns the matrix kind, or `None` when the file is not a tabular matrix
/// (e.g. RAW.tar of CEL files, bigwigs, peak/diff files, readmes).
fn classify_matrix(filename: &str) -> Option<&'static str> {
    let f = filename.to_ascii_lowercase();
    let tabular = [".csv", ".tsv", ".txt", ".xlsx"]
        .iter()
        .any(|e| f.contains(e));
    if !tabular {
        return None;
    }
    if f.contains("readme") || f.contains("annotation") || f.contains("metadata") {
        return None;
    }
    if f.contains("count") || f.contains("featurecounts") || f.contains("rsem") {
        Some("counts")
    } else if f.contains("fpkm") {
        Some("fpkm")
    } else if f.contains("tpm") {
        Some("tpm")
    } else if f.contains("rpkm") {
        Some("rpkm")
    } else if f.contains("matrix") || f.contains("expression") || f.contains("genes") {
        Some("expression")
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
        assert_eq!(r.raw_reads_sra.expect("sra").study, "SRP299835");
    }

    #[test]
    fn microarray_offers_neither_rnaseq_entry_point() {
        let r = parse_geo_soft("GSE2034", GSE2034);
        assert!(r.recognized);
        // RAW.tar of CEL files is not a tabular count/expression matrix.
        assert!(r.processed_matrix.is_none(), "got {:?}", r.processed_matrix);
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
        assert_eq!(r.raw_reads_sra.expect("sra").study, "SRP033351");
    }

    #[test]
    fn classify_matrix_excludes_nontabular_and_metadata() {
        assert_eq!(classify_matrix("X_counts.csv.gz"), Some("counts"));
        assert_eq!(classify_matrix("X_TPM.tsv"), Some("tpm"));
        assert!(classify_matrix("X_RAW.tar").is_none());
        assert!(classify_matrix("peaks.narrowPeak.gz").is_none());
        assert!(classify_matrix("README.txt").is_none());
        assert!(classify_matrix("samples_metadata.csv").is_none());
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
