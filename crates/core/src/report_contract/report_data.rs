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

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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

/// Degenerate-output guard: above this many significant rows, the inline
/// `significant_entities` list is dropped from `report-data.json` (the
/// supplementary `.significant.tsv` is still written in full) so a
/// pathological result set can't balloon the report payload.
pub const SPILL_THRESHOLD: usize = 250_000;

/// Whether the degenerate-output guard trips for a significant set of this
/// size. Callers building a [`ResultArtifactSummary`] use this to decide
/// `spilled_to_attachment_only` and whether to embed `significant_entities`.
pub fn should_spill(significant_count: usize) -> bool {
    significant_count > SPILL_THRESHOLD
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

/// Writes the two supplementary result tables for one terminal artifact:
/// `<artifact_stem>.significant.tsv` (header + only `sig_indices` rows) and
/// `<artifact_stem>.full.tsv` (header + every row). Column order is exactly
/// `headers` — no reordering, so the supplementary tables are a strict row
/// subset/superset of the source artifact. Returns the two file names
/// (relative to `dir`; the caller prefixes with the stage's output-dir path
/// to build a package-relative attachment path).
///
/// Always writes the significant table, even when the degenerate-output
/// guard ([`should_spill`]) trips — spilling only suppresses the INLINE
/// `significant_entities` list in `report-data.json`, never the on-disk
/// attachment.
pub fn write_supplementary(
    dir: &Path,
    artifact_stem: &str,
    headers: &csv::StringRecord,
    rows: &[csv::StringRecord],
    sig_indices: &[usize],
) -> std::io::Result<(String, String)> {
    std::fs::create_dir_all(dir)?;

    let significant_name = format!("{artifact_stem}.significant.tsv");
    let full_name = format!("{artifact_stem}.full.tsv");

    let mut sig_writer = csv::WriterBuilder::new()
        .delimiter(b'\t')
        .from_path(dir.join(&significant_name))?;
    sig_writer.write_record(headers)?;
    for &i in sig_indices {
        sig_writer.write_record(&rows[i])?;
    }
    sig_writer.flush()?;

    let mut full_writer = csv::WriterBuilder::new()
        .delimiter(b'\t')
        .from_path(dir.join(&full_name))?;
    full_writer.write_record(headers)?;
    for row in rows {
        full_writer.write_record(row)?;
    }
    full_writer.flush()?;

    Ok((significant_name, full_name))
}

/// Builds the inline `EntityRow`s for a set of row indices, resolving
/// entity/effect/significance columns BY NAME from the schema (never
/// positionally). Returns an empty `Vec` when `entity_column` doesn't
/// resolve against `headers` — the caller has no reliable identifier to
/// key each row on. `literature` starts at `NotAssessed`; the caller runs
/// [`join_literature`] to fill it in.
pub(crate) fn build_entity_rows(
    rows: &[csv::StringRecord],
    headers: &csv::StringRecord,
    schema: &ResultSchema,
    indices: &[usize],
) -> Vec<EntityRow> {
    let Some(entity_idx) = resolve_column(headers, &schema.entity_column) else {
        return Vec::new();
    };
    let effect_idx = schema
        .signed_effect_column
        .as_deref()
        .and_then(|name| resolve_column(headers, name));
    let significance_idx = schema
        .significance
        .as_ref()
        .and_then(|sig| resolve_column(headers, &sig.column));

    indices
        .iter()
        .filter_map(|&i| {
            let row = &rows[i];
            let entity = row.get(entity_idx)?.to_string();
            let effect = effect_idx.and_then(|idx| parse_finite(row, idx));
            let significance = significance_idx.and_then(|idx| parse_finite(row, idx));
            Some(EntityRow {
                entity,
                effect,
                significance,
                literature: LiteratureStatus::NotAssessed,
            })
        })
        .collect()
}

/// One `claims_evidence_matrix.csv` row, keyed by both `finding_id` and
/// `entity` (see [`join_literature`]).
#[derive(Debug, Clone)]
struct MatrixRow {
    concordance_flag: String,
    pmid: String,
    effect: Option<f64>,
    evidence_quote: String,
}

/// Reads `contextualize_findings_with_literature/result.json`'s
/// `excluded_nonsig` map (prior-reported entities that were NOT
/// significant in this run) into [`NonReplication`] rows, sorted by
/// entity for determinism (JSON object key order is not guaranteed).
/// Returns an empty `Vec` when the file is absent, unparseable, or has no
/// `excluded_nonsig` key.
fn parse_non_replications(result_json: &Path) -> Vec<NonReplication> {
    let Ok(text) = std::fs::read_to_string(result_json) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let Some(obj) = value.get("excluded_nonsig").and_then(|v| v.as_object()) else {
        return Vec::new();
    };

    let mut out: Vec<NonReplication> = obj
        .iter()
        .map(|(entity, row)| NonReplication {
            entity: entity.clone(),
            pmid: row
                .get("pmid")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            prior_claim: row
                .get("prior_direction")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            here_effect: row.get("lfc").and_then(json_as_f64),
            here_significance: row.get("padj").and_then(json_as_f64),
        })
        .collect();
    out.sort_by(|a, b| a.entity.cmp(&b.entity));
    out
}

/// Reads `result.json`'s `cited_pmids` array. Returns an empty `Vec` when
/// the file is absent, unparseable, or has no `cited_pmids` key.
fn read_cited_pmids(result_json: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(result_json) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    value
        .get("cited_pmids")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// A JSON number OR numeric string as `f64` (the contextualize atom's
/// `result.json` mixes both representations across fields).
fn json_as_f64(v: &serde_json::Value) -> Option<f64> {
    v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

/// Joins the reporting significant set against the contextualize stage's
/// literature outputs. Mutates each `EntityRow.literature` in place by
/// matching on `finding_id` OR `entity` from `claims_matrix_csv`
/// (`same_direction`→`Concordant`, `opposite_direction`→`Discordant`,
/// `unverifiable`→`Unverifiable`, `no_prior_finding` or no matrix row at
/// all→`Novel`). Returns the rollup: per-status `LitFinding` lists, the
/// non-replication list built from `contextualize_result_json`'s
/// `excluded_nonsig`, `novel_count`, and the union of every PMID touched
/// (matrix-referenced ∪ non-replication PMIDs ∪ `result.json`'s
/// `cited_pmids`), sorted.
///
/// When `claims_matrix_csv` doesn't exist (or fails to parse), every
/// entity is marked `NotAssessed` and an empty rollup is returned —
/// `result.json` is not consulted in that case, since without a matrix
/// there was no literature contextualization run to report on.
pub fn join_literature(
    sig_entities: &mut [EntityRow],
    claims_matrix_csv: &Path,
    contextualize_result_json: &Path,
) -> LiteratureRollup {
    let empty_rollup = || LiteratureRollup {
        concordant: Vec::new(),
        discordant: Vec::new(),
        unverifiable: Vec::new(),
        non_replications: Vec::new(),
        novel_count: 0,
        retrieved_sources: Vec::new(),
    };

    let Ok(mut reader) = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .from_path(claims_matrix_csv)
    else {
        for e in sig_entities.iter_mut() {
            e.literature = LiteratureStatus::NotAssessed;
        }
        return empty_rollup();
    };
    let Ok(headers) = reader.headers().cloned() else {
        for e in sig_entities.iter_mut() {
            e.literature = LiteratureStatus::NotAssessed;
        }
        return empty_rollup();
    };

    let finding_idx = resolve_column(&headers, "finding_id");
    let entity_idx = resolve_column(&headers, "entity");
    let flag_idx = resolve_column(&headers, "concordance_flag");
    let pmid_idx = resolve_column(&headers, "prior_pmid");
    let lfc_idx = resolve_column(&headers, "lfc");
    let quote_idx = resolve_column(&headers, "evidence_quote");

    let mut by_key: BTreeMap<String, MatrixRow> = BTreeMap::new();
    for rec in reader.records().flatten() {
        let row = MatrixRow {
            concordance_flag: flag_idx.and_then(|i| rec.get(i)).unwrap_or("").to_string(),
            pmid: pmid_idx.and_then(|i| rec.get(i)).unwrap_or("").to_string(),
            effect: lfc_idx
                .and_then(|i| rec.get(i))
                .and_then(|s| s.trim().parse::<f64>().ok()),
            evidence_quote: quote_idx.and_then(|i| rec.get(i)).unwrap_or("").to_string(),
        };
        if let Some(fid) = finding_idx.and_then(|i| rec.get(i)).filter(|s| !s.is_empty()) {
            by_key.insert(fid.to_string(), row.clone());
        }
        if let Some(ent) = entity_idx.and_then(|i| rec.get(i)).filter(|s| !s.is_empty()) {
            by_key.insert(ent.to_string(), row.clone());
        }
    }

    let mut concordant = Vec::new();
    let mut discordant = Vec::new();
    let mut unverifiable = Vec::new();
    let mut novel_count = 0u64;
    let mut sources: BTreeSet<String> = BTreeSet::new();

    for e in sig_entities.iter_mut() {
        let Some(row) = by_key.get(&e.entity) else {
            e.literature = LiteratureStatus::Novel;
            novel_count += 1;
            continue;
        };
        if !row.pmid.is_empty() {
            sources.insert(row.pmid.clone());
        }
        let finding = LitFinding {
            entity: e.entity.clone(),
            pmid: row.pmid.clone(),
            evidence_quote: row.evidence_quote.clone(),
            effect: row.effect,
        };
        match row.concordance_flag.as_str() {
            "same_direction" => {
                e.literature = LiteratureStatus::Concordant { pmid: row.pmid.clone() };
                concordant.push(finding);
            }
            "opposite_direction" => {
                e.literature = LiteratureStatus::Discordant { pmid: row.pmid.clone() };
                discordant.push(finding);
            }
            "unverifiable" => {
                e.literature = LiteratureStatus::Unverifiable { pmid: row.pmid.clone() };
                unverifiable.push(finding);
            }
            // "no_prior_finding" (or any other/unrecognized flag) — treated
            // as Novel, same as "no matrix row at all".
            _ => {
                e.literature = LiteratureStatus::Novel;
                novel_count += 1;
            }
        }
    }

    let non_replications = parse_non_replications(contextualize_result_json);
    for nr in &non_replications {
        if !nr.pmid.is_empty() {
            sources.insert(nr.pmid.clone());
        }
    }
    for pmid in read_cited_pmids(contextualize_result_json) {
        sources.insert(pmid);
    }

    LiteratureRollup {
        concordant,
        discordant,
        unverifiable,
        non_replications,
        novel_count,
        retrieved_sources: sources.into_iter().collect(),
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

    // -- Task 4: write_supplementary + degenerate-output guard --------

    #[test]
    fn write_supplementary_writes_significant_and_full_tables() {
        let tmp = tempfile::tempdir().unwrap();
        let (hdr, rows) = tsv(
            "gene\tlog2FoldChange\tpadj\nGUP\t5\t0.001\nGDOWN\t-4.8\t0.002\nNS\t0.1\t0.9",
        );
        let sig_indices = vec![0usize, 1usize];

        let (sig_rel, full_rel) =
            write_supplementary(tmp.path(), "de_results", &hdr, &rows, &sig_indices).unwrap();

        assert_eq!(sig_rel, "de_results.significant.tsv");
        assert_eq!(full_rel, "de_results.full.tsv");

        let sig_content = std::fs::read_to_string(tmp.path().join(&sig_rel)).unwrap();
        let sig_lines: Vec<&str> = sig_content.lines().collect();
        assert_eq!(sig_lines.len(), 3, "header + 2 significant rows");
        assert_eq!(sig_lines[0], "gene\tlog2FoldChange\tpadj");
        assert!(sig_lines[1..].iter().any(|l| l.starts_with("GUP\t")));
        assert!(sig_lines[1..].iter().any(|l| l.starts_with("GDOWN\t")));
        assert!(!sig_lines[1..].iter().any(|l| l.starts_with("NS\t")));

        let full_content = std::fs::read_to_string(tmp.path().join(&full_rel)).unwrap();
        let full_lines: Vec<&str> = full_content.lines().collect();
        assert_eq!(full_lines.len(), 4, "header + all 3 rows");
        assert!(full_lines[1..].iter().any(|l| l.starts_with("NS\t")));
    }

    #[test]
    fn write_supplementary_writes_zero_row_significant_table_when_none_significant() {
        let tmp = tempfile::tempdir().unwrap();
        let (hdr, rows) = tsv("gene\tlog2FoldChange\tpadj\nNS\t0.1\t0.9");
        let (sig_rel, full_rel) =
            write_supplementary(tmp.path(), "x", &hdr, &rows, &[]).unwrap();
        let sig_content = std::fs::read_to_string(tmp.path().join(&sig_rel)).unwrap();
        assert_eq!(sig_content.lines().count(), 1, "header only");
        let full_content = std::fs::read_to_string(tmp.path().join(&full_rel)).unwrap();
        assert_eq!(full_content.lines().count(), 2, "header + 1 row");
    }

    #[test]
    fn spill_threshold_guard_trips_only_above_threshold() {
        assert!(!should_spill(0));
        assert!(!should_spill(SPILL_THRESHOLD));
        assert!(should_spill(SPILL_THRESHOLD + 1));
    }

    #[test]
    fn write_supplementary_still_writes_full_significant_table_past_spill_threshold() {
        // The degenerate-output guard suppresses the INLINE
        // `significant_entities` list only — the on-disk supplementary
        // table is always written in full, regardless of size.
        let tmp = tempfile::tempdir().unwrap();
        let mut lines = vec!["gene\tpadj".to_string()];
        for i in 0..(SPILL_THRESHOLD + 1) {
            lines.push(format!("G{i}\t0.001"));
        }
        let (hdr, rows) = tsv(&lines.join("\n"));
        let sig_indices: Vec<usize> = (0..rows.len()).collect();
        assert!(should_spill(sig_indices.len()));

        let (sig_rel, _full_rel) =
            write_supplementary(tmp.path(), "big", &hdr, &rows, &sig_indices).unwrap();
        let sig_content = std::fs::read_to_string(tmp.path().join(&sig_rel)).unwrap();
        assert_eq!(sig_content.lines().count(), SPILL_THRESHOLD + 2, "header + every significant row");
    }

    // -- Task 5: literature join ---------------------------------------

    fn matrix_csv(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("claims_evidence_matrix.csv");
        std::fs::write(
            &path,
            "finding_id,entity,entity_kind,prior_pmid,concordance_flag,direction_from_prior,lfc,padj,evidence_quote\n\
             F1,GCON,gene,111,same_direction,up,2.0,0.01,quote1\n\
             F2,GDIS,gene,222,opposite_direction,down,-1.5,0.02,quote2\n\
             F3,GUNV,gene,333,unverifiable,,1.1,0.03,quote3\n\
             F4,GNOPRIOR,gene,,no_prior_finding,,0.9,0.04,\n",
        )
        .unwrap();
        path
    }

    fn contextualize_result_json(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("result.json");
        std::fs::write(
            &path,
            r#"{
                "cited_pmids": ["111", "222", "333"],
                "excluded_nonsig": {
                    "GEXCL": {"ensembl": "F5", "pmid": "444", "prior_direction": "down", "lfc": 0.05, "padj": 0.9}
                }
            }"#,
        )
        .unwrap();
        path
    }

    fn entity_row(entity: &str) -> EntityRow {
        EntityRow {
            entity: entity.to_string(),
            effect: None,
            significance: None,
            literature: LiteratureStatus::NotAssessed,
        }
    }

    #[test]
    fn join_literature_covers_all_statuses_and_non_replication() {
        let tmp = tempfile::tempdir().unwrap();
        let matrix_path = matrix_csv(tmp.path());
        let result_json_path = contextualize_result_json(tmp.path());

        let mut sig_entities = vec![
            entity_row("F1"),
            entity_row("F2"),
            entity_row("F3"),
            entity_row("F4"),
            entity_row("GNOMATCH"),
        ];

        let rollup = super::join_literature(&mut sig_entities, &matrix_path, &result_json_path);

        assert_eq!(
            sig_entities[0].literature,
            LiteratureStatus::Concordant { pmid: "111".to_string() }
        );
        assert_eq!(
            sig_entities[1].literature,
            LiteratureStatus::Discordant { pmid: "222".to_string() }
        );
        assert_eq!(
            sig_entities[2].literature,
            LiteratureStatus::Unverifiable { pmid: "333".to_string() }
        );
        assert_eq!(sig_entities[3].literature, LiteratureStatus::Novel);
        assert_eq!(sig_entities[4].literature, LiteratureStatus::Novel);

        assert_eq!(rollup.concordant.len(), 1);
        assert_eq!(rollup.concordant[0].entity, "F1");
        assert_eq!(rollup.concordant[0].pmid, "111");
        assert_eq!(rollup.concordant[0].evidence_quote, "quote1");
        assert_eq!(rollup.concordant[0].effect, Some(2.0));

        assert_eq!(rollup.discordant.len(), 1);
        assert_eq!(rollup.discordant[0].entity, "F2");

        assert_eq!(rollup.unverifiable.len(), 1);
        assert_eq!(rollup.unverifiable[0].entity, "F3");

        assert_eq!(rollup.novel_count, 2);

        assert_eq!(rollup.non_replications.len(), 1);
        let nr = &rollup.non_replications[0];
        assert_eq!(nr.entity, "GEXCL");
        assert_eq!(nr.pmid, "444");
        assert_eq!(nr.prior_claim, "down");
        assert_eq!(nr.here_effect, Some(0.05));
        assert_eq!(nr.here_significance, Some(0.9));

        assert_eq!(
            rollup.retrieved_sources,
            vec!["111".to_string(), "222".to_string(), "333".to_string(), "444".to_string()]
        );
    }

    #[test]
    fn join_literature_missing_matrix_marks_not_assessed_and_returns_empty_rollup() {
        let mut sig_entities = vec![entity_row("X")];
        let rollup = super::join_literature(
            &mut sig_entities,
            std::path::Path::new("/nonexistent/claims_evidence_matrix.csv"),
            std::path::Path::new("/nonexistent/result.json"),
        );
        assert_eq!(sig_entities[0].literature, LiteratureStatus::NotAssessed);
        assert!(rollup.concordant.is_empty());
        assert!(rollup.discordant.is_empty());
        assert!(rollup.unverifiable.is_empty());
        assert!(rollup.non_replications.is_empty());
        assert_eq!(rollup.novel_count, 0);
        assert!(rollup.retrieved_sources.is_empty());
    }

    #[test]
    fn join_literature_missing_result_json_still_classifies_from_matrix() {
        let tmp = tempfile::tempdir().unwrap();
        let matrix_path = matrix_csv(tmp.path());
        let mut sig_entities = vec![entity_row("F1")];
        let rollup = super::join_literature(
            &mut sig_entities,
            &matrix_path,
            std::path::Path::new("/nonexistent/result.json"),
        );
        assert_eq!(
            sig_entities[0].literature,
            LiteratureStatus::Concordant { pmid: "111".to_string() }
        );
        assert!(rollup.non_replications.is_empty());
        assert_eq!(rollup.retrieved_sources, vec!["111".to_string()]);
    }
}
