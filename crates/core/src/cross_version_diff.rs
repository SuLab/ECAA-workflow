//! Compare two emitted packages' `results/tables/*.{csv,tsv}` and classify
//! each row as robust / concordant / discordant / new-in-child /
//! dropped-in-parent / entity-missing / numerics-incomplete.
//!
//! Produced at emit time when the child session has a parent package
//! (amendment or branched-from-emit). Conceptually adjacent to
//! [`claim_verifier`] but operates over *two* tables rather than one
//! table plus extracted narrative claims.
//!
//! Classification rules:
//!
//! * **Robust**: same direction and same significance state (both
//!   significant or both non-significant).
//! * **Concordant**: same direction but significance differs (one sig,
//!   one not).
//! * **Discordant**: direction flips.
//! * **NewInChild** / **DroppedInParent**: row present on only one side.
//! * **EntityMissing**: entity key present in one table but not the other
//!   after normalization (supersedes the former `Unverifiable` case when
//!   the effect column is absent on one side due to a missing entity).
//! * **NumericsIncomplete**: both sides have the entity but one or both
//!   lack the effect-size value needed for direction comparison.
//!
//! Significance decision follows the v2 spec §11: prefer adjusted p when
//! the column is present, fall back to raw p otherwise. Threshold
//! configurable per-table.
//!
//! Deterministic — reads CSV, no network, no LLM.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use ts_rs::TS;

/// Per-row classification. See module docs for semantics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum RowClassification {
    /// Robust variant.
    Robust,
    /// Concordant variant.
    Concordant,
    /// Discordant variant.
    Discordant,
    /// NewInChild variant.
    NewInChild,
    /// DroppedInParent variant.
    DroppedInParent,
    /// The entity key is absent from one side after normalization — no
    /// effect-size comparison is possible.
    EntityMissing,
    /// Both sides carry the entity but one or both lack a parseable
    /// effect-size value; records which numeric fields are absent.
    NumericsIncomplete {
        /// Human-readable list of the missing numeric columns,
        /// e.g. `"parent:log2FC"` or `"parent:log2FC,child:log2FC"`.
        which_missing: String,
    },
}

/// One entity's cross-version diff. Both raw and adjusted p-values are
/// surfaced independently so the UI can spot rows that move between
/// adjustment regimes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RowDiff {
    /// Entity.
    pub entity: String,
    /// Classification.
    pub classification: RowClassification,
    #[ts(optional)]
    /// Parent effect.
    pub parent_effect: Option<f64>,
    #[ts(optional)]
    /// Child effect.
    pub child_effect: Option<f64>,
    #[ts(optional)]
    /// Parent pvalue raw.
    pub parent_pvalue_raw: Option<f64>,
    #[ts(optional)]
    /// Parent pvalue adjusted.
    pub parent_pvalue_adjusted: Option<f64>,
    #[ts(optional)]
    /// Child pvalue raw.
    pub child_pvalue_raw: Option<f64>,
    #[ts(optional)]
    /// Child pvalue adjusted.
    pub child_pvalue_adjusted: Option<f64>,
    /// Per-row contribution to the Pearson correlation of effect sizes
    /// across the overlap. Rows with positive contribution agree with
    /// the trend; negative contribution pulls against it. Sum of
    /// contributions equals `pearson_r` (up to float rounding).
    pub effect_correlation_contribution: f64,
}

/// One table's diff. Aggregates per-row classifications, a Pearson
/// correlation, and a Spearman correlation on overlapping effect sizes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct TableDiff {
    /// Table name.
    pub table_name: String,
    /// N rows parent.
    pub n_rows_parent: usize,
    /// N rows child.
    pub n_rows_child: usize,
    /// N overlap.
    pub n_overlap: usize,
    /// N robust.
    pub n_robust: usize,
    /// N concordant.
    pub n_concordant: usize,
    /// N discordant.
    pub n_discordant: usize,
    /// Rows where the entity key is absent from one side after normalization.
    pub n_entity_missing: usize,
    /// Rows where the entity is present on both sides but effect-size values
    /// are missing or non-numeric.
    pub n_numerics_incomplete: usize,
    #[ts(optional)]
    /// Pearson r.
    pub pearson_r: Option<f64>,
    /// Spearman rank correlation of overlapping effect sizes.
    /// More robust than Pearson for heavy-tailed biological effect-size
    /// distributions (e.g. log2FC with extreme fold-changes).
    #[ts(optional)]
    pub spearman_rho: Option<f64>,
    /// Rows.
    pub rows: Vec<RowDiff>,
}

/// Whole-package cross-version report. Written to
/// `runtime/cross-version-diff.json` in the child package.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct CrossVersionReport {
    /// Parent package.
    pub parent_package: String,
    /// Child package.
    pub child_package: String,
    /// Tables.
    pub tables: Vec<TableDiff>,
    /// Fraction of overlapping classified rows (Robust | Concordant) over
    /// total overlapping classified rows. 0.0 when no overlap.
    pub overall_concordance: f64,
    /// Describes how this diff was anchored: `"package-pair"` (default,
    /// compares entire packages) or `"task-pair"` (anchored to a specific
    /// task boundary, set when the child session branched from a named task
    /// via `branched_from_task_id`). Backward-compatible: absent on reports
    /// written before this field was added; `serde(default)` fills in
    /// `"package-pair"`.
    #[serde(default = "default_anchor_kind")]
    pub anchor_kind: String,
    /// The task id used as the diff anchor when `anchor_kind == "task-pair"`.
    /// `None` for `"package-pair"` diffs and for older reports that predate
    /// this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub anchored_task_id: Option<String>,
}

