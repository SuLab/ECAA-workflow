//! `ReportData` — the canonical, deterministically-assembled summary of
//! every terminal analytical artifact plus (optionally) a literature
//! concordance rollup. Written as `report-data.json` by the report-data
//! assembler and read by the reporting agent, which narrates over it
//! without inventing numbers.
//!
//! `summarize_artifact` is the pure, filesystem-free core: given an
//! already-parsed delimited table (`rows`/`headers`) and the atom's
//! declared `ResultSchema`, it resolves every column BY NAME (never by
//! position) and computes counts, a significance filter, a direction
//! split, and an effect-magnitude distribution. All modality-specific
//! meaning enters only through the schema — this module never hardcodes
//! a gene/pathway/variant column name.

use super::{Comparator, ResultSchema};

/// Top-level `report-data.json` payload: one summary per terminal
/// artifact plus an optional literature-concordance rollup.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReportData {
    pub artifacts: Vec<ResultArtifactSummary>,
    pub literature: Option<LiteratureRollup>,
}

/// Deterministic summary of one terminal atom's primary result artifact.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ResultArtifactSummary {
    pub stage_id: String,
    pub artifact: String,
    pub n_total: u64,
    pub n_significant: Option<u64>,
    /// `Some` iff `ResultSchema::signed_effect_column` was declared and resolved.
    pub direction_split: Option<DirectionSplit>,
    pub effect_distribution: Option<Vec<DistBin>>,
    /// The ENTIRE significant set (or all rows if no significance declared).
    pub significant_entities: Vec<EntityRow>,
    /// Supplementary attachment (rel path).
    pub significant_table_path: String,
    /// Supplementary attachment (rel path).
    pub full_table_path: String,
    /// Degenerate-output guard tripped.
    pub spilled_to_attachment_only: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DirectionSplit {
    pub up: u64,
    pub down: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DistBin {
    pub label: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EntityRow {
    pub entity: String,
    pub effect: Option<f64>,
    pub significance: Option<f64>,
    pub literature: LiteratureStatus,
}

/// `#[non_exhaustive]`: wire-facing enum crossing the report-data.json
/// boundary; new statuses may be added without breaking downstream matches.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LiteratureStatus {
    Concordant { pmid: String },
    Discordant { pmid: String },
    Unverifiable { pmid: String },
    Novel,
    NotAssessed,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LiteratureRollup {
    pub concordant: Vec<LitFinding>,
    pub discordant: Vec<LitFinding>,
    pub unverifiable: Vec<LitFinding>,
    pub non_replications: Vec<NonReplication>,
    pub novel_count: u64,
    pub retrieved_sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LitFinding {
    pub entity: String,
    pub pmid: String,
    pub evidence_quote: String,
    pub effect: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NonReplication {
    pub entity: String,
    pub pmid: String,
    pub prior_claim: String,
    pub here_effect: Option<f64>,
    pub here_significance: Option<f64>,
}

/// Result of [`summarize_artifact`]: counts / significance filter /
/// direction split / effect distribution computed over one already-parsed
/// delimited table. Purely a computation result — never touches the
/// filesystem or an attachment path (those are assembled by the caller).
#[derive(Debug, Clone, PartialEq)]
pub struct ArtifactStats {
    pub n_total: u64,
    pub n_significant: Option<u64>,
    pub direction_split: Option<DirectionSplit>,
    pub effect_distribution: Option<Vec<DistBin>>,
    /// Indices into `rows` of the significant rows (or all rows when the
    /// schema declares no `significance`).
    pub significant_row_indices: Vec<usize>,
}

/// Resolves a header name to its column index. Never positional — a
/// schema's column names are matched exactly against the parsed header row.
fn resolve_column(headers: &csv::StringRecord, name: &str) -> Option<usize> {
    headers.iter().position(|h| h == name)
}

/// Parses a row's value at `idx` as a finite `f64`, excluding NA/blank/
/// unparseable/non-finite values.
fn parse_finite(row: &csv::StringRecord, idx: usize) -> Option<f64> {
    let raw = row.get(idx)?;
    let v: f64 = raw.trim().parse().ok()?;
    v.is_finite().then_some(v)
}

/// Bins `|effect|` over `[0,0.5) [0.5,1) [1,2) [2,inf)`.
fn magnitude_bin(effect: f64) -> usize {
    let a = effect.abs();
    if a < 0.5 {
        0
    } else if a < 1.0 {
        1
    } else if a < 2.0 {
        2
    } else {
        3
    }
}

const DIST_BIN_LABELS: [&str; 4] = ["0-0.5", "0.5-1", "1-2", "2+"];

/// Reads a delimited result table via its `ResultSchema` and computes
/// counts / significance / direction split / effect distribution.
/// Columns are resolved by header name only — never positionally.
pub fn summarize_artifact(
    rows: &[csv::StringRecord],
    headers: &csv::StringRecord,
    schema: &ResultSchema,
) -> ArtifactStats {
    let n_total = rows.len() as u64;

    // Resolved by the schema's declared name only — the atom declaration is
    // the sole source of truth for the column name, never a hardcoded alias.
    let effect_idx = schema.signed_effect_column.as_deref().and_then(|name| resolve_column(headers, name));

    // Distinguishes three states: no significance declared (every row
    // counts, `n_significant` stays `None`); declared and resolved (normal
    // filter); declared but the column is absent from the header
    // (unresolvable — must NOT be conflated with "zero rows pass").
    let (significant_row_indices, n_significant, significance_unresolvable): (Vec<usize>, Option<u64>, bool) =
        match &schema.significance {
            None => ((0..rows.len()).collect(), None, false),
            Some(sig) => match resolve_column(headers, &sig.column) {
                Some(sig_idx) => {
                    let indices: Vec<usize> = rows
                        .iter()
                        .enumerate()
                        .filter_map(|(i, row)| {
                            let v = parse_finite(row, sig_idx)?;
                            let hit = match sig.comparator {
                                Comparator::Lt => v < sig.threshold,
                                Comparator::Gt => v > sig.threshold,
                            };
                            hit.then_some(i)
                        })
                        .collect();
                    let n = indices.len() as u64;
                    (indices, Some(n), false)
                }
                // Declared significance column absent from the actual header:
                // unresolvable / not-assessed, never "zero significant rows found".
                None => (Vec::new(), None, true),
            },
        };

    let direction_split = (!significance_unresolvable)
        .then(|| {
            effect_idx.map(|idx| {
                let mut up = 0u64;
                let mut down = 0u64;
                for &i in &significant_row_indices {
                    if let Some(v) = parse_finite(&rows[i], idx) {
                        if v > 0.0 {
                            up += 1;
                        } else if v < 0.0 {
                            down += 1;
                        }
                    }
                }
                DirectionSplit { up, down }
            })
        })
        .flatten();

    let effect_distribution = (!significance_unresolvable)
        .then(|| {
            effect_idx.map(|idx| {
                let mut bins = [0u64; 4];
                for &i in &significant_row_indices {
                    if let Some(v) = parse_finite(&rows[i], idx) {
                        bins[magnitude_bin(v)] += 1;
                    }
                }
                DIST_BIN_LABELS
                    .iter()
                    .zip(bins)
                    .map(|(label, count)| DistBin { label: (*label).to_string(), count })
                    .collect()
            })
        })
        .flatten();

    ArtifactStats {
        n_total,
        n_significant,
        direction_split,
        effect_distribution,
        significant_row_indices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report_contract::Significance;

    /// Parses a TSV literal into `(headers, rows)` the way the assembler
    /// reads a real result artifact.
    fn tsv(s: &str) -> (csv::StringRecord, Vec<csv::StringRecord>) {
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(b'\t')
            .has_headers(true)
            .flexible(true)
            .from_reader(s.as_bytes());
        let headers = reader.headers().unwrap().clone();
        let rows: Vec<csv::StringRecord> = reader.records().map(|r| r.unwrap()).collect();
        (headers, rows)
    }

    fn de_schema() -> ResultSchema {
        ResultSchema {
            artifact: "de_results.tsv".into(),
            entity_column: "gene".into(),
            significance: Some(Significance {
                column: "padj".into(),
                threshold: 0.05,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: Some("log2FoldChange".into()),
            grouping_column: None,
        }
    }

    #[test]
    fn signed_table_counts_significant_and_direction_split() {
        let (hdr, rows) = tsv(
            "gene\tlog2FoldChange\tpadj\nGUP\t5\t0.001\nGDOWN\t-4.8\t0.002\nNS\t0.1\t0.9",
        );
        let schema = de_schema();
        let s = summarize_artifact(&rows, &hdr, &schema);
        assert_eq!(s.n_total, 3);
        assert_eq!(s.n_significant, Some(2));
        assert_eq!(s.direction_split, Some(DirectionSplit { up: 1, down: 1 }));
        assert_eq!(s.significant_row_indices, vec![0, 1]);
        let dist = s.effect_distribution.expect("effect column resolved");
        let total: u64 = dist.iter().map(|b| b.count).sum();
        assert_eq!(total, 2);
        assert!(dist.iter().any(|b| b.count > 0));
    }

    #[test]
    fn unsigned_table_has_no_direction_split_or_distribution() {
        let (hdr, rows) = tsv("variant_id\tqual\nv1\t40\nv2\t10");
        let schema = ResultSchema {
            artifact: "variants.tsv".into(),
            entity_column: "variant_id".into(),
            significance: Some(Significance {
                column: "qual".into(),
                threshold: 30.0,
                comparator: Comparator::Gt,
            }),
            signed_effect_column: None,
            grouping_column: None,
        };
        let s = summarize_artifact(&rows, &hdr, &schema);
        assert_eq!(s.n_total, 2);
        assert_eq!(s.n_significant, Some(1)); // qual>30
        assert_eq!(s.direction_split, None);
        assert_eq!(s.effect_distribution, None);
        assert_eq!(s.significant_row_indices, vec![0]);
    }

    #[test]
    fn no_significance_declared_reports_all_rows() {
        let (hdr, rows) = tsv("gene\tlog2FoldChange\nA\t1\nB\t-2\nC\t0.5");
        let schema = ResultSchema {
            artifact: "x.tsv".into(),
            entity_column: "gene".into(),
            significance: None,
            signed_effect_column: Some("log2FoldChange".into()),
            grouping_column: None,
        };
        let s = summarize_artifact(&rows, &hdr, &schema);
        assert_eq!(s.n_total, 3);
        assert_eq!(s.n_significant, None);
        assert_eq!(s.significant_row_indices, vec![0, 1, 2]);
        // Direction split/distribution still computed over "all rows" since
        // an effect column resolved, even though nothing was filtered.
        assert_eq!(s.direction_split, Some(DirectionSplit { up: 2, down: 1 }));
    }

    #[test]
    fn declared_effect_column_absent_yields_no_direction_split() {
        // Schema-only resolution (no hardcoded aliases): the schema declares
        // `log2FoldChange` but this table only has `log2FC`, so the effect
        // column does not resolve → no direction split / distribution.
        // Significance still computed off the resolvable `padj` column.
        let (hdr, rows) = tsv("gene\tlog2FC\tpadj\nA\t3\t0.01\n");
        let schema = de_schema();
        let s = summarize_artifact(&rows, &hdr, &schema);
        assert_eq!(s.n_significant, Some(1));
        assert_eq!(s.direction_split, None);
        assert_eq!(s.effect_distribution, None);
    }

    #[test]
    fn declared_significance_column_absent_is_unresolvable_not_zero() {
        // The schema declares significance on `padj`, but this table has no
        // `padj` column. `n_significant` must be None (unresolvable), NOT
        // Some(0) which would falsely read as "zero significant rows found".
        let (hdr, rows) = tsv("gene\tlog2FoldChange\nA\t3\nB\t-1");
        let schema = de_schema();
        let s = summarize_artifact(&rows, &hdr, &schema);
        assert_eq!(s.n_significant, None);
        assert!(s.significant_row_indices.is_empty());
        assert_eq!(s.direction_split, None);
    }
}
