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

use ts_rs::TS;

use super::{Comparator, ResultSchema, Significance};

/// Data-driven column-name synonym lists loaded from the emitted package's
/// interpretation policy (`verifiableEntities` block) — the SAME maintained
/// synonym source the claim-verifier uses. Threaded into report-data column
/// resolution so a terminal artifact whose agent emitted a column under a
/// policy-listed synonym (e.g. pathway `term`/`nes`/`adj_p_value` for the
/// declared `pathway`/`NES`/`padj`) still resolves, without per-atom alias
/// maintenance. The lists are DATA (loaded from the policy), never hardcoded
/// column names in the resolution logic.
///
/// Empty (the [`Default`]) → resolution equals declared-name + declared-alias
/// only (no behavior change from today). `#[non_exhaustive]` for SemVer
/// headroom (a later kind — e.g. a grouping synonym list — can be added
/// without a breaking change).
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct PolicyColumnSynonyms {
    pub entity: Vec<String>,
    pub significance: Vec<String>,
    pub effect: Vec<String>,
}

/// Loads the column-name synonym lists from the emitted package's
/// interpretation policy. Reads
/// `package_root/policies/interpretation-policy.json` first, falling back to
/// `package_root/config/downstream-policy/interpretation-policy.json`. Parses
/// `verifiableEntities.{entityColumns → entity, pvalueColumns → significance,
/// effectSizeColumns → effect}`. Best-effort: an absent, unreadable, or
/// unparseable policy yields all-empty lists — resolution then falls back to
/// declared-name-only, with no behavior change from before synonyms existed.
pub fn load_policy_column_synonyms(package_root: &Path) -> PolicyColumnSynonyms {
    let primary = package_root
        .join("policies")
        .join("interpretation-policy.json");
    let path = if primary.exists() {
        primary
    } else {
        package_root
            .join("config")
            .join("downstream-policy")
            .join("interpretation-policy.json")
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return PolicyColumnSynonyms::default();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return PolicyColumnSynonyms::default();
    };
    let ve = value.get("verifiableEntities");
    let read_list = |key: &str| -> Vec<String> {
        ve.and_then(|v| v.get(key))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    PolicyColumnSynonyms {
        entity: read_list("entityColumns"),
        significance: read_list("pvalueColumns"),
        effect: read_list("effectSizeColumns"),
    }
}

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
    /// Per-group significant breakdown — `Some` iff `ResultSchema::grouping_column`
    /// was declared and resolved against the header (e.g. one entry per
    /// pathway `collection`). Sorted by group name for determinism.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grouped_significant: Option<Vec<GroupCount>>,
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

/// The count of significant rows in one group of a `grouping_column`-declared
/// artifact (e.g. one pathway `collection`). Agent-facing JSON only — no
/// ts-rs derive, consistent with the other report_data types.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GroupCount {
    pub group: String,
    pub n_significant: u64,
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
    /// Entities a query WAS issued for and no prior finding was retrieved
    /// (`no_prior_finding` matrix rows). "Novel" applies only to this
    /// searched set.
    pub novel_count: u64,
    /// Entities retrieval was NOT performed for (`not_assessed` rows, plus any
    /// unrecognized/empty flag). Distinct from `novel_count`: absence of
    /// retrieved evidence for an unsearched entity is not a novelty claim.
    /// `#[serde(default)]` so a `report-data.json` written before this field
    /// existed still deserializes (defaults to 0) for read-back / replay.
    #[serde(default)]
    pub not_assessed_count: u64,
    pub retrieved_sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, TS)]
#[ts(export)]
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
    /// Per-group significant counts when the schema declares a
    /// `grouping_column` that resolves against the header; `None` otherwise.
    pub grouped_significant: Option<Vec<GroupCount>>,
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

/// Resolves the signed-effect column index, trying the schema's declared
/// `signed_effect_column` first, then each `signed_effect_aliases` entry, then
/// each policy `synonyms.effect` entry, in order — the first candidate that
/// resolves against the header wins. The declared names come from the atom's
/// schema; the synonym names are DATA loaded from the interpretation policy —
/// this function never hardcodes a column name. `None` when no candidate
/// resolves (there is no silent positional fallback).
fn resolve_effect_column(
    headers: &csv::StringRecord,
    schema: &ResultSchema,
    synonyms: &PolicyColumnSynonyms,
) -> Option<usize> {
    schema
        .signed_effect_column
        .iter()
        .chain(schema.signed_effect_aliases.iter())
        .chain(synonyms.effect.iter())
        .find_map(|name| resolve_column(headers, name))
}