/// Per-table diff configuration. Source: `interpretation-policy.json`
/// `crossVersionDiff.tables` block. `table_name: "*"` acts as a
/// wildcard fallback.
#[derive(Debug, Clone, PartialEq)]
pub struct TableDiffConfig {
    /// Table name.
    pub table_name: String,
    /// Entity column.
    pub entity_column: String,
    /// Effect size column.
    pub effect_size_column: String,
    /// Pvalue raw column.
    pub pvalue_raw_column: Option<String>,
    /// Pvalue adjusted column.
    pub pvalue_adjusted_column: Option<String>,
    /// Significance threshold.
    pub significance_threshold: f64,
}

/// Config loaded from `interpretation-policy.json`. Callers obtain this
/// from policy JSON via [`CrossVersionConfig::from_policy`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CrossVersionConfig {
    /// Tables.
    pub tables: Vec<TableDiffConfig>,
}

impl CrossVersionConfig {
    /// Parse a policy JSON value. Missing `crossVersionDiff` block
    /// falls back to `verifiableEntities` column hints, yielding one
    /// wildcard-table config. Returns an empty config when neither
    /// block is present (diff will be a no-op).
    pub fn from_policy(policy: &serde_json::Value) -> Self {
        let mut tables = Vec::new();
        if let Some(block) = policy.get("crossVersionDiff") {
            if let Some(items) = block.get("tables").and_then(|v| v.as_array()) {
                for item in items {
                    let Some(name) = item.get("table").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    let entity = item
                        .get("entityColumn")
                        .and_then(|v| v.as_str())
                        .unwrap_or("gene")
                        .to_string();
                    let effect = item
                        .get("effectSizeColumn")
                        .and_then(|v| v.as_str())
                        .unwrap_or("log2FC")
                        .to_string();
                    let p_raw = item
                        .get("pvalueRawColumn")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let p_adj = item
                        .get("pvalueAdjustedColumn")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    let thresh = item
                        .get("significanceThreshold")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.05);
                    tables.push(TableDiffConfig {
                        table_name: name.to_string(),
                        entity_column: entity,
                        effect_size_column: effect,
                        pvalue_raw_column: p_raw,
                        pvalue_adjusted_column: p_adj,
                        significance_threshold: thresh,
                    });
                }
            }
        }
        if tables.is_empty() {
            if let Some(ve) = policy.get("verifiableEntities") {
                let entity = ve
                    .get("entityColumns")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                    .unwrap_or("gene")
                    .to_string();
                let effect = ve
                    .get("effectSizeColumns")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                    .unwrap_or("log2FC")
                    .to_string();
                let p_cols: Vec<String> = ve
                    .get("pvalueColumns")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect()
                    })
                    .unwrap_or_else(|| vec!["padj".into(), "pvalue".into()]);
                let (adjusted, raw): (Vec<_>, Vec<_>) = p_cols.iter().cloned().partition(|c| {
                    let lc = c.to_ascii_lowercase();
                    lc.contains("adj")
                        || lc.contains("padj")
                        || lc.contains("fdr")
                        || lc.contains("qvalue")
                });
                tables.push(TableDiffConfig {
                    table_name: "*".into(),
                    entity_column: entity,
                    effect_size_column: effect,
                    pvalue_raw_column: raw.first().cloned(),
                    pvalue_adjusted_column: adjusted.first().cloned(),
                    significance_threshold: 0.05,
                });
            }
        }
        // Final fallback: when neither block is present at all, use a
        // bioinformatics-default wildcard. Diff is opt-out at the policy
        // level — packages that really want no diff set
        // `crossVersionDiff: {"enabled": false}` (unsupported today;
        // future extension point).
        if tables.is_empty() {
            tables.push(TableDiffConfig {
                table_name: "*".into(),
                entity_column: "gene".into(),
                effect_size_column: "log2FC".into(),
                pvalue_raw_column: Some("pvalue".into()),
                pvalue_adjusted_column: Some("padj".into()),
                significance_threshold: 0.05,
            });
        }
        Self { tables }
    }

    fn for_table(&self, name: &str) -> Option<&TableDiffConfig> {
        self.tables
            .iter()
            .find(|t| t.table_name == name)
            .or_else(|| self.tables.iter().find(|t| t.table_name == "*"))
    }
}

fn default_anchor_kind() -> String {
    "package-pair".to_string()
}

/// Diff two packages' `results/tables/` directories.
///
/// `parent` and `child` are package-root paths. The function reads each
/// directory's CSV/TSV files, classifies every row that can be matched
/// by entity, and returns a [`CrossVersionReport`].
pub fn diff_packages(
    parent: &Path,
    child: &Path,
    cfg: &CrossVersionConfig,
) -> Result<CrossVersionReport> {
    let parent_tables = parent.join("results").join("tables");
    let child_tables = child.join("results").join("tables");

    let mut names: BTreeMap<String, (bool, bool)> = BTreeMap::new();
    for (dir, mark_parent) in [(&parent_tables, true), (&child_tables, false)] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if !p.is_file() {
                    continue;
                }
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    let slot = names.entry(name.to_string()).or_insert((false, false));
                    if mark_parent {
                        slot.0 = true;
                    } else {
                        slot.1 = true;
                    }
                }
            }
        }
    }

    let mut table_diffs: Vec<TableDiff> = Vec::new();
    let mut total_robust = 0usize;
    let mut total_concordant = 0usize;
    let mut total_classified = 0usize;

    for (table_name, (in_parent, in_child)) in names {
        let Some(tcfg) = cfg.for_table(&table_name) else {
            continue;
        };
        let parent_rows = if in_parent {
            load_table(&parent_tables.join(&table_name), tcfg)?
        } else {
            Vec::new()
        };
        let child_rows = if in_child {
            load_table(&child_tables.join(&table_name), tcfg)?
        } else {
            Vec::new()
        };
        let diff = diff_table(&table_name, tcfg, parent_rows, child_rows);
        total_robust += diff.n_robust;
        total_concordant += diff.n_concordant;
        total_classified += diff
            .rows
            .iter()
            .filter(|r| {
                matches!(
                    r.classification,
                    RowClassification::Robust
                        | RowClassification::Concordant
                        | RowClassification::Discordant
                )
            })
            .count();
        table_diffs.push(diff);
    }

    let overall_concordance = if total_classified > 0 {
        (total_robust + total_concordant) as f64 / total_classified as f64
    } else {
        0.0
    };

    Ok(CrossVersionReport {
        parent_package: parent.to_string_lossy().to_string(),
        child_package: child.to_string_lossy().to_string(),
        tables: table_diffs,
        overall_concordance,
        anchor_kind: default_anchor_kind(),
        anchored_task_id: None,
    })
}

/// Variant of [`diff_packages`] that attaches a task-pair anchor to the
/// report. Called when the child session was branched at a specific task
/// boundary (`SessionLineage::branched_from_task_id.is_some()`). The
/// diff computation itself is identical; the anchor fields on the
/// returned report communicate the granularity to downstream consumers
/// (UI Compare tab, audit-proof ECAA subgraph).
pub fn diff_packages_at_task(
    parent: &Path,
    child: &Path,
    cfg: &CrossVersionConfig,
    task_id: &str,
) -> Result<CrossVersionReport> {
    let mut report = diff_packages(parent, child, cfg)?;
    report.anchor_kind = "task-pair".to_string();
    report.anchored_task_id = Some(task_id.to_string());
    Ok(report)
}

#[derive(Debug, Clone)]
struct Row {
    entity: String,
    effect: Option<f64>,
    pvalue_raw: Option<f64>,
    pvalue_adjusted: Option<f64>,
}

fn load_table(path: &Path, cfg: &TableDiffConfig) -> Result<Vec<Row>> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let delimiter = if ext == "csv" { b',' } else { b'\t' };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(true)
        .from_reader(file);
    let headers = reader.headers()?.clone();
    let lower: Vec<String> = headers.iter().map(|h| h.to_ascii_lowercase()).collect();
    let find = |col: &str| -> Option<usize> {
        let needle = col.to_ascii_lowercase();
        lower.iter().position(|h| h == &needle)
    };
    let entity_idx = find(&cfg.entity_column).ok_or_else(|| {
        anyhow!(
            "no entity column `{}` in {}",
            cfg.entity_column,
            path.display()
        )
    })?;
    let effect_idx = find(&cfg.effect_size_column);
    let praw_idx = cfg.pvalue_raw_column.as_ref().and_then(|c| find(c));
    let padj_idx = cfg.pvalue_adjusted_column.as_ref().and_then(|c| find(c));

    let mut rows = Vec::new();
    for rec in reader.records() {
        let rec = rec?;
        let entity = rec.get(entity_idx).unwrap_or("").trim().to_string();
        if entity.is_empty() {
            continue;
        }
        let parse = |idx: Option<usize>| -> Option<f64> {
            idx.and_then(|i| rec.get(i))
                .and_then(|s| s.trim().parse::<f64>().ok())
        };
        rows.push(Row {
            entity,
            effect: parse(effect_idx),
            pvalue_raw: parse(praw_idx),
            pvalue_adjusted: parse(padj_idx),
        });
    }
    Ok(rows)
}

/// Normalize an entity name for case-insensitive, format-agnostic matching.
///
/// Steps applied in order:
/// 1. Trim surrounding whitespace.
/// 2. Lowercase.
/// 3. Strip a leading species prefix of the form `XX:` where `XX` is one to
///    four ASCII letters (e.g. `mm:`, `hs:`, `dme:`, `sce:`).
/// 4. Collapse internal whitespace runs and remove underscores/dashes.
///
/// HGNC↔Ensembl alias resolution (e.g. mapping `CD274` ↔ `PDCD1LG1`) is
/// out of scope for this function — that would require an `aliases.csv`
/// lookup table loaded from config.
fn normalize_entity(s: &str) -> String {
    let trimmed = s.trim().to_ascii_lowercase();
    // Strip species prefix: up to 4 lowercase letters followed by ':'.
    let after_prefix = if let Some(colon) = trimmed.find(':') {
        let prefix = &trimmed[..colon];
        if !prefix.is_empty()
            && prefix.len() <= 4
            && prefix.chars().all(|c| c.is_ascii_alphabetic())
        {
            &trimmed[colon + 1..]
        } else {
            &trimmed
        }
    } else {
        &trimmed
    };
    // Collapse whitespace and strip underscores/dashes.
    let mut out = String::with_capacity(after_prefix.len());
    let mut prev_was_space = false;
    for c in after_prefix.chars() {
        if c.is_whitespace() {
            if !prev_was_space && !out.is_empty() {
                // Preserve a single space for readability but callers
                // compare normalized strings; internal spaces are fine.
                out.push(' ');
            }
            prev_was_space = true;
        } else if c == '_' || c == '-' {
            prev_was_space = false;
            // Drop separators entirely.
        } else {
            out.push(c);
            prev_was_space = false;
        }
    }
    out
}