/// Resolves the entity (row-identifier) column index, trying the schema's
/// declared `entity_column` first, then each `entity_column_aliases` entry,
/// then each policy `synonyms.entity` entry, in order — the first candidate
/// that resolves against the header wins. The declared names come from the
/// atom's schema; the synonym names are DATA loaded from the interpretation
/// policy — this function never hardcodes a column name. `None` when no
/// candidate resolves (there is no silent positional fallback).
fn resolve_entity_column(
    headers: &csv::StringRecord,
    schema: &ResultSchema,
    synonyms: &PolicyColumnSynonyms,
) -> Option<usize> {
    std::iter::once(&schema.entity_column)
        .chain(schema.entity_column_aliases.iter())
        .chain(synonyms.entity.iter())
        .find_map(|name| resolve_column(headers, name))
}

/// Resolves the significance column index, trying the schema's declared
/// `Significance::column` first, then each policy `synonyms.significance`
/// entry, in order — the first candidate that resolves against the header
/// wins. The schema's threshold + comparator apply to whichever column
/// resolves. The declared name comes from the atom's schema; the synonym names
/// are DATA loaded from the interpretation policy — this function never
/// hardcodes a column name. `None` when no candidate resolves (unresolvable —
/// must not be conflated with "zero rows pass").
fn resolve_significance_column(
    headers: &csv::StringRecord,
    sig: &Significance,
    synonyms: &PolicyColumnSynonyms,
) -> Option<usize> {
    // The declared name wins outright.
    if let Some(idx) = resolve_column(headers, &sig.column) {
        return Some(idx);
    }
    // Synonym fallback. When the atom declared an ADJUSTED-p column (padj / FDR /
    // q-value — the FDR-controlled significance convention for DE + enrichment),
    // prefer adjusted-p synonyms over raw ones: a header may carry BOTH `p_value`
    // and `adj_p_value`, and the significance THRESHOLD is meant for the adjusted
    // value — resolving the raw column would over-count the significant set.
    if crate::claim_verifier::is_adjusted_pvalue_keyword(&sig.column) {
        let (adjusted, raw): (Vec<&String>, Vec<&String>) = synonyms
            .significance
            .iter()
            .partition(|c| crate::claim_verifier::is_adjusted_pvalue_keyword(c));
        adjusted
            .into_iter()
            .chain(raw)
            .find_map(|name| resolve_column(headers, name))
    } else {
        synonyms
            .significance
            .iter()
            .find_map(|name| resolve_column(headers, name))
    }
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
    synonyms: &PolicyColumnSynonyms,
) -> ArtifactStats {
    let n_total = rows.len() as u64;

    // Resolved by the schema's declared name, then its declared aliases, then
    // the policy synonym list — all DATA-driven (schema declaration + loaded
    // policy); this never hardcodes an alias.
    let effect_idx = resolve_effect_column(headers, schema, synonyms);

    // Distinguishes three states: no significance declared (every row
    // counts, `n_significant` stays `None`); declared and resolved (normal
    // filter); declared but no candidate column present in the header
    // (unresolvable — must NOT be conflated with "zero rows pass").
    let (significant_row_indices, n_significant, significance_unresolvable): (Vec<usize>, Option<u64>, bool) =
        match &schema.significance {
            None => ((0..rows.len()).collect(), None, false),
            Some(sig) => match resolve_significance_column(headers, sig, synonyms) {
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

    // Per-group breakdown of the significant set, when the schema declares a
    // grouping column that resolves against the header. Skipped when
    // significance was declared-but-unresolvable (nothing was assessed, so an
    // empty breakdown would read misleadingly as "no group has any"). The
    // group value comes from the declared column — never a hardcoded name.
    // BTreeMap → sorted Vec keeps the output deterministic.
    let grouped_significant = (!significance_unresolvable)
        .then(|| {
            schema
                .grouping_column
                .as_deref()
                .and_then(|name| resolve_column(headers, name))
                .map(|gidx| {
                    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
                    for &i in &significant_row_indices {
                        if let Some(group) = rows[i].get(gidx) {
                            *counts.entry(group.to_string()).or_insert(0) += 1;
                        }
                    }
                    counts
                        .into_iter()
                        .map(|(group, n_significant)| GroupCount { group, n_significant })
                        .collect::<Vec<_>>()
                })
        })
        .flatten();

    ArtifactStats {
        n_total,
        n_significant,
        direction_split,
        effect_distribution,
        grouped_significant,
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
/// entity/effect/significance columns BY NAME (never positionally) via the
/// schema's declared name, its declared aliases, then the policy synonym list
/// (data-driven — the synonym names are loaded from the interpretation policy,
/// not hardcoded here). Returns an empty `Vec` when no entity-column candidate
/// resolves against `headers` — the caller has no reliable identifier to key
/// each row on. `literature` starts at `NotAssessed`; the caller runs
/// [`join_literature`] to fill it in.
pub(crate) fn build_entity_rows(
    rows: &[csv::StringRecord],
    headers: &csv::StringRecord,
    schema: &ResultSchema,
    synonyms: &PolicyColumnSynonyms,
    indices: &[usize],
) -> Vec<EntityRow> {
    let Some(entity_idx) = resolve_entity_column(headers, schema, synonyms) else {
        return Vec::new();
    };
    let effect_idx = resolve_effect_column(headers, schema, synonyms);
    let significance_idx = schema
        .significance
        .as_ref()
        .and_then(|sig| resolve_significance_column(headers, sig, synonyms));

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

/// One `claims_evidence_matrix.csv` row. Retained in file order to build the
/// authoritative literature rollup, and also indexed by both `finding_id` and
/// `entity` for per-`EntityRow` tagging (see [`join_literature`]).
#[derive(Debug, Clone)]
struct MatrixRow {
    entity: String,
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
/// literature outputs.
///
/// The returned rollup is built from the `claims_matrix_csv` ROWS directly —
/// the matrix already carries the authoritative per-finding
/// `concordance_flag`, so the counts are independent of which report-data
/// entities happen to have populated. Every matrix row is bucketed by its
/// flag (`same_direction`→concordant, `opposite_direction`→discordant,
/// `unverifiable`→unverifiable, `no_prior_finding`→`novel_count`,
/// `not_assessed`→`not_assessed_count`). A `no_prior_finding` row means a
/// query WAS issued for the entity and nothing was retrieved — only that
/// searched set is "novel". Any UNRECOGNIZED or empty flag routes to
/// `not_assessed_count` (never novel): an entity retrieval was not performed
/// for is not a novelty claim. `non_replications` come from
/// `contextualize_result_json`'s
/// `excluded_nonsig`; `retrieved_sources` is the union of every matrix PMID
/// (the `prior_pmid`/`pmid` column, whichever resolves) ∪ non-replication PMIDs
/// ∪ `result.json`'s `cited_pmids`, sorted — so it is non-empty from the matrix
/// alone even when `result.json` has no `cited_pmids`.
///
/// SEPARATELY, each `EntityRow.literature` is tagged in place by matching the
/// entity (on `finding_id` OR `entity`) to a matrix row — the same flag→status
/// mapping, but this per-entity tagging NEVER drives the rollup counts. An
/// entity that matches no matrix row stays `NotAssessed` (it is NOT counted as
/// novel — novel comes only from the matrix's `no_prior_finding` rows), so
/// entities from a different modality (e.g. pathways against a gene-keyed
/// matrix) cannot pollute the rollup.
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
        not_assessed_count: 0,
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
    // The contextualize atom's PMID column name varies across runs
    // (`prior_pmid` / `pmid`); try the known variants in order. `retrieved_sources`
    // then fills from whichever resolves, even when `result.json` has no
    // `cited_pmids`.
    let pmid_idx = ["prior_pmid", "pmid"]
        .iter()
        .find_map(|name| resolve_column(&headers, name));
    // The contextualize atom's effect column name also varies across runs
    // (`lfc` / `log2FoldChange` / `logFC` / `log2fc` / `nes` / `analysis_log2fc`);
    // try the known variants in order. Local to this matrix reader — the atom's
    // own output is fixed-ish, so a small candidate list tolerates its known
    // column-name drift.
    let effect_idx = ["lfc", "log2FoldChange", "logFC", "log2fc", "nes", "analysis_log2fc"]
        .iter()
        .find_map(|name| resolve_column(&headers, name));
    let quote_idx = resolve_column(&headers, "evidence_quote");

    // File-order list of every matrix row (drives the authoritative rollup)
    // plus a finding_id/entity → row index for per-EntityRow tagging.
    let mut matrix_rows: Vec<MatrixRow> = Vec::new();
    let mut by_key: BTreeMap<String, MatrixRow> = BTreeMap::new();
    for rec in reader.records().flatten() {
        let row = MatrixRow {
            entity: entity_idx.and_then(|i| rec.get(i)).unwrap_or("").to_string(),
            concordance_flag: flag_idx.and_then(|i| rec.get(i)).unwrap_or("").to_string(),
            pmid: pmid_idx.and_then(|i| rec.get(i)).unwrap_or("").to_string(),
            effect: effect_idx
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
        matrix_rows.push(row);
    }

    // Rollup built from the matrix rows themselves — authoritative and
    // independent of which report-data entities populated.
    let mut concordant = Vec::new();
    let mut discordant = Vec::new();
    let mut unverifiable = Vec::new();
    let mut novel_count = 0u64;
    let mut not_assessed_count = 0u64;
    let mut sources: BTreeSet<String> = BTreeSet::new();

    for row in &matrix_rows {
        if !row.pmid.is_empty() {
            sources.insert(row.pmid.clone());
        }
        let finding = || LitFinding {
            entity: row.entity.clone(),
            pmid: row.pmid.clone(),
            evidence_quote: row.evidence_quote.clone(),
            effect: row.effect,
        };
        match row.concordance_flag.as_str() {
            "same_direction" => concordant.push(finding()),
            "opposite_direction" => discordant.push(finding()),
            "unverifiable" => unverifiable.push(finding()),
            // A query WAS issued for this entity and nothing was retrieved —
            // the only case that counts as novel.
            "no_prior_finding" => novel_count += 1,
            // Retrieval was not performed for this entity. Any unrecognized or
            // empty flag routes here too — an unsearched entity is never novel.
            "not_assessed" => not_assessed_count += 1,
            _ => not_assessed_count += 1,
        }
    }

    // Deterministic emission order (matrix file order is not a stable
    // contract): sort each list by entity then pmid.
    let sort_findings = |v: &mut Vec<LitFinding>| {
        v.sort_by(|a, b| a.entity.cmp(&b.entity).then_with(|| a.pmid.cmp(&b.pmid)));
    };
    sort_findings(&mut concordant);
    sort_findings(&mut discordant);
    sort_findings(&mut unverifiable);

    // Per-entity tagging — does NOT feed the rollup counts. An entity that
    // matches no matrix row stays NotAssessed (never counted as novel).
    for e in sig_entities.iter_mut() {
        e.literature = match by_key.get(&e.entity) {
            Some(row) => match row.concordance_flag.as_str() {
                "same_direction" => LiteratureStatus::Concordant { pmid: row.pmid.clone() },
                "opposite_direction" => LiteratureStatus::Discordant { pmid: row.pmid.clone() },
                "unverifiable" => LiteratureStatus::Unverifiable { pmid: row.pmid.clone() },
                // Searched, nothing retrieved → the searched-set novelty tag.
                "no_prior_finding" => LiteratureStatus::Novel,
                // Retrieval not performed (or an unrecognized/empty flag) →
                // never a novelty claim.
                _ => LiteratureStatus::NotAssessed,
            },
            None => LiteratureStatus::NotAssessed,
        };
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
        not_assessed_count,
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
            entity_column_aliases: Vec::new(),
            significance: Some(Significance {
                column: "padj".into(),
                threshold: 0.05,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: Some("log2FoldChange".into()),
            signed_effect_aliases: Vec::new(),
            grouping_column: None,
        }
    }

    #[test]
    fn signed_table_counts_significant_and_direction_split() {
        let (hdr, rows) = tsv(
            "gene\tlog2FoldChange\tpadj\nGUP\t5\t0.001\nGDOWN\t-4.8\t0.002\nNS\t0.1\t0.9",
        );
        let schema = de_schema();
        let s = summarize_artifact(&rows, &hdr, &schema, &PolicyColumnSynonyms::default());
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
            entity_column_aliases: Vec::new(),
            significance: Some(Significance {
                column: "qual".into(),
                threshold: 30.0,
                comparator: Comparator::Gt,
            }),
            signed_effect_column: None,
            signed_effect_aliases: Vec::new(),
            grouping_column: None,
        };
        let s = summarize_artifact(&rows, &hdr, &schema, &PolicyColumnSynonyms::default());
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
            entity_column_aliases: Vec::new(),
            significance: None,
            signed_effect_column: Some("log2FoldChange".into()),
            signed_effect_aliases: Vec::new(),
            grouping_column: None,
        };
        let s = summarize_artifact(&rows, &hdr, &schema, &PolicyColumnSynonyms::default());
        assert_eq!(s.n_total, 3);
        assert_eq!(s.n_significant, None);
        assert_eq!(s.significant_row_indices, vec![0, 1, 2]);
        // Direction split/distribution still computed over "all rows" since
        // an effect column resolved, even though nothing was filtered.
        assert_eq!(s.direction_split, Some(DirectionSplit { up: 2, down: 1 }));
    }

    #[test]
    fn alias_resolves_signed_effect_column_when_declared_name_absent() {
        // The table emits the ECAA-canonical `log2FC` header; the schema
        // declares the DESeq2-native `log2FoldChange` as the primary name
        // plus `log2FC` as an accepted alias. Resolution falls through to the
        // alias — data-driven, from the schema, no hardcoded alias list here.
        // This is the inverse of the negative test below.
        let (hdr, rows) =
            tsv("gene\tlog2FC\tpadj\nGUP\t5\t0.001\nGDOWN\t-4.8\t0.002\nNS\t0.1\t0.9");
        let mut schema = de_schema();
        schema.signed_effect_aliases = vec!["log2FC".into()];
        let s = summarize_artifact(&rows, &hdr, &schema, &PolicyColumnSynonyms::default());
        assert_eq!(s.n_significant, Some(2));
        assert_eq!(s.direction_split, Some(DirectionSplit { up: 1, down: 1 }));
        let dist = s.effect_distribution.expect("aliased effect column resolved");
        assert_eq!(dist.iter().map(|b| b.count).sum::<u64>(), 2);
    }

    #[test]
    fn declared_effect_column_absent_yields_no_direction_split() {
        // Schema-only resolution: the schema declares `log2FoldChange` plus
        // the alias `log2FC`, but this table's effect column is named neither
        // (`log2ratio`), so NO declared candidate resolves → no direction
        // split / distribution. Proves resolution never falls back beyond the
        // declared candidates (no silent fallback). Significance still
        // computed off the resolvable `padj` column.
        let (hdr, rows) = tsv("gene\tlog2ratio\tpadj\nA\t3\t0.01\n");
        let mut schema = de_schema();
        schema.signed_effect_aliases = vec!["log2FC".into()];
        let s = summarize_artifact(&rows, &hdr, &schema, &PolicyColumnSynonyms::default());
        assert_eq!(s.n_significant, Some(1));
        assert_eq!(s.direction_split, None);
        assert_eq!(s.effect_distribution, None);
    }

    #[test]
    fn grouping_column_yields_per_group_significant_counts() {
        // Pathway-shaped: `collection` groups the rows. padj<0.05 filters to
        // the significant set (2 HALLMARK + 1 GO_BP), then per-collection
        // counts are tallied in sorted (BTreeMap) order. KEGG's only row is
        // non-significant, so it never appears in the breakdown.
        let (hdr, rows) = tsv(
            "pathway\tcollection\tpadj\n\
             P1\tHALLMARK\t0.01\n\
             P2\tHALLMARK\t0.02\n\
             P3\tGO_BP\t0.001\n\
             P4\tGO_BP\t0.9\n\
             P5\tKEGG\t0.9",
        );
        let schema = ResultSchema {
            artifact: "pathway_results.tsv".into(),
            entity_column: "pathway".into(),
            entity_column_aliases: Vec::new(),
            significance: Some(Significance {
                column: "padj".into(),
                threshold: 0.05,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: None,
            signed_effect_aliases: Vec::new(),
            grouping_column: Some("collection".into()),
        };
        let s = summarize_artifact(&rows, &hdr, &schema, &PolicyColumnSynonyms::default());
        assert_eq!(s.n_significant, Some(3));
        let grouped = s.grouped_significant.expect("grouping column resolved");
        assert_eq!(
            grouped,
            vec![
                GroupCount { group: "GO_BP".into(), n_significant: 1 },
                GroupCount { group: "HALLMARK".into(), n_significant: 2 },
            ]
        );
    }

    #[test]
    fn no_grouping_column_yields_none_grouped_significant() {
        let (hdr, rows) = tsv("gene\tlog2FoldChange\tpadj\nA\t1\t0.01\nB\t-2\t0.02");
        // de_schema declares no grouping_column.
        let s = summarize_artifact(&rows, &hdr, &de_schema(), &PolicyColumnSynonyms::default());
        assert_eq!(s.grouped_significant, None);
    }

    #[test]
    fn unresolved_grouping_column_yields_none_grouped_significant() {
        // grouping_column declared but absent from the header → None (not an
        // empty breakdown).
        let (hdr, rows) = tsv("gene\tlog2FoldChange\tpadj\nA\t1\t0.01");
        let mut schema = de_schema();
        schema.grouping_column = Some("collection".into());
        let s = summarize_artifact(&rows, &hdr, &schema, &PolicyColumnSynonyms::default());
        assert_eq!(s.grouped_significant, None);
    }

    #[test]
    fn declared_significance_column_absent_is_unresolvable_not_zero() {
        // The schema declares significance on `padj`, but this table has no
        // `padj` column. `n_significant` must be None (unresolvable), NOT
        // Some(0) which would falsely read as "zero significant rows found".
        let (hdr, rows) = tsv("gene\tlog2FoldChange\nA\t3\nB\t-1");
        let schema = de_schema();
        let s = summarize_artifact(&rows, &hdr, &schema, &PolicyColumnSynonyms::default());
        assert_eq!(s.n_significant, None);
        assert!(s.significant_row_indices.is_empty());
        assert_eq!(s.direction_split, None);
    }

    // -- policy-synonym tolerant column resolution ---------------------

    /// The real interpretation policy shipped under `config/downstream-policy/`,
    /// loaded via the fallback path from the repo root (no `policies/` dir at
    /// the repo root, so `load_policy_column_synonyms` falls back to
    /// `config/downstream-policy/interpretation-policy.json`).
    fn real_policy_synonyms() -> PolicyColumnSynonyms {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        super::load_policy_column_synonyms(&repo_root)
    }

    #[test]
    fn policy_synonyms_resolve_pathway_shaped_table() {
        // A pathway-shaped fgsea table whose headers match the pathway atom's
        // result_schema on NONE of its declared names — entity `pathway` vs
        // actual `term`, significance `padj` vs actual `p_value`/`adj_p_value`,
        // effect `NES` vs actual lowercase `nes` — resolves once the real
        // interpretation-policy synonym lists are threaded (term∈entityColumns,
        // p_value/adj_p_value∈pvalueColumns, nes∈effectSizeColumns). Grouping
        // `collection` is declared AND present (unchanged path).
        let synonyms = real_policy_synonyms();
        assert!(
            !synonyms.entity.is_empty() && !synonyms.significance.is_empty(),
            "the real config/downstream-policy/interpretation-policy.json loaded"
        );

        // The last row is raw-significant (p_value 0.03 < 0.05) but adjusted-
        // NON-significant (adj_p_value 0.20 >= 0.05): it must NOT count, proving
        // the significance filter resolves the ADJUSTED column, not raw `p_value`
        // (both are present, and raw precedes adjusted in the policy list).
        let (hdr, rows) = tsv(
            "term\tcollection\tp_value\tadj_p_value\tlog2err\tes\tnes\tn_overlap\tn_leading_edge\n\
             HALLMARK_HYPOXIA\tHALLMARK\t0.001\t0.01\t0.1\t0.5\t2.1\t50\t10\n\
             GO_MAPK_CASCADE\tGO_BP\t0.002\t0.02\t0.1\t0.4\t1.8\t30\t8\n\
             KEGG_METABOLISM\tKEGG\t0.5\t0.9\t0.1\t0.1\t0.3\t10\t2\n\
             RAW_ONLY_SIG\tGO_BP\t0.03\t0.20\t0.1\t0.2\t1.1\t12\t3",
        );
        let schema = ResultSchema {
            artifact: "pathway_results.tsv".into(),
            entity_column: "pathway".into(),
            entity_column_aliases: Vec::new(),
            significance: Some(Significance {
                column: "padj".into(),
                threshold: 0.05,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: Some("NES".into()),
            signed_effect_aliases: Vec::new(),
            grouping_column: Some("collection".into()),
        };

        let s = summarize_artifact(&rows, &hdr, &schema, &synonyms);
        // significance resolves via the ADJUSTED p-value synonym (adj_p_value):
        // 2 of 4 rows pass. If it wrongly resolved raw `p_value`, the RAW_ONLY_SIG
        // row (p=0.03) would push this to 3.
        assert_eq!(
            s.n_significant,
            Some(2),
            "n_significant must resolve via the ADJUSTED p-value column, not raw p_value"
        );
        assert!(
            s.effect_distribution.is_some(),
            "effect_distribution present via the `nes` effect synonym"
        );
        assert!(s.direction_split.is_some());
        assert!(
            s.grouped_significant.is_some(),
            "grouped_significant present via the declared `collection` grouping"
        );

        // entity resolves via `term` (an entityColumns synonym) → rows populated.
        let entities =
            build_entity_rows(&rows, &hdr, &schema, &synonyms, &s.significant_row_indices);
        assert_eq!(entities.len(), 2, "significant_entities populated via the `term` synonym");
        assert!(entities.iter().any(|e| e.entity == "HALLMARK_HYPOXIA"));
        assert!(
            entities.iter().all(|e| e.effect.is_some()),
            "each entity's effect populated via the `nes` synonym"
        );
    }

    #[test]
    fn empty_synonyms_with_mismatched_header_yields_none_significant() {
        // Same pathway-shaped header, but EMPTY synonyms and a schema whose
        // declared significance column (`padj`) is absent → n_significant None
        // (unchanged three-state: unresolvable, never Some(0)). Proves synonyms
        // are the ONLY thing that made the mismatched header resolve above.
        let (hdr, rows) = tsv(
            "term\tcollection\tp_value\tadj_p_value\tnes\n\
             HALLMARK_HYPOXIA\tHALLMARK\t0.001\t0.01\t2.1",
        );
        let schema = ResultSchema {
            artifact: "pathway_results.tsv".into(),
            entity_column: "pathway".into(),
            entity_column_aliases: Vec::new(),
            significance: Some(Significance {
                column: "padj".into(),
                threshold: 0.05,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: Some("NES".into()),
            signed_effect_aliases: Vec::new(),
            grouping_column: Some("collection".into()),
        };
        let s = summarize_artifact(&rows, &hdr, &schema, &PolicyColumnSynonyms::default());
        assert_eq!(s.n_significant, None);
    }

    // -- build_entity_rows entity-column alias resolution --------------

    #[test]
    fn entity_column_alias_resolves_when_declared_header_absent() {
        // The table emits `gene_id` (Ensembl-style) as the row identifier;
        // the schema declares canonical `entity_column: gene` plus `gene_id`
        // as an accepted alias. Resolution falls through to the alias —
        // data-driven, from the schema, no hardcoded synonym list here.
        let (hdr, rows) =
            tsv("gene_id\tlog2FoldChange\tpadj\nENSG1\t5\t0.001\nENSG2\t-4.8\t0.002");
        let mut schema = de_schema();
        schema.entity_column_aliases = vec!["gene_id".into()];
        let entities =
            build_entity_rows(&rows, &hdr, &schema, &PolicyColumnSynonyms::default(), &[0, 1]);
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].entity, "ENSG1");
        assert_eq!(entities[1].entity, "ENSG2");
    }

    #[test]
    fn entity_column_mismatch_without_alias_yields_empty() {
        // Same `gene_id` header, but the schema declares only
        // `entity_column: gene` with NO alias → no declared candidate
        // resolves → empty (no silent fallback beyond the declared names).
        let (hdr, rows) = tsv("gene_id\tlog2FoldChange\tpadj\nENSG1\t5\t0.001");
        let schema = de_schema(); // entity_column "gene", no aliases
        let entities =
            build_entity_rows(&rows, &hdr, &schema, &PolicyColumnSynonyms::default(), &[0]);
        assert!(entities.is_empty());
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
        // F4 matches the no_prior_finding matrix row → Novel per-entity tag.
        assert_eq!(sig_entities[3].literature, LiteratureStatus::Novel);
        // GNOMATCH matches no matrix row → NotAssessed (never counted as
        // novel; novel comes only from the matrix's no_prior_finding rows).
        assert_eq!(sig_entities[4].literature, LiteratureStatus::NotAssessed);

        // Rollup is built from the matrix ROWS (entity column = GCON/GDIS/...),
        // not from the sig_entities that matched.
        assert_eq!(rollup.concordant.len(), 1);
        assert_eq!(rollup.concordant[0].entity, "GCON");
        assert_eq!(rollup.concordant[0].pmid, "111");
        assert_eq!(rollup.concordant[0].evidence_quote, "quote1");
        assert_eq!(rollup.concordant[0].effect, Some(2.0));

        assert_eq!(rollup.discordant.len(), 1);
        assert_eq!(rollup.discordant[0].entity, "GDIS");

        assert_eq!(rollup.unverifiable.len(), 1);
        assert_eq!(rollup.unverifiable[0].entity, "GUNV");

        // novel_count = the single no_prior_finding matrix row (F4/GNOPRIOR);
        // GNOMATCH's non-match does NOT contribute.
        assert_eq!(rollup.novel_count, 1);

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
        // retrieved_sources is the union of ALL matrix prior_pmids (rollup is
        // matrix-driven), not just the PMID of the one matched entity.
        assert_eq!(
            rollup.retrieved_sources,
            vec!["111".to_string(), "222".to_string(), "333".to_string()]
        );
    }

    #[test]
    fn join_literature_rollup_from_matrix_independent_of_entities() {
        // A matrix with 2 same_direction + 1 opposite_direction +
        // 3 no_prior_finding + 1 unverifiable. The rollup counts must come
        // from these matrix rows REGARDLESS of which sig_entities are passed.
        let tmp = tempfile::tempdir().unwrap();
        let matrix_path = tmp.path().join("claims_evidence_matrix.csv");
        std::fs::write(
            &matrix_path,
            "finding_id,entity,prior_pmid,concordance_flag,lfc,evidence_quote\n\
             F1,GA,10,same_direction,2.0,qa\n\
             F2,GB,11,same_direction,1.5,qb\n\
             F3,GC,12,opposite_direction,-1.0,qc\n\
             F4,GD,,no_prior_finding,0.3,\n\
             F5,GE,,no_prior_finding,0.4,\n\
             F6,GF,,no_prior_finding,0.5,\n\
             F7,GG,13,unverifiable,0.9,qg\n",
        )
        .unwrap();
        let missing_json = std::path::Path::new("/nonexistent/result.json");

        let expected = |rollup: &LiteratureRollup| {
            assert_eq!(rollup.concordant.len(), 2, "2 same_direction rows");
            assert_eq!(rollup.discordant.len(), 1, "1 opposite_direction row");
            assert_eq!(rollup.unverifiable.len(), 1, "1 unverifiable row");
            assert_eq!(rollup.novel_count, 3, "3 no_prior_finding rows");
        };

        // (a) Empty entity slice — rollup identical.
        let mut empty: Vec<EntityRow> = Vec::new();
        let r_empty = super::join_literature(&mut empty, &matrix_path, missing_json);
        expected(&r_empty);

        // (b) A pathway-only slice that matches nothing in the gene-keyed
        // matrix — rollup MUST be identical (pathways cannot pollute it), and
        // every non-matching entity stays NotAssessed (never novel).
        let mut pathways = vec![
            entity_row("HALLMARK_HYPOXIA"),
            entity_row("KEGG_MAPK"),
        ];
        let r_path = super::join_literature(&mut pathways, &matrix_path, missing_json);
        expected(&r_path);
        assert_eq!(r_empty, r_path, "rollup independent of the entities passed");
        assert!(pathways
            .iter()
            .all(|e| e.literature == LiteratureStatus::NotAssessed));

        // Per-entity tagging still applies to a matching gene entity.
        let mut genes = vec![entity_row("GA"), entity_row("GC")];
        let r_genes = super::join_literature(&mut genes, &matrix_path, missing_json);
        expected(&r_genes);
        assert_eq!(
            genes[0].literature,
            LiteratureStatus::Concordant { pmid: "10".to_string() }
        );
        assert_eq!(
            genes[1].literature,
            LiteratureStatus::Discordant { pmid: "12".to_string() }
        );
    }

    #[test]
    fn join_literature_resolves_log2foldchange_effect_column() {
        // The matrix effect column is named `log2FoldChange` (not `lfc`);
        // LitFinding.effect must still populate via the candidate set.
        let tmp = tempfile::tempdir().unwrap();
        let matrix_path = tmp.path().join("claims_evidence_matrix.csv");
        std::fs::write(
            &matrix_path,
            "finding_id,entity,prior_pmid,concordance_flag,log2FoldChange,evidence_quote\n\
             F1,GCON,111,same_direction,2.61,quote1\n",
        )
        .unwrap();
        let mut entities = vec![entity_row("GCON")];
        let rollup = super::join_literature(
            &mut entities,
            &matrix_path,
            std::path::Path::new("/nonexistent/result.json"),
        );
        assert_eq!(rollup.concordant.len(), 1);
        assert_eq!(rollup.concordant[0].effect, Some(2.61));
    }

    #[test]
    fn join_literature_resolves_pmid_and_analysis_log2fc_columns() {
        // This run's matrix names the PMID column `pmid` (not `prior_pmid`) and
        // the effect column `analysis_log2fc` (not `lfc`). Both resolve via the
        // broadened candidate lists → LitFinding.pmid + effect populate, and
        // `retrieved_sources` fills from the matrix `pmid` even with no
        // result.json `cited_pmids`.
        let tmp = tempfile::tempdir().unwrap();
        let matrix_path = tmp.path().join("claims_evidence_matrix.csv");
        std::fs::write(
            &matrix_path,
            "finding_id,entity,entity_kind,pmid,prior_direction,analysis_log2fc,analysis_padj,concordance_flag,evidence_quote\n\
             F1,CRISPLD2,gene,999,up,2.61,0.001,same_direction,prior quote\n",
        )
        .unwrap();
        let mut entities = vec![entity_row("CRISPLD2")];
        let rollup = super::join_literature(
            &mut entities,
            &matrix_path,
            std::path::Path::new("/nonexistent/result.json"),
        );
        assert_eq!(rollup.concordant.len(), 1);
        assert_eq!(rollup.concordant[0].pmid, "999");
        assert_eq!(rollup.concordant[0].effect, Some(2.61));
        assert_eq!(
            rollup.retrieved_sources,
            vec!["999".to_string()],
            "retrieved_sources fills from the matrix `pmid` column with no result.json"
        );
        assert_eq!(
            entities[0].literature,
            LiteratureStatus::Concordant { pmid: "999".to_string() }
        );
    }

    #[test]
    fn join_literature_separates_novel_from_not_assessed() {
        // `no_prior_finding` (a query WAS issued, nothing retrieved) counts as
        // novel; `not_assessed` (retrieval not performed) and any
        // unrecognized/empty flag count as not_assessed — NEVER novel. This is
        // the searched-vs-unsearched distinction the report must preserve.
        let tmp = tempfile::tempdir().unwrap();
        let matrix_path = tmp.path().join("claims_evidence_matrix.csv");
        std::fs::write(
            &matrix_path,
            "finding_id,entity,prior_pmid,concordance_flag,lfc,evidence_quote\n\
             F1,GNOVEL1,,no_prior_finding,0.3,\n\
             F2,GNOVEL2,,no_prior_finding,0.4,\n\
             F3,GNA1,,not_assessed,0.5,\n\
             F4,GNA2,,not_assessed,0.6,\n\
             F5,GNA3,,not_assessed,0.7,\n\
             F6,GUNKNOWN,,some_future_flag,0.8,\n\
             F7,GEMPTY,,,0.9,\n",
        )
        .unwrap();
        let missing_json = std::path::Path::new("/nonexistent/result.json");

        let mut entities = vec![
            entity_row("GNOVEL1"),
            entity_row("GNA1"),
            entity_row("GUNKNOWN"),
            entity_row("GEMPTY"),
        ];
        let rollup = super::join_literature(&mut entities, &matrix_path, missing_json);

        // Only the two searched-yet-empty rows are novel.
        assert_eq!(rollup.novel_count, 2, "2 no_prior_finding rows");
        // The three not_assessed rows + the unrecognized flag + the empty flag
        // all land in not_assessed — none of them are counted as novel.
        assert_eq!(
            rollup.not_assessed_count, 5,
            "3 not_assessed + 1 unrecognized + 1 empty flag"
        );
        assert!(rollup.concordant.is_empty());
        assert!(rollup.discordant.is_empty());
        assert!(rollup.unverifiable.is_empty());

        // Per-entity: no_prior_finding → Novel; not_assessed / unrecognized /
        // empty → NotAssessed (never Novel).
        assert_eq!(entities[0].literature, LiteratureStatus::Novel);
        assert_eq!(entities[1].literature, LiteratureStatus::NotAssessed);
        assert_eq!(entities[2].literature, LiteratureStatus::NotAssessed);
        assert_eq!(entities[3].literature, LiteratureStatus::NotAssessed);
    }
}