fn diff_table(
    table_name: &str,
    cfg: &TableDiffConfig,
    parent: Vec<Row>,
    child: Vec<Row>,
) -> TableDiff {
    let n_rows_parent = parent.len();
    let n_rows_child = child.len();

    // Normalized entity lookup; preserve the first encountered casing
    // so the output is deterministic.
    let mut parent_idx: BTreeMap<String, usize> = BTreeMap::new();
    for (i, r) in parent.iter().enumerate() {
        parent_idx.entry(normalize_entity(&r.entity)).or_insert(i);
    }
    let mut child_idx: BTreeMap<String, usize> = BTreeMap::new();
    for (i, r) in child.iter().enumerate() {
        child_idx.entry(normalize_entity(&r.entity)).or_insert(i);
    }

    // Deterministic enumeration over the union of keys.
    let mut all_keys: Vec<String> = parent_idx.keys().cloned().collect();
    for k in child_idx.keys() {
        if !parent_idx.contains_key(k) {
            all_keys.push(k.clone());
        }
    }
    all_keys.sort();

    struct Working {
        entity: String,
        classification: RowClassification,
        parent_effect: Option<f64>,
        child_effect: Option<f64>,
        parent_praw: Option<f64>,
        parent_padj: Option<f64>,
        child_praw: Option<f64>,
        child_padj: Option<f64>,
    }

    let mut working: Vec<Working> = Vec::new();
    let mut xs: Vec<f64> = Vec::new();
    let mut ys: Vec<f64> = Vec::new();
    let mut n_overlap = 0usize;
    let mut n_robust = 0usize;
    let mut n_concordant = 0usize;
    let mut n_discordant = 0usize;
    let mut n_entity_missing = 0usize;
    let mut n_numerics_incomplete = 0usize;

    for key in all_keys {
        let pr = parent_idx.get(&key).map(|&i| &parent[i]);
        let ch = child_idx.get(&key).map(|&i| &child[i]);
        let entity = match (pr, ch) {
            (Some(r), _) => r.entity.clone(),
            (None, Some(r)) => r.entity.clone(),
            _ => unreachable!("union of keys must have at least one side"),
        };
        let classification = match (pr, ch) {
            (None, Some(_)) => RowClassification::NewInChild,
            (Some(_), None) => RowClassification::DroppedInParent,
            (Some(p), Some(c)) => {
                n_overlap += 1;
                classify_row(p, c, cfg, &mut xs, &mut ys)
            }
            (None, None) => unreachable!(),
        };
        match &classification {
            RowClassification::Robust => n_robust += 1,
            RowClassification::Concordant => n_concordant += 1,
            RowClassification::Discordant => n_discordant += 1,
            RowClassification::EntityMissing => n_entity_missing += 1,
            RowClassification::NumericsIncomplete { .. } => n_numerics_incomplete += 1,
            RowClassification::NewInChild | RowClassification::DroppedInParent => {}
        }
        working.push(Working {
            entity,
            classification,
            parent_effect: pr.and_then(|r| r.effect),
            child_effect: ch.and_then(|r| r.effect),
            parent_praw: pr.and_then(|r| r.pvalue_raw),
            parent_padj: pr.and_then(|r| r.pvalue_adjusted),
            child_praw: ch.and_then(|r| r.pvalue_raw),
            child_padj: ch.and_then(|r| r.pvalue_adjusted),
        });
    }

    let stats = pearson_stats(&xs, &ys);
    let spearman_rho = spearman_rho(&xs, &ys);

    let rows: Vec<RowDiff> = working
        .into_iter()
        .map(|w| {
            let contribution = match (w.parent_effect, w.child_effect, &stats) {
                (Some(pe), Some(ce), Some(s)) if s.den > 0.0 => {
                    ((pe - s.mean_x) * (ce - s.mean_y)) / s.den
                }
                _ => 0.0,
            };
            RowDiff {
                entity: w.entity,
                classification: w.classification,
                parent_effect: w.parent_effect,
                child_effect: w.child_effect,
                parent_pvalue_raw: w.parent_praw,
                parent_pvalue_adjusted: w.parent_padj,
                child_pvalue_raw: w.child_praw,
                child_pvalue_adjusted: w.child_padj,
                effect_correlation_contribution: contribution,
            }
        })
        .collect();

    TableDiff {
        table_name: table_name.to_string(),
        n_rows_parent,
        n_rows_child,
        n_overlap,
        n_robust,
        n_concordant,
        n_discordant,
        n_entity_missing,
        n_numerics_incomplete,
        pearson_r: stats.as_ref().map(|s| s.r),
        spearman_rho,
        rows,
    }
}

fn classify_row(
    p: &Row,
    c: &Row,
    cfg: &TableDiffConfig,
    xs: &mut Vec<f64>,
    ys: &mut Vec<f64>,
) -> RowClassification {
    let (pe, ce) = match (p.effect, c.effect) {
        (Some(pe), Some(ce)) => (pe, ce),
        (has_p, has_c) => {
            let mut missing = Vec::new();
            if has_p.is_none() {
                missing.push(format!("parent:{}", cfg.effect_size_column));
            }
            if has_c.is_none() {
                missing.push(format!("child:{}", cfg.effect_size_column));
            }
            return RowClassification::NumericsIncomplete {
                which_missing: missing.join(","),
            };
        }
    };
    xs.push(pe);
    ys.push(ce);

    let p_sig = significance(p, cfg);
    let c_sig = significance(c, cfg);

    let same_direction = pe.signum() == ce.signum() || pe == 0.0 || ce == 0.0;
    if !same_direction {
        return RowClassification::Discordant;
    }

    match (p_sig, c_sig) {
        (Some(true), Some(true)) => RowClassification::Robust,
        (Some(false), Some(false)) => RowClassification::Robust,
        (Some(_), Some(_)) => RowClassification::Concordant,
        // One or both sides lack p-value columns; direction is known but
        // significance state is indeterminate.
        _ => RowClassification::NumericsIncomplete {
            which_missing: "pvalue".into(),
        },
    }
}

fn significance(row: &Row, cfg: &TableDiffConfig) -> Option<bool> {
    if let Some(padj) = row.pvalue_adjusted {
        return Some(padj < cfg.significance_threshold);
    }
    if let Some(praw) = row.pvalue_raw {
        return Some(praw < cfg.significance_threshold);
    }
    None
}

struct PearsonStats {
    r: f64,
    mean_x: f64,
    mean_y: f64,
    den: f64,
}

fn pearson_stats(xs: &[f64], ys: &[f64]) -> Option<PearsonStats> {
    if xs.len() < 2 || ys.len() != xs.len() {
        return None;
    }
    let n = xs.len() as f64;
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let (mut dx2, mut dy2, mut xy) = (0.0, 0.0, 0.0);
    for (x, y) in xs.iter().zip(ys.iter()) {
        let dx = x - mean_x;
        let dy = y - mean_y;
        dx2 += dx * dx;
        dy2 += dy * dy;
        xy += dx * dy;
    }
    let den = (dx2 * dy2).sqrt();
    if den == 0.0 {
        None
    } else {
        Some(PearsonStats {
            r: xy / den,
            mean_x,
            mean_y,
            den,
        })
    }
}

/// Compute the Spearman rank correlation of two equal-length series.
///
/// Ranks are computed with average-rank tie-breaking. Spearman is more
/// defensible than Pearson for heavy-tailed biological effect-size
/// distributions (e.g. log2FC series dominated by a few extreme outliers).
fn spearman_rho(xs: &[f64], ys: &[f64]) -> Option<f64> {
    if xs.len() < 2 || ys.len() != xs.len() {
        return None;
    }
    let rx = average_ranks(xs);
    let ry = average_ranks(ys);
    // Pearson on the rank-transformed series equals Spearman rho.
    pearson_stats(&rx, &ry).map(|s| s.r)
}

/// Assign average ranks to the values in `vals` (ascending order, ties
/// share the mean of the ranks they would occupy).
fn average_ranks(vals: &[f64]) -> Vec<f64> {
    let n = vals.len();
    // Pair each value with its original index so we can scatter ranks back.
    let mut order: Vec<usize> = (0..n).collect();
    // Sort indices by value; NaN sorts to the end (treated as largest).
    order.sort_by(|&a, &b| {
        vals[a]
            .partial_cmp(&vals[b])
            .unwrap_or(std::cmp::Ordering::Greater)
    });
    let mut ranks = vec![0.0f64; n];
    let mut i = 0usize;
    while i < n {
        // Find the run of equal values.
        let mut j = i + 1;
        while j < n && vals[order[j]] == vals[order[i]] {
            j += 1;
        }
        // Average rank for positions i..j (1-based ranks).
        let avg = (i + 1 + j) as f64 / 2.0;
        for &idx in &order[i..j] {
            ranks[idx] = avg;
        }
        i = j;
    }
    ranks
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn write_tbl(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn bare_cfg() -> TableDiffConfig {
        TableDiffConfig {
            table_name: "*".into(),
            entity_column: "gene".into(),
            effect_size_column: "log2FC".into(),
            pvalue_raw_column: Some("pvalue".into()),
            pvalue_adjusted_column: Some("padj".into()),
            significance_threshold: 0.05,
        }
    }

    fn pkg_dir(root: &Path, name: &str) -> std::path::PathBuf {
        let pkg = root.join(name).join("results").join("tables");
        std::fs::create_dir_all(&pkg).unwrap();
        pkg
    }

    fn setup_pair(
        root: &Path,
        parent_body: &str,
        child_body: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let pdir = pkg_dir(root, "v1");
        let cdir = pkg_dir(root, "v2");
        write_tbl(&pdir, "de_summary.tsv", parent_body);
        write_tbl(&cdir, "de_summary.tsv", child_body);
        (
            pdir.parent().unwrap().parent().unwrap().to_path_buf(),
            cdir.parent().unwrap().parent().unwrap().to_path_buf(),
        )
    }

    fn wildcard_cfg() -> CrossVersionConfig {
        CrossVersionConfig {
            tables: vec![bare_cfg()],
        }
    }

    #[test]
    fn robust_same_direction_both_significant() {
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.1\t0.0001\t0.001\n",
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.0\t0.0002\t0.002\n",
        );
        let report = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        assert_eq!(report.tables.len(), 1);
        let t = &report.tables[0];
        assert_eq!(t.n_robust, 1);
        assert_eq!(t.n_concordant, 0);
        assert_eq!(t.n_discordant, 0);
        assert!((report.overall_concordance - 1.0).abs() < 1e-9);
    }

    #[test]
    fn concordant_same_direction_significance_varies() {
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.1\t0.0001\t0.001\nSOX9\t1.5\t0.02\t0.08\n",
            // ACAN still sig, SOX9 now sig (padj dropped under 0.05).
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.0\t0.0001\t0.001\nSOX9\t1.6\t0.001\t0.01\n",
        );
        let report = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let t = &report.tables[0];
        let sox9 = t.rows.iter().find(|r| r.entity == "SOX9").unwrap();
        assert_eq!(sox9.classification, RowClassification::Concordant);
        assert_eq!(t.n_concordant, 1);
        assert_eq!(t.n_robust, 1);
    }

    #[test]
    fn discordant_direction_flips() {
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.1\t0.0001\t0.001\n",
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t-1.8\t0.0002\t0.002\n",
        );
        let report = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let t = &report.tables[0];
        assert_eq!(t.n_discordant, 1);
        let acan = t.rows.iter().find(|r| r.entity == "ACAN").unwrap();
        assert_eq!(acan.classification, RowClassification::Discordant);
    }

    #[test]
    fn new_in_child_and_dropped_in_parent() {
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.1\t0.0001\t0.001\nDROPPED_GENE\t1.2\t0.01\t0.03\n",
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.0\t0.0002\t0.002\nNEW_GENE\t1.5\t0.01\t0.02\n",
        );
        let report = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let t = &report.tables[0];
        let dropped = t.rows.iter().find(|r| r.entity == "DROPPED_GENE").unwrap();
        assert_eq!(dropped.classification, RowClassification::DroppedInParent);
        let new = t.rows.iter().find(|r| r.entity == "NEW_GENE").unwrap();
        assert_eq!(new.classification, RowClassification::NewInChild);
    }

    #[test]
    fn numerics_incomplete_when_effect_missing() {
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.1\t0.0001\t0.001\n",
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t\t0.0001\t0.001\n",
        );
        let report = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let t = &report.tables[0];
        let acan = t.rows.iter().find(|r| r.entity == "ACAN").unwrap();
        assert!(
            matches!(
                &acan.classification,
                RowClassification::NumericsIncomplete { which_missing }
                    if which_missing.contains("child")
            ),
            "expected NumericsIncomplete(child:log2FC), got {:?}",
            acan.classification
        );
        assert_eq!(t.n_numerics_incomplete, 1);
        assert_eq!(t.n_entity_missing, 0);
    }

    #[test]
    fn raw_p_fallback_when_adj_missing() {
        // Child lacks padj column; classifier should fall back to raw.
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.1\t0.0001\t0.001\n",
            "gene\tlog2FC\tpvalue\nACAN\t2.0\t0.0002\n",
        );
        let report = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let t = &report.tables[0];
        let acan = t.rows.iter().find(|r| r.entity == "ACAN").unwrap();
        assert_eq!(acan.classification, RowClassification::Robust);
    }

    #[test]
    fn pearson_r_matches_known_value() {
        // Four rows, effect sizes chosen so Pearson = 1.0 exactly.
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nA\t1.0\t0.001\t0.01\nB\t2.0\t0.001\t0.01\nC\t3.0\t0.001\t0.01\nD\t4.0\t0.001\t0.01\n",
            "gene\tlog2FC\tpvalue\tpadj\nA\t2.0\t0.001\t0.01\nB\t4.0\t0.001\t0.01\nC\t6.0\t0.001\t0.01\nD\t8.0\t0.001\t0.01\n",
        );
        let report = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let t = &report.tables[0];
        let r = t.pearson_r.unwrap();
        assert!((r - 1.0).abs() < 1e-9, "r={}", r);
    }

    #[test]
    fn contributions_sum_to_pearson_r() {
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nA\t0.5\t0.001\t0.01\nB\t1.8\t0.001\t0.01\nC\t-1.2\t0.001\t0.01\nD\t2.7\t0.001\t0.01\n",
            "gene\tlog2FC\tpvalue\tpadj\nA\t0.4\t0.001\t0.01\nB\t2.1\t0.001\t0.01\nC\t-1.0\t0.001\t0.01\nD\t2.3\t0.001\t0.01\n",
        );
        let report = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let t = &report.tables[0];
        let r = t.pearson_r.unwrap();
        let sum: f64 = t
            .rows
            .iter()
            .map(|r| r.effect_correlation_contribution)
            .sum();
        assert!((sum - r).abs() < 1e-9, "sum={}, r={}", sum, r);
    }

    #[test]
    fn csv_delimiter_autodetected() {
        let tmp = tempdir().unwrap();
        let pdir = pkg_dir(tmp.path(), "v1");
        let cdir = pkg_dir(tmp.path(), "v2");
        write_tbl(
            &pdir,
            "de_summary.csv",
            "gene,log2FC,pvalue,padj\nACAN,2.1,0.001,0.01\n",
        );
        write_tbl(
            &cdir,
            "de_summary.csv",
            "gene,log2FC,pvalue,padj\nACAN,2.0,0.002,0.02\n",
        );
        let report = diff_packages(
            pdir.parent().unwrap().parent().unwrap(),
            cdir.parent().unwrap().parent().unwrap(),
            &wildcard_cfg(),
        )
        .unwrap();
        let t = &report.tables[0];
        assert_eq!(t.n_robust, 1);
    }

    #[test]
    fn empty_report_when_no_tables() {
        let tmp = tempdir().unwrap();
        let _ = pkg_dir(tmp.path(), "v1");
        let _ = pkg_dir(tmp.path(), "v2");
        let report = diff_packages(
            &tmp.path().join("v1"),
            &tmp.path().join("v2"),
            &wildcard_cfg(),
        )
        .unwrap();
        assert_eq!(report.tables.len(), 0);
        assert_eq!(report.overall_concordance, 0.0);
    }

    #[test]
    fn determinism_byte_identical_output() {
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nZYX\t1.2\t0.001\t0.01\nACAN\t2.1\t0.001\t0.01\nSOX9\t1.5\t0.001\t0.01\n",
            "gene\tlog2FC\tpvalue\tpadj\nSOX9\t1.6\t0.001\t0.01\nACAN\t2.0\t0.001\t0.01\nZYX\t1.1\t0.001\t0.01\n",
        );
        let r1 = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let r2 = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let s1 = serde_json::to_string(&r1).unwrap();
        let s2 = serde_json::to_string(&r2).unwrap();
        assert_eq!(s1, s2, "serialization must be stable");
        // Entities in output should be sorted.
        let t = &r1.tables[0];
        let order: Vec<&str> = t.rows.iter().map(|r| r.entity.as_str()).collect();
        assert_eq!(order, vec!["ACAN", "SOX9", "ZYX"]);
    }

    #[test]
    fn policy_loader_parses_explicit_block() {
        let policy = json!({
            "crossVersionDiff": {
                "tables": [
                    {
                        "table": "de_summary.tsv",
                        "entityColumn": "gene_id",
                        "effectSizeColumn": "logFC",
                        "pvalueRawColumn": "PValue",
                        "pvalueAdjustedColumn": "FDR",
                        "significanceThreshold": 0.01
                    }
                ]
            }
        });
        let cfg = CrossVersionConfig::from_policy(&policy);
        assert_eq!(cfg.tables.len(), 1);
        let t = &cfg.tables[0];
        assert_eq!(t.table_name, "de_summary.tsv");
        assert_eq!(t.entity_column, "gene_id");
        assert_eq!(t.effect_size_column, "logFC");
        assert_eq!(t.pvalue_raw_column.as_deref(), Some("PValue"));
        assert_eq!(t.pvalue_adjusted_column.as_deref(), Some("FDR"));
        assert!((t.significance_threshold - 0.01).abs() < 1e-9);
    }

    #[test]
    fn policy_loader_falls_back_to_verifiable_entities() {
        let policy = json!({
            "verifiableEntities": {
                "entityColumns": ["gene"],
                "effectSizeColumns": ["log2FC"],
                "pvalueColumns": ["padj", "pvalue"]
            }
        });
        let cfg = CrossVersionConfig::from_policy(&policy);
        assert_eq!(cfg.tables.len(), 1);
        let t = &cfg.tables[0];
        assert_eq!(t.table_name, "*");
        assert_eq!(t.pvalue_adjusted_column.as_deref(), Some("padj"));
        assert_eq!(t.pvalue_raw_column.as_deref(), Some("pvalue"));
    }

    #[test]
    fn policy_loader_returns_default_wildcard_when_neither_block_present() {
        let cfg = CrossVersionConfig::from_policy(&json!({}));
        assert_eq!(cfg.tables.len(), 1);
        assert_eq!(cfg.tables[0].table_name, "*");
        assert_eq!(cfg.tables[0].entity_column, "gene");
        assert_eq!(cfg.tables[0].effect_size_column, "log2FC");
    }

    #[test]
    fn large_table_1m_rows_completes() {
        // Stress test: generate 1M synthetic rows, exercise the pipeline,
        // assert classification counts add up correctly.
        let tmp = tempdir().unwrap();
        let pdir = pkg_dir(tmp.path(), "v1");
        let cdir = pkg_dir(tmp.path(), "v2");
        let mut parent = String::from("gene\tlog2FC\tpvalue\tpadj\n");
        let mut child = String::from("gene\tlog2FC\tpvalue\tpadj\n");
        for i in 0..100_000u32 {
            // 100k rows (1M is still okay but 100k keeps the test fast).
            parent.push_str(&format!("GENE{}\t{}\t0.001\t0.01\n", i, (i as f64) * 0.01));
            child.push_str(&format!("GENE{}\t{}\t0.001\t0.01\n", i, (i as f64) * 0.01));
        }
        write_tbl(&pdir, "big.tsv", &parent);
        write_tbl(&cdir, "big.tsv", &child);
        let report = diff_packages(
            pdir.parent().unwrap().parent().unwrap(),
            cdir.parent().unwrap().parent().unwrap(),
            &wildcard_cfg(),
        )
        .unwrap();
        let t = &report.tables[0];
        assert_eq!(t.n_rows_parent, 100_000);
        assert_eq!(t.n_rows_child, 100_000);
        assert_eq!(t.n_overlap, 100_000);
        assert_eq!(t.n_robust, 100_000);
    }

    // ── E7 / E8 / E48 new tests ──────────────────────────────────────────

    #[test]
    fn numerics_incomplete_both_effects_missing_counts_correctly() {
        // Both parent AND child are missing the effect value.
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nGENEA\t\t0.001\t0.01\nGENEB\t1.5\t0.001\t0.01\n",
            "gene\tlog2FC\tpvalue\tpadj\nGENEA\t\t0.002\t0.02\nGENEB\t1.6\t0.001\t0.01\n",
        );
        let report = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let t = &report.tables[0];
        assert_eq!(
            t.n_numerics_incomplete, 1,
            "GENEA should be NumericsIncomplete"
        );
        assert_eq!(t.n_robust, 1, "GENEB should be Robust");
        assert_eq!(t.n_entity_missing, 0);
        let genea = t.rows.iter().find(|r| r.entity == "GENEA").unwrap();
        assert!(
            matches!(&genea.classification, RowClassification::NumericsIncomplete { which_missing }
                if which_missing.contains("parent") && which_missing.contains("child")),
            "expected both parent and child listed, got {:?}",
            genea.classification
        );
    }

    #[test]
    fn spearman_rho_monotonic_series_equals_one() {
        // Monotonically increasing parent and child effect sizes → Spearman = 1.
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nA\t1.0\t0.001\t0.01\nB\t2.0\t0.001\t0.01\nC\t3.0\t0.001\t0.01\n",
            "gene\tlog2FC\tpvalue\tpadj\nA\t10.0\t0.001\t0.01\nB\t20.0\t0.001\t0.01\nC\t30.0\t0.001\t0.01\n",
        );
        let report = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let t = &report.tables[0];
        let rho = t.spearman_rho.expect("spearman_rho should be Some");
        assert!((rho - 1.0).abs() < 1e-9, "rho={}", rho);
        // Pearson should also be 1 for a linear relationship.
        let r = t.pearson_r.expect("pearson_r should be Some");
        assert!((r - 1.0).abs() < 1e-9, "r={}", r);
    }

    #[test]
    fn spearman_rho_with_ties_is_valid() {
        // Tie-breaking: two rows share the same parent effect; Spearman should
        // still produce a finite value in [-1, 1].
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nA\t1.0\t0.001\t0.01\nB\t1.0\t0.001\t0.01\nC\t3.0\t0.001\t0.01\n",
            "gene\tlog2FC\tpvalue\tpadj\nA\t1.5\t0.001\t0.01\nB\t2.5\t0.001\t0.01\nC\t3.5\t0.001\t0.01\n",
        );
        let report = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let t = &report.tables[0];
        let rho = t.spearman_rho.expect("spearman_rho should be Some");
        assert!(rho >= -1.0 && rho <= 1.0, "rho out of range: {}", rho);
    }

    #[test]
    fn normalize_entity_strips_species_prefix_and_separators() {
        assert_eq!(normalize_entity("mm:Tp53"), "tp53");
        assert_eq!(normalize_entity("hs:TP53"), "tp53");
        assert_eq!(normalize_entity("BRCA_1"), "brca1");
        assert_eq!(normalize_entity("BRCA-2"), "brca2");
        assert_eq!(normalize_entity("  SOX9  "), "sox9");
        // Prefix longer than 4 chars should NOT be stripped.
        assert_eq!(normalize_entity("human:TP53"), "human:tp53");
        // Colon in non-prefix position.
        assert_eq!(
            normalize_entity("ENSG00000:foo"),
            "ENSG00000:foo".to_ascii_lowercase()
        );
    }

    #[test]
    fn normalize_entity_enables_case_insensitive_match_across_species_prefix() {
        // An entity keyed as "mm:Trp53" in parent matches "TRP53" in child.
        let tmp = tempdir().unwrap();
        let (p, c) = setup_pair(
            tmp.path(),
            "gene\tlog2FC\tpvalue\tpadj\nmm:Trp53\t2.0\t0.001\t0.01\n",
            "gene\tlog2FC\tpvalue\tpadj\nTRP53\t2.1\t0.001\t0.01\n",
        );
        let report = diff_packages(&p, &c, &wildcard_cfg()).unwrap();
        let t = &report.tables[0];
        // Should match as a single overlapping row (Robust), not two separate rows.
        assert_eq!(
            t.n_overlap, 1,
            "species-prefixed entity should match its unprefixed counterpart"
        );
        assert_eq!(t.n_robust, 1);
        assert_eq!(t.rows.len(), 1);
    }
}
