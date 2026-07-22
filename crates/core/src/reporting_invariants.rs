//! Source-owned reporting-correctness validator (RP-1..RP-9; spec
//! `package-integrity-rca-and-spec` §C / §G-C1).
//!
//! # Why this exists
//!
//! The per-run reporting/validation layers a deposit ships are
//! **existence + transcription** checks: they confirm that report files
//! exist and that the report faithfully copies the upstream stage JSON —
//! and therefore pass even when the upstream JSON is itself wrong (RP-8).
//! The deposited `611cf5ee` package shipped a report that stated
//! `10,085 gene sets tested` (the number *loaded*) when only `5,056` were
//! actually tested after the size filter, described a below-background
//! effect-abundance ratio as "above", and captioned a single-column
//! log2FC heatmap as an eight-sample expression matrix — all of which
//! passed the run's own agent-authored validators.
//!
//! This module is the durable, **source-owned** guarantee: it RECOMPUTES
//! the values from the package's own runtime outputs (e.g. it counts the
//! rows of `pathway_results.tsv` itself rather than trusting the
//! `gene_sets_tested` number recorded in `pathway_summary.json`), so it
//! cannot be defeated by an agent-authored per-run script that records the
//! wrong number. It never reads or trusts any `runtime/outputs/**`
//! validator script — those are per-run artifacts, not source.
//!
//! # Bounded checklist, not a correctness proof
//!
//! This is a **bounded checklist** over the enumerated invariants
//! (RP-1/RP-2/RP-3/RP-4/RP-5/RP-9). It is emphatically **not** a general
//! scientific-correctness proof: a report can pass every check here and
//! still contain scientific errors that fall outside these specific
//! invariants. Absence of a finding means "none of the enumerated
//! invariants were violated", never "the report is correct".
//!
//! # Severity split (§G-C1, normative)
//!
//! * **Required** (blocks deposit) — structural/numeric invariants that
//!   recompute against a stage output:
//!   * **RP-2** reported `gene_sets_tested` (per collection AND total) must
//!     equal the post-filter *tested* rowcount recomputed from
//!     `pathway_results.tsv`, never the loaded count.
//!   * **RP-4** every mapped/unmapped/resolved/unresolved gene count the
//!     report asserts must trace to a stage output (reject narrative-only
//!     back-computed numbers).
//!   * **RP-5** a figure caption's asserted data shape ("N samples") must
//!     match the figure's actual data shape (`top_features_heatmap` is a
//!     single-column log2FC heatmap, not a per-sample expression matrix).
//!   * **RC-COUNT** every `report-data.json` headline count
//!     (`n_significant` / `direction_split`) must equal the value
//!     recomputed directly from its declared source artifact via
//!     [`crate::report_contract::ResultSchema`] +
//!     [`crate::report_contract::summarize_artifact`] — zero tolerance.
//!     Unlike RP-2 (DE/pathway-shaped), this generalizes to every
//!     modality's terminal result artifact.
//!   * **RC-IDENTITY** a `direction_split`'s `up + down` must not EXCEED
//!     `n_significant` (directional rows can't outnumber the significant
//!     set). A shortfall is legitimate — a significant row with a zero/NA
//!     effect counts in `n_significant` but in neither `up` nor `down`.
//!     Artifacts with no split (unsigned modalities, e.g. variant calling)
//!     are skipped, never faulted.
//!   * **RC-SECTIONS** every `required_report_sections` id declared on the
//!     `reporting`/`final_reporting` task specs must appear as a non-empty
//!     section in the emitted report.
//! * **Warn-only** — free-text prose invariants, so a brittle regex can
//!   never block a scientifically-correct deposit:
//!   * **RP-1** effect-abundance direction word (derived structurally from
//!     the sign of `top_effect_abundance_ratio`).
//!   * **RP-3** FDR-family qualification (gene-level vs pathway-level).
//!   * **RP-9** method label (fixed-effects negative-binomial GLM, not a
//!     "linear mixed model").
//!
//! The verdict is folded into the deposit-readiness domain rollup by
//! [`crate::deposit_readiness::scan_domain_validation`], so a **required**
//! failure flips `deposit_ready` to `false`. Warnings are surfaced but
//! never block.
//!
//! Deterministic: reads only files already present under `package_root`,
//! visits inputs in a fixed order, and never consults the wall clock.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use regex::Regex;
use serde_json::Value;

use crate::report_contract::{
    ReportData, ResultSchema, load_policy_column_synonyms, summarize_artifact,
};

/// Severity of a reporting-invariant finding.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// A structural/numeric invariant recomputed against a stage output;
    /// a violation blocks deposit-readiness.
    Required,
    /// A free-text prose invariant; a violation is surfaced but never
    /// blocks.
    Warn,
}

/// One reporting-invariant finding.
#[derive(Debug, Clone)]
pub struct ReportingFinding {
    /// The catalog id of the violated invariant (e.g. `"RP-2"`).
    pub invariant: &'static str,
    /// Whether this finding blocks deposit (`Required`) or is advisory
    /// (`Warn`).
    pub severity: Severity,
    /// Human-readable explanation, including the recomputed vs reported
    /// values where applicable.
    pub detail: String,
}

/// Result of running the bounded reporting-correctness checklist over a
/// package.
#[derive(Debug, Clone, Default)]
pub struct ReportingInvariantsReport {
    /// Catalog ids of the invariants that actually ran (their inputs were
    /// present), in check order. An invariant whose inputs are absent is
    /// silently skipped — absence of an input is never a failure.
    pub checked: Vec<&'static str>,
    /// Every finding, in check order.
    pub findings: Vec<ReportingFinding>,
}

impl ReportingInvariantsReport {
    /// `"<invariant>: <detail>"` for every `Required`-severity finding, in
    /// order. These are the entries that block deposit-readiness.
    pub fn required_failures(&self) -> Vec<String> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Required)
            .map(|f| format!("{}: {}", f.invariant, f.detail))
            .collect()
    }

    /// `"<invariant>: <detail>"` for every `Warn`-severity finding, in
    /// order. Advisory only — these never block.
    pub fn warnings(&self) -> Vec<String> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warn)
            .map(|f| format!("{}: {}", f.invariant, f.detail))
            .collect()
    }

    /// `true` iff no `Required`-severity invariant was violated. Vacuously
    /// `true` when no invariant ran (a package with none of the enumerated
    /// inputs) — a bounded checklist that found nothing to check has
    /// nothing to fail.
    pub fn passed(&self) -> bool {
        !self
            .findings
            .iter()
            .any(|f| f.severity == Severity::Required)
    }
}

/// Run the bounded reporting-correctness checklist over `package_root`,
/// recomputing every value from the package's own runtime outputs. See the
/// module docs for the invariant list and severity split.
pub fn check_reporting_invariants(package_root: &Path) -> ReportingInvariantsReport {
    let mut report = ReportingInvariantsReport::default();
    let outputs = package_root.join("runtime").join("outputs");

    check_rp1_effect_direction(&outputs, &mut report);
    check_rp2_gene_sets_tested(&outputs, &mut report);
    check_rp3_fdr_family(&outputs, &mut report);
    check_rp4_mapping_reconciliation(&outputs, &mut report);
    check_rp5_figure_caption_shape(&outputs, &mut report);
    check_rp9_method_label(&outputs, &mut report);
    check_rc_count(package_root, &outputs, &mut report);
    check_rc_identity(&outputs, &mut report);
    check_rc_sections(package_root, &outputs, &mut report);

    report
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Parse a JSON file, returning `None` when it is missing or unparseable.
fn read_json(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Concatenate the package's human-readable report markdown files, in a
/// fixed order, returning `None` when none are present. Both the terminal
/// `final_reporting/final_report.md` and the intermediate
/// `reporting/report.md` are considered — a defect in either is a defect.
fn read_reports(outputs: &Path) -> Option<String> {
    let candidates = [
        outputs.join("final_reporting").join("final_report.md"),
        outputs.join("reporting").join("report.md"),
    ];
    let mut combined = String::new();
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path) {
            combined.push_str(&text);
            combined.push('\n');
        }
    }
    (!combined.is_empty()).then_some(combined)
}

/// Parse an integer that may carry `,` thousands separators (`"5,179"`).
fn parse_grouped_int(s: &str) -> Option<u64> {
    let cleaned: String = s.chars().filter(|c| *c != ',').collect();
    if cleaned.is_empty() {
        return None;
    }
    cleaned.parse().ok()
}

/// Interpret a JSON scalar as a non-negative integer count. Accepts a plain
/// u64, an exact non-negative integral float (`17190.0` — how numpy /
/// pandas / jsonlite routinely serialize integer counts), and a numeric
/// string (`"17190"` / `"17,190"`). Returns `None` for anything else
/// (fractional floats, non-numeric strings, bools, null). Accepting the
/// float/string encodings is the conservative direction for the REQUIRED
/// RP-4 gate: it WIDENS the set of stage-output-sourced counts, so a
/// scientifically-correct count recorded as a float or string is no longer
/// mistaken for a narrative-only number.
fn as_count(value: &Value) -> Option<u64> {
    match value {
        Value::Number(n) => n.as_u64().or_else(|| {
            n.as_f64().and_then(|f| {
                (f.is_finite() && f >= 0.0 && f.fract() == 0.0 && f <= u64::MAX as f64)
                    .then_some(f as u64)
            })
        }),
        Value::String(s) => parse_grouped_int(s.trim()),
        _ => None,
    }
}

/// Normalize a collection label for tolerant comparison: lower-case and
/// strip every non-alphanumeric character, so `"GO_BP"`, `"go-bp"`, and
/// `"GO BP"` all compare equal. RP-2 uses this so a pure label-FORMAT
/// difference between `pathway_summary.json` keys and the TSV `collection`
/// column can never trigger a REQUIRED failure.
fn normalize_label(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Recursively collect every non-negative integer count in a JSON tree into
/// `out`, accepting integral-float and numeric-string encodings (see
/// [`as_count`]). Used to build the set of stage-output-sourced gene counts
/// RP-4 reconciles a report's mapping claims against.
fn collect_ints(value: &Value, out: &mut BTreeSet<u64>) {
    match value {
        Value::Array(items) => items.iter().for_each(|v| collect_ints(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_ints(v, out)),
        scalar => {
            if let Some(u) = as_count(scalar) {
                out.insert(u);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RP-2 (Required) — reported gene_sets_tested == recomputed tested rowcount
// ---------------------------------------------------------------------------

/// The reported `gene_sets_tested` count (per collection AND total) must
/// equal the post-filter *tested* rowcount — recomputed here by counting
/// the data rows of `pathway_results.tsv` grouped by the `collection`
/// column — never the *loaded* count. The deposited package reported the
/// loaded count (10085) recorded in `pathway_summary.json` while the TSV
/// carried only the 5056 rows that actually survived the size filter.
fn check_rp2_gene_sets_tested(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let pe = outputs.join("pathway_enrichment");
    let Some(summary) = read_json(&pe.join("pathway_summary.json")) else {
        return;
    };
    let Some(reported) = summary.get("gene_sets_tested").and_then(Value::as_object) else {
        return;
    };
    let Ok(tsv) = std::fs::read_to_string(pe.join("pathway_results.tsv")) else {
        return;
    };
    report.checked.push("RP-2");

    // Recompute the tested rowcount per collection (first column) + total.
    let mut recomputed: BTreeMap<String, u64> = BTreeMap::new();
    let mut recomputed_total: u64 = 0;
    for line in tsv.lines().skip(1) {
        let coll = line.split('\t').next().unwrap_or("").trim();
        if coll.is_empty() {
            continue;
        }
        *recomputed.entry(coll.to_string()).or_insert(0) += 1;
        recomputed_total += 1;
    }

    // Normalized lookup of the recomputed per-collection counts, so a pure
    // label-FORMAT difference (case/separator) never blocks.
    let recomputed_norm: BTreeMap<String, u64> = recomputed
        .iter()
        .map(|(raw, n)| (normalize_label(raw), *n))
        .collect();

    // Compare each reported entry (sorted for deterministic output) against
    // the recomputed value. `total` is the HARD REQUIRED check (row total).
    // A per-collection entry is compared by NORMALIZED label; if it has no
    // TSV counterpart after normalization it is unverifiable and SKIPPED —
    // never a REQUIRED failure on a label-format difference alone.
    let mut mismatches: Vec<String> = Vec::new();
    for (key, reported_val) in reported {
        let Some(reported_n) = as_count(reported_val) else {
            continue;
        };
        if key == "total" {
            if reported_n != recomputed_total {
                mismatches.push(format!(
                    "total: reported {reported_n} vs recomputed {recomputed_total}"
                ));
            }
            continue;
        }
        // A reported collection with no normalized TSV counterpart is
        // unverifiable — silently skipped, never a REQUIRED failure on a
        // label-format difference alone.
        if let Some(&actual) = recomputed_norm.get(&normalize_label(key)) {
            if reported_n != actual {
                mismatches.push(format!(
                    "{key}: reported {reported_n} vs recomputed {actual}"
                ));
            }
        }
    }
    if !mismatches.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RP-2",
            severity: Severity::Required,
            detail: format!(
                "reported gene_sets_tested does not equal the post-filter tested rowcount \
                 recomputed from pathway_results.tsv (loaded-not-tested inflation) — {}",
                mismatches.join("; ")
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// RP-4 (Required) — mapping counts trace to a stage output
// ---------------------------------------------------------------------------

/// Every mapped/unmapped/resolved/unresolved gene count the report asserts
/// must trace to a value some stage actually recorded. The deposited report
/// stated "5,179 unmapped" — a narrative-only number back-computed as
/// `22369 − 17190` that appears in no stage output; the real recorded
/// unmapped count was 5,160.
fn check_rp4_mapping_reconciliation(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(reports) = read_reports(outputs) else {
        return;
    };
    // Build the set of stage-output-sourced integers by scanning EVERY
    // stage's `result.json` (plus the pathway summary, which is not a
    // `result.json` but carries mapping counts). Widening this set is the
    // conservative direction for a REQUIRED gate (fewer false-positive
    // blocks): a narrative count is flagged only when it appears in NO stage
    // output at all — not merely because it happened to be recorded in a
    // stage dir this check did not previously enumerate.
    let mut sourced: BTreeSet<u64> = BTreeSet::new();
    if let Ok(entries) = std::fs::read_dir(outputs) {
        // Sort for deterministic traversal (the BTreeSet result is
        // order-independent, but fixed order keeps behavior reproducible).
        let mut stage_dirs: Vec<std::path::PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        stage_dirs.sort();
        for dir in stage_dirs {
            if let Some(v) = read_json(&dir.join("result.json")) {
                collect_ints(&v, &mut sourced);
            }
        }
    }
    if let Some(v) = read_json(
        &outputs
            .join("pathway_enrichment")
            .join("pathway_summary.json"),
    ) {
        collect_ints(&v, &mut sourced);
    }
    if sourced.is_empty() {
        // No mapping stage outputs to reconcile against — nothing to check.
        return;
    }
    report.checked.push("RP-4");

    // `<number> [up to 2 words] (un)mapped|(un)resolved` — captures
    // "17,190 successfully mapped", "5,179 unmapped", "17,209 resolved".
    let re =
        Regex::new(r"(\d[\d,]*)\s+(?:[A-Za-z-]+\s+){0,2}(mapped|unmapped|resolved|unresolved)")
            .expect("static RP-4 regex compiles");
    let mut offenders: Vec<String> = Vec::new();
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    for cap in re.captures_iter(&reports) {
        let Some(n) = parse_grouped_int(&cap[1]) else {
            continue;
        };
        if !sourced.contains(&n) && seen.insert(n) {
            offenders.push(format!("{n} {}", &cap[2]));
        }
    }
    if !offenders.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RP-4",
            severity: Severity::Required,
            detail: format!(
                "report asserts gene mapping count(s) that trace to no stage output \
                 (narrative-only / back-computed): {}",
                offenders.join(", ")
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// RP-5 (Required) — figure caption shape matches figure data shape
// ---------------------------------------------------------------------------

/// `top_features_heatmap` is, by the plotting library's construction, a
/// single-column log2FC heatmap (one column per DE contrast), not a
/// per-sample expression matrix. A caption ASSERTING it shows a per-sample
/// shape — "across N samples" or an "N-sample expression heatmap/matrix" —
/// misrepresents its data shape (the deposited report captioned it as an
/// "expression heatmap … across 8 samples"). A truthful PROVENANCE mention
/// ("derived from N samples" / "computed from N samples") describes the
/// upstream data rather than the figure's shape and never blocks: the check
/// fires only on a positively-matched shape assertion. Gated on the figure
/// actually being present so it only fires for packages that render it.
fn check_rp5_figure_caption_shape(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let de = outputs.join("differential_expression");
    if !de.join("figures").join("top_features_heatmap.png").exists() {
        return;
    }
    let Some(reports) = read_reports(outputs) else {
        return;
    };
    report.checked.push("RP-5");

    let contrast = read_json(&de.join("result.json")).and_then(|v| {
        v.get("contrast")
            .and_then(|c| c.as_str().map(str::to_string))
    });

    // Assemble the figure's CAPTION BLOCK — the line naming the figure plus
    // any wrapped continuation lines up to a blank line, the next bullet, a
    // table row, or a heading — rather than inspecting a single line, so a
    // caption spanning lines is judged as one unit.
    let lines: Vec<&str> = reports.lines().collect();
    let Some(start) = lines
        .iter()
        .position(|l| l.contains("top_features_heatmap"))
    else {
        return;
    };
    let mut block = String::new();
    for (i, line) in lines.iter().enumerate().skip(start) {
        if i > start {
            let t = line.trim_start();
            if t.is_empty()
                || t.starts_with("- ")
                || t.starts_with("* ")
                || t.starts_with('#')
                || t.starts_with('|')
            {
                break;
            }
        }
        block.push_str(line);
        block.push(' ');
    }
    let block_lc = block.to_lowercase();

    // Only a SHAPE ASSERTION about the figure blocks: the figure is claimed
    // to display data "across N samples", or to be an "N-sample expression
    // heatmap/matrix". A truthful PROVENANCE mention ("derived from N
    // samples" / "computed from N samples") describes the upstream data, not
    // the figure's shape, and must never block — so we fire only on a
    // positively-matched assertion pattern.
    let assert_across =
        Regex::new(r"across\s+(\d[\d,]*)\s+samples?\b").expect("static RP-5 across regex compiles");
    let assert_n_sample =
        Regex::new(r"(\d[\d,]*)[\s-]samples?\s+(?:expression\s+)?(?:heatmap|matrix)")
            .expect("static RP-5 n-sample regex compiles");
    let n_claimed = assert_across
        .captures(&block_lc)
        .or_else(|| assert_n_sample.captures(&block_lc))
        .map(|c| c[1].to_string());
    let Some(n) = n_claimed else {
        return;
    };

    let contrast_note = contrast
        .as_deref()
        .map(|c| format!(" (its single column is the {c} log2FC)"))
        .unwrap_or_default();
    report.findings.push(ReportingFinding {
        invariant: "RP-5",
        severity: Severity::Required,
        detail: format!(
            "caption for top_features_heatmap asserts a {n}-sample expression matrix, but the \
             figure is a single-column log2FC heatmap{contrast_note}, not a per-sample \
             expression matrix"
        ),
    });
}

// ---------------------------------------------------------------------------
// RP-1 (Warn) — effect-abundance direction word vs computed ratio sign
// ---------------------------------------------------------------------------

/// Warn-only, but derived structurally from the SIGN of the computed
/// `top_effect_abundance_ratio` rather than from a free-text regex over the
/// prose: a ratio < 1 means the top effects sit BELOW background, so
/// narrative that calls them "above" is inverted (the deposited report did
/// exactly this at ratio 0.558). Left warn-only per §G-C1 so a prose
/// mismatch cannot block an otherwise-correct deposit.
fn check_rp1_effect_direction(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(de) = read_json(&outputs.join("differential_expression").join("result.json")) else {
        return;
    };
    let Some(ratio) = de.get("top_effect_abundance_ratio").and_then(Value::as_f64) else {
        return;
    };
    let Some(narrative) = de.get("narrative_text").and_then(Value::as_str) else {
        return;
    };
    report.checked.push("RP-1");

    let lower = narrative.to_lowercase();
    let says_above = ["above", "higher", "greater", "exceed"]
        .iter()
        .any(|w| lower.contains(w));
    let says_below = ["below", "lower", "less than", "beneath"]
        .iter()
        .any(|w| lower.contains(w));
    let finding = if ratio < 1.0 && says_above && !says_below {
        Some(format!(
            "narrative describes the top-effect abundance as ABOVE background, but \
             top_effect_abundance_ratio = {ratio:.4} (< 1) means the top effects sit BELOW \
             the whole-set median abundance"
        ))
    } else if ratio > 1.0 && says_below && !says_above {
        Some(format!(
            "narrative describes the top-effect abundance as BELOW background, but \
             top_effect_abundance_ratio = {ratio:.4} (> 1) means the top effects sit ABOVE \
             the whole-set median abundance"
        ))
    } else {
        None
    };
    if let Some(detail) = finding {
        report.findings.push(ReportingFinding {
            invariant: "RP-1",
            severity: Severity::Warn,
            detail,
        });
    }
}

// ---------------------------------------------------------------------------
// RP-3 (Warn) — FDR family qualification
// ---------------------------------------------------------------------------

/// Warn when the report cites two distinct FDR thresholds (the gene-level DE
/// family and the pathway-level enrichment family) but labels both a bare
/// "FDR" with no family qualifier. Warn-only — an FDR-labelling nuance must
/// not block a deposit.
fn check_rp3_fdr_family(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(reports) = read_reports(outputs) else {
        return;
    };
    if !reports.contains("FDR") {
        return;
    }
    report.checked.push("RP-3");

    let re = Regex::new(r"FDR[^0-9]{0,10}(0\.\d+)").expect("static RP-3 regex compiles");
    let thresholds: BTreeSet<String> = re
        .captures_iter(&reports)
        .map(|c| c[1].to_string())
        .collect();
    let lower = reports.to_lowercase();
    let family_qualified = lower.contains("gene-level")
        || lower.contains("pathway-level")
        || lower.contains("enrichment fdr")
        || lower.contains("gene level fdr");
    if thresholds.len() >= 2 && !family_qualified {
        let mut list: Vec<String> = thresholds.into_iter().collect();
        list.sort();
        report.findings.push(ReportingFinding {
            invariant: "RP-3",
            severity: Severity::Warn,
            detail: format!(
                "report labels two distinct FDR thresholds ({}) as bare \"FDR\" without \
                 disambiguating the gene-level DE family from the pathway-level enrichment family",
                list.join(", ")
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// RP-9 (Warn) — method label matches executed model
// ---------------------------------------------------------------------------

/// Mixed-model phrases whose AFFIRMATIVE use RP-9 flags.
const MIXED_MODEL_PHRASES: &[&str] = &[
    "linear mixed model",
    "linear mixed-effects",
    "linear mixed effects",
    "mixed-effects model",
    "mixed effects model",
];

/// True when the report AFFIRMATIVELY labels the DE model a linear mixed model.
///
/// A correct fixed-effects report often DISAVOWS a mixed model explicitly
/// ("this is NOT a linear mixed model; the design is fixed-effects"). A naive
/// substring match fired on that disavowal and flagged a correct report for the
/// opposite of what it says (himes rerun audit 2026-07-21). So each occurrence
/// is affirmative only when it is NOT immediately preceded by a negation cue.
fn mentions_mixed_model_affirmatively(reports: &str) -> bool {
    // Negation cues that, within the short window immediately before a phrase,
    // mark the mention as a disavowal rather than a label.
    const NEGATION_CUES: &[&str] = &[
        "not ", "n't ", "rather than", "instead of", "no ", "without", "isn't", "aren't",
    ];
    const WINDOW: usize = 24;
    let lower = reports.to_lowercase();
    for phrase in MIXED_MODEL_PHRASES {
        let mut from = 0;
        while let Some(rel) = lower[from..].find(phrase) {
            let idx = from + rel;
            let before = &lower[idx.saturating_sub(WINDOW)..idx];
            let negated = NEGATION_CUES.iter().any(|cue| before.contains(cue));
            if !negated {
                return true;
            }
            from = idx + phrase.len();
        }
    }
    false
}

/// Warn when the report labels the DE model a "linear mixed model"; the
/// executed model is a fixed-effects negative-binomial GLM (DESeq2
/// `~ cell + dex`). Warn-only per §G-C1. Negation-aware: a report that
/// explicitly disavows a mixed model is not flagged.
fn check_rp9_method_label(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(reports) = read_reports(outputs) else {
        return;
    };
    report.checked.push("RP-9");

    if mentions_mixed_model_affirmatively(&reports) {
        report.findings.push(ReportingFinding {
            invariant: "RP-9",
            severity: Severity::Warn,
            detail: "report labels the DE model a \"linear mixed model\"; the executed model is \
                     a fixed-effects negative-binomial GLM (e.g. DESeq2 ~ cell + dex), not a \
                     mixed model"
                .to_string(),
        });
    }
}

// ---------------------------------------------------------------------------
// RC-COUNT / RC-IDENTITY / RC-SECTIONS (Required) — recompute-from-source
// enforcement layer, generalized to every modality via `ResultSchema`
// (comprehensive-reporting-contract, §G-C1 Task E).
// ---------------------------------------------------------------------------
//
// RP-2/RP-4/RP-5 above are DE/pathway-shaped: they recompute a specific
// gene-set/mapping/figure invariant. These three checks generalize the same
// source-owned posture — never trust a narrative or per-run validator, only
// the package's own runtime outputs — to EVERY modality's terminal result
// artifact, by reading exclusively through the atom-declared `ResultSchema`
// (never a hardcoded gene/log2FC/padj literal). All three are
// `Severity::Required`, so a genuine mismatch gates the deposit through
// `deposit_readiness::scan_domain_validation`'s unfiltered fold of
// `required_failures()`.

/// Reads `WORKFLOW.json`'s `assemble_report_data` task's
/// `spec.report_schemas` into the `BTreeMap<stage_id, ResultSchema>` the
/// assembler itself was built from. `None` when the file, task, or field is
/// absent/unparseable — a package with no schema map has nothing for
/// RC-COUNT to recompute against.
fn read_report_schemas(package_root: &Path) -> Option<BTreeMap<String, ResultSchema>> {
    let wf = read_json(&package_root.join("WORKFLOW.json"))?;
    let schemas_val = wf
        .get("tasks")?
        .get("assemble_report_data")?
        .get("spec")?
        .get("report_schemas")?;
    serde_json::from_value(schemas_val.clone()).ok()
}

/// Reads the union of `required_report_sections` declared on WORKFLOW.json's
/// `reporting` AND `final_reporting` task specs (both normally declare the
/// same atom-level obligation; unioning is a no-op when they agree, and the
/// conservative direction — checking more, not fewer, sections — when they
/// don't). Empty when neither task declares any sections.
fn read_required_report_sections(package_root: &Path) -> Vec<String> {
    let Some(wf) = read_json(&package_root.join("WORKFLOW.json")) else {
        return Vec::new();
    };
    let mut sections: BTreeSet<String> = BTreeSet::new();
    for task_id in ["reporting", "final_reporting"] {
        if let Some(arr) = wf
            .get("tasks")
            .and_then(|t| t.get(task_id))
            .and_then(|t| t.get("spec"))
            .and_then(|s| s.get("required_report_sections"))
            .and_then(Value::as_array)
        {
            for v in arr {
                if let Some(s) = v.as_str() {
                    sections.insert(s.to_string());
                }
            }
        }
    }
    sections.into_iter().collect()
}

/// Reads `report-data.json` from `outputs/reporting/report-data.json`.
/// `None` when absent or unparseable.
fn read_report_data(outputs: &Path) -> Option<ReportData> {
    let raw = std::fs::read_to_string(outputs.join("reporting").join("report-data.json")).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Reads the emitted report text the SME actually reads: the terminal
/// `final_reporting/final_report.md` when present, else the intermediate
/// `reporting/report.md`. Unlike [`read_reports`] (which concatenates both
/// for the RP-4/RP-9 narrative scans), RC-SECTIONS checks the ONE document a
/// reader lands on, preferring the more complete terminal report.
fn read_terminal_report(outputs: &Path) -> Option<String> {
    let final_path = outputs.join("final_reporting").join("final_report.md");
    if let Ok(text) = std::fs::read_to_string(&final_path) {
        return Some(text);
    }
    std::fs::read_to_string(outputs.join("reporting").join("report.md")).ok()
}

/// RC-COUNT: defense-in-depth over `report-data.json` itself. For every
/// artifact whose stage has a declared schema AND whose source artifact is
/// present on disk, recompute its stats directly from the source table (via
/// the same [`summarize_artifact`] the assembler itself used) and require its
/// `n_significant` — and, when both sides declare one, its `direction_split`
/// — to equal what `report-data.json` states, EXACTLY (zero tolerance).
///
/// What this actually catches: a `report-data.json` whose headline counts no
/// longer agree with a fresh recompute from the source artifact — e.g. a
/// stale `report-data.json` left behind when the source stage re-ran without
/// re-assembly, a hand-edit of the JSON, or a post-assembly mutation of the
/// source table. On the untouched happy path (report-data.json written by
/// this exact recompute) it is tautological, which is the point: it makes the
/// deterministic contract file self-verifying at gate time.
///
/// What this does NOT do: it never reads or parses the human-readable
/// narrative (`report.md` / `final_report.md`). Narrative-number correctness
/// is not gated here (deterministically gating free-text prose numbers is
/// fragile); it is enforced upstream — the assembler is the single source of
/// truth for every headline number, and the task-execution prompt requires
/// the reporting agent to cite those numbers from `report-data.json` rather
/// than inventing them.
fn check_rc_count(package_root: &Path, outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(report_data) = read_report_data(outputs) else {
        return;
    };
    let Some(schemas) = read_report_schemas(package_root) else {
        return;
    };

    // The RC-COUNT recompute must resolve columns IDENTICALLY to the assembler,
    // so it loads the SAME policy synonym lists (see `assemble_report_data`).
    let synonyms = load_policy_column_synonyms(package_root);

    let mut ran = false;
    let mut mismatches: Vec<String> = Vec::new();
    for artifact in &report_data.artifacts {
        let Some(schema) = schemas.get(&artifact.stage_id) else {
            continue;
        };
        let source_path = outputs.join(&artifact.stage_id).join(&schema.artifact);
        if !source_path.exists() {
            continue;
        }
        let Ok((headers, rows)) = crate::report_contract::assemble::read_table(&source_path)
        else {
            continue;
        };
        ran = true;
        let stats = summarize_artifact(&rows, &headers, schema, &synonyms);

        if stats.n_significant != artifact.n_significant {
            mismatches.push(format!(
                "{}: n_significant reported {:?} vs recomputed {:?} from {}",
                artifact.stage_id, artifact.n_significant, stats.n_significant, schema.artifact
            ));
        }
        if let (Some(reported), Some(actual)) =
            (&artifact.direction_split, &stats.direction_split)
        {
            if reported.up != actual.up || reported.down != actual.down {
                mismatches.push(format!(
                    "{}: direction_split reported up={}/down={} vs recomputed up={}/down={}",
                    artifact.stage_id, reported.up, reported.down, actual.up, actual.down
                ));
            }
        }
    }
    if !ran {
        return;
    }
    report.checked.push("RC-COUNT");
    if !mismatches.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-COUNT",
            severity: Severity::Required,
            detail: format!(
                "report-data.json headline count(s) disagree with the value recomputed \
                 directly from the source artifact (zero tolerance) — {}",
                mismatches.join("; ")
            ),
        });
    }
}

/// RC-IDENTITY: for every `report-data.json` artifact that declares a
/// `direction_split`, its `up + down` must not EXCEED `n_significant` — the
/// directional rows are a subset of the significant set, so they can never
/// outnumber it. A shortfall (`up + down < n_significant`) is legitimate and
/// NOT flagged: a significant row (padj passes) whose signed effect is exactly
/// zero, NA, or unparseable is counted in `n_significant` but in neither `up`
/// nor `down`. The gross inconsistency this guards against — a split that
/// exceeds the significant count (himes-style `4017 > 3993`) — still fails.
/// Artifacts with no split (unsigned modalities — e.g. variant calling has no
/// signed effect column) or no `n_significant` to reconcile against have
/// nothing to check and are silently skipped — never a failure.
fn check_rc_identity(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(report_data) = read_report_data(outputs) else {
        return;
    };

    let mut ran = false;
    let mut mismatches: Vec<String> = Vec::new();
    for artifact in &report_data.artifacts {
        let Some(split) = &artifact.direction_split else {
            continue;
        };
        // A split with no significance count (e.g. no significance declared,
        // so the split was computed over all rows) has no significant set to
        // be a subset of — nothing to reconcile.
        let Some(n_sig) = artifact.n_significant else {
            continue;
        };
        ran = true;
        let sum = split.up + split.down;
        if sum > n_sig {
            mismatches.push(format!(
                "{}: direction_split up={}+down={}={} exceeds n_significant={}",
                artifact.stage_id, split.up, split.down, sum, n_sig
            ));
        }
    }
    if !ran {
        return;
    }
    report.checked.push("RC-IDENTITY");
    if !mismatches.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-IDENTITY",
            severity: Severity::Required,
            detail: format!(
                "direction_split up+down exceeds n_significant (directional rows cannot \
                 outnumber the significant set) — {}",
                mismatches.join("; ")
            ),
        });
    }
}

/// The significant words of a required-section id, `_`/`-`/whitespace-split,
/// lower-cased, empties dropped: `"provenance_method_rationale"` →
/// `["provenance", "method", "rationale"]`. Deliberately minimal — no
/// per-modality heading vocabulary, so the check never hardcodes a
/// domain-specific section name.
fn section_id_words(id: &str) -> Vec<String> {
    id.split(|c: char| c == '_' || c == '-' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// The markdown heading LEVEL of a line — the count of leading `#` after
/// trimming leading whitespace — or `None` when the line is not a heading.
/// `## X` → `Some(2)`, `#### Y` → `Some(4)`, prose → `None`.
fn heading_level(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    Some(trimmed.chars().take_while(|&c| c == '#').count())
}

/// Whether a single line is a markdown HEADING that contains EVERY word of
/// the section id (case-insensitive substring, order-independent). Only
/// heading lines (`#`-prefixed after trimming) qualify, so a prose mention of
/// the id's words can never anchor a section. Order-independence lets a
/// natural heading like `## Provenance & Method-Selection Rationale` satisfy
/// `provenance_method_rationale` (intervening words / punctuation are fine),
/// which the old consecutive-words regex false-blocked.
fn heading_matches_section(line: &str, words: &[String]) -> bool {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return false;
    }
    let lc = trimmed.to_lowercase();
    words.iter().all(|w| lc.contains(w.as_str()))
}

/// Whether required section `id` is present as a matching markdown HEADING in
/// `text` AND that heading is followed by non-whitespace content before the
/// next heading of EQUAL-OR-SHALLOWER level (or EOF). `None` when no heading
/// matches (missing); `Some(false)` when a heading matches but is immediately
/// followed by nothing but blank lines / a same-or-shallower heading (present
/// but empty); `Some(true)` otherwise. Restricting the match to heading lines
/// kills the false-anchor on a prose mention that preceded the real heading.
///
/// The section boundary is heading-LEVEL-aware: the section ends at the next
/// heading whose level (count of leading `#`) is <= the matched heading's
/// level. A DEEPER subheading (more `#`) is part of this section's content, so
/// a required section whose first content is a `###` subheading is correctly
/// non-empty (its subheading text counts as non-whitespace) rather than
/// false-flagged "present but empty".
fn section_has_content(text: &str, id: &str) -> Option<bool> {
    let words = section_id_words(id);
    if words.is_empty() {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    let start = lines
        .iter()
        .position(|l| heading_matches_section(l, &words))?;
    // The matched heading is `#`-prefixed (heading_matches_section required
    // it), so `heading_level` is always `Some`; default 1 is defensive only.
    let matched_level = heading_level(lines[start]).unwrap_or(1);
    let mut content = String::new();
    for line in lines.iter().skip(start + 1) {
        if let Some(level) = heading_level(line) {
            if level <= matched_level {
                break;
            }
        }
        content.push_str(line);
        content.push('\n');
    }
    Some(!content.trim().is_empty())
}

/// RC-SECTIONS: every id in `required_report_sections` (declared on the
/// `reporting`/`final_reporting` task specs) must appear in the emitted
/// terminal report text as a non-empty section. Runs only when both the
/// requirement list and a report are present — a package with neither has
/// nothing to check.
fn check_rc_sections(package_root: &Path, outputs: &Path, report: &mut ReportingInvariantsReport) {
    let required = read_required_report_sections(package_root);
    if required.is_empty() {
        return;
    }
    let Some(text) = read_terminal_report(outputs) else {
        return;
    };
    report.checked.push("RC-SECTIONS");

    let mut offenders: Vec<String> = Vec::new();
    for id in &required {
        match section_has_content(&text, id) {
            Some(true) => {}
            Some(false) => offenders.push(format!("{id} (present but empty)")),
            None => offenders.push(format!("{id} (missing)")),
        }
    }
    if !offenders.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-SECTIONS",
            severity: Severity::Required,
            detail: format!(
                "required report section(s) missing or empty: {}",
                offenders.join(", ")
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Build a package skeleton and return its `runtime/outputs` dir.
    fn outputs_dir(tmp: &TempDir) -> PathBuf {
        let outputs = tmp.path().join("runtime").join("outputs");
        std::fs::create_dir_all(&outputs).unwrap();
        outputs
    }

    fn write(outputs: &Path, rel: &str, body: &str) {
        let path = outputs.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// A `pathway_results.tsv` with the given per-collection row counts.
    fn pathway_results_tsv(collections: &[(&str, usize)]) -> String {
        let mut s = String::from("collection\tpathway\tpval\tpadj\tES\tNES\tsize\tleadingEdge\n");
        for (coll, n) in collections {
            for i in 0..*n {
                s.push_str(&format!(
                    "{coll}\t{coll}_SET_{i}\t0.01\t0.02\t0.5\t1.5\t100\tA|B|C\n"
                ));
            }
        }
        s
    }

    // -- RP-2 -------------------------------------------------------------

    #[test]
    fn rp2_loaded_not_tested_count_is_required_failure() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        // Actually tested (post-filter) rowcount: HALLMARK 2, GO_BP 3 → total 5.
        write(
            &outputs,
            "pathway_enrichment/pathway_results.tsv",
            &pathway_results_tsv(&[("HALLMARK", 2), ("GO_BP", 3)]),
        );
        // Reported the LOADED count (the RP-2 defect): total 10085, GO_BP 7538.
        write(
            &outputs,
            "pathway_enrichment/pathway_summary.json",
            &serde_json::json!({
                "gene_sets_tested": {
                    "HALLMARK": 2, "GO_BP": 7538, "total": 10085
                }
            })
            .to_string(),
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(
            report.checked.contains(&"RP-2"),
            "RP-2 must run when the pathway inputs are present"
        );
        assert!(
            !report.passed(),
            "a loaded-not-tested gene_sets_tested count must be a REQUIRED failure: {report:?}"
        );
        let failures = report.required_failures();
        assert!(
            failures
                .iter()
                .any(|f| f.contains("RP-2") && f.contains("10085") && f.contains('5')),
            "RP-2 failure must name the reported (10085) vs recomputed (5) total: {failures:?}"
        );
        assert!(
            failures.iter().any(|f| f.contains("GO_BP")),
            "RP-2 must catch the per-collection GO_BP inflation too: {failures:?}"
        );
    }

    #[test]
    fn rp2_tested_count_matches_passes() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "pathway_enrichment/pathway_results.tsv",
            &pathway_results_tsv(&[("HALLMARK", 2), ("GO_BP", 3)]),
        );
        write(
            &outputs,
            "pathway_enrichment/pathway_summary.json",
            &serde_json::json!({
                "gene_sets_tested": { "HALLMARK": 2, "GO_BP": 3, "total": 5 }
            })
            .to_string(),
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-2"));
        assert!(
            report.passed(),
            "a correctly-reported count must pass: {report:?}"
        );
        assert!(report.required_failures().is_empty());
    }

    // -- RP-4 -------------------------------------------------------------

    fn seed_mapping_stage_outputs(outputs: &Path) {
        write(
            outputs,
            "pathway_enrichment/pathway_summary.json",
            &serde_json::json!({
                "n_genes_ranked": 17190,
                "n_genes_unmapped": 5160,
                "gene_sets_tested": {"total": 5}
            })
            .to_string(),
        );
        write(
            outputs,
            "contextualize_findings_with_literature/result.json",
            &serde_json::json!({
                "counts": {
                    "ensembl_ids_resolved": 17209,
                    "ensembl_ids_unresolved": 5160,
                    "total": 22369
                }
            })
            .to_string(),
        );
        write(
            outputs,
            "differential_expression/result.json",
            &serde_json::json!({ "n_genes_tested": 22369 }).to_string(),
        );
    }

    #[test]
    fn rp4_backcomputed_unmapped_number_is_required_failure() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        seed_mapping_stage_outputs(&outputs);
        // The report back-computes "5,179 unmapped" (= 22369 - 17190); that
        // number appears in NO stage output.
        write(
            &outputs,
            "final_reporting/final_report.md",
            "fgsea was run on the full ranked gene list (22,369 genes; \
             17,190 successfully mapped; 5,179 unmapped).\n\
             Gene symbols were resolved (17,209 resolved, 5,160 unresolved).\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-4"));
        assert!(
            !report.passed(),
            "back-computed unmapped count must block: {report:?}"
        );
        assert!(
            report
                .required_failures()
                .iter()
                .any(|f| f.contains("RP-4") && f.contains("5179")),
            "RP-4 must name the narrative-only 5,179: {:?}",
            report.required_failures()
        );
    }

    #[test]
    fn rp4_all_counts_sourced_passes() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        seed_mapping_stage_outputs(&outputs);
        write(
            &outputs,
            "final_reporting/final_report.md",
            "22,369 genes; 17,190 mapped; 5,160 unmapped; \
             17,209 resolved, 5,160 unresolved.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-4"));
        assert!(
            report.passed(),
            "all counts traceable to stage outputs: {report:?}"
        );
    }

    // -- RP-5 -------------------------------------------------------------

    #[test]
    fn rp5_heatmap_caption_sample_shape_is_required_failure() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "differential_expression/figures/top_features_heatmap.png",
            "PNG",
        );
        write(
            &outputs,
            "differential_expression/result.json",
            &serde_json::json!({ "contrast": "dex_trt_vs_untrt" }).to_string(),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "- **top_features_heatmap** (differential_expression): expression \
             heatmap of top DE genes across 8 samples.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-5"));
        assert!(
            !report.passed(),
            "captioning a 1-column log2FC heatmap as an 8-sample matrix must block: {report:?}"
        );
        assert!(
            report
                .required_failures()
                .iter()
                .any(|f| f.contains("RP-5") && f.contains('8')),
            "RP-5 must name the false 8-sample claim: {:?}",
            report.required_failures()
        );
    }

    #[test]
    fn rp5_faithful_caption_passes() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "differential_expression/figures/top_features_heatmap.png",
            "PNG",
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "- **top_features_heatmap**: single-column log2FC of the top DE \
             genes for the dex_trt_vs_untrt contrast.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            report.passed(),
            "a shape-faithful caption must pass: {report:?}"
        );
    }

    // -- RP-9 (warn) ------------------------------------------------------

    #[test]
    fn rp9_linear_mixed_model_label_is_warning_not_block() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "final_reporting/final_report.md",
            "All results were produced under a linear mixed model (`~ cell + dex`).\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            report.passed(),
            "a warn-only method-label finding must NOT block deposit: {report:?}"
        );
        assert!(
            report.warnings().iter().any(|w| w.contains("RP-9")),
            "RP-9 must surface as a warning: {:?}",
            report.warnings()
        );
        assert!(report.required_failures().is_empty());
    }

    #[test]
    fn rp9_negated_mixed_model_disavowal_does_not_warn() {
        // Regression (himes rerun audit 2026-07-21): a report that CORRECTLY
        // disavows a mixed model must NOT trip RP-9. The naive substring match
        // fired on "This is NOT a linear mixed model", flagging a correct
        // report for the opposite of what it says. RP-9 must fire only on
        // AFFIRMATIVE use.
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "final_reporting/final_report.md",
            "The design `~ cell + dex` treats cell line as a fixed effect. This is NOT a \
             linear mixed model; results are a fixed-effects negative-binomial GLM (DESeq2).\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            !report.warnings().iter().any(|w| w.contains("RP-9")),
            "RP-9 must NOT fire on a negated disavowal of a mixed model: {:?}",
            report.warnings()
        );
        assert!(report.required_failures().is_empty());
    }

    // -- RP-1 (warn, structural) -----------------------------------------

    #[test]
    fn rp1_reversed_direction_is_structural_warning() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "differential_expression/result.json",
            &serde_json::json!({
                "top_effect_abundance_ratio": 0.5579336,
                "narrative_text": "the top genes have a median baseMean substantially \
                                   above the whole-set median, well-supported by signal."
            })
            .to_string(),
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            report.passed(),
            "RP-1 is warn-only and must not block: {report:?}"
        );
        assert!(
            report.warnings().iter().any(|w| w.contains("RP-1")),
            "ratio<1 described as 'above' must warn: {:?}",
            report.warnings()
        );
    }

    #[test]
    fn rp1_consistent_direction_no_warning() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "differential_expression/result.json",
            &serde_json::json!({
                "top_effect_abundance_ratio": 0.5579336,
                "narrative_text": "the top effects sit below the whole-set median \
                                   abundance, a low-count-artifact signal."
            })
            .to_string(),
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.warnings().iter().all(|w| !w.contains("RP-1")));
    }

    // -- RP-4 over-block guards ------------------------------------------

    /// A count serialized as a JSON float (`17190.0` — common from
    /// numpy/pandas/jsonlite) or as a numeric string, or recorded in a
    /// non-canonical stage dir (`reporting/result.json`), must count as
    /// "sourced" — it must NOT trip the REQUIRED narrative-only gate.
    #[test]
    fn rp4_float_string_and_reporting_dir_counts_are_sourced() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        // 17190 as a JSON FLOAT; 5160 as a numeric STRING.
        write(
            &outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "n_genes_ranked": 17190.0,
                "n_genes_unmapped": "5160"
            })
            .to_string(),
        );
        // A count only present in reporting/result.json (broadened scan).
        write(
            &outputs,
            "reporting/result.json",
            &serde_json::json!({ "n_genes_total": 22369, "n_resolved": "17,209" }).to_string(),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "22,369 genes; 17,190 successfully mapped; 5,160 unmapped; \
             17,209 resolved, 5,160 unresolved.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-4"));
        assert!(
            report.passed(),
            "float/string/reporting-dir-sourced counts must not false-block RP-4: {report:?}"
        );
    }

    /// The real back-computed `5,179` must STILL be flagged even after the
    /// sourced scan is broadened to every stage `result.json`.
    #[test]
    fn rp4_backcomputed_still_flagged_after_broadening() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        // Full spread of stage outputs (broadened scan reads them all), none
        // of which contains 5179.
        write(
            &outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({ "n_genes_ranked": 17190.0, "n_genes_unmapped": 5160 }).to_string(),
        );
        write(
            &outputs,
            "contextualize_findings_with_literature/result.json",
            &serde_json::json!({ "resolved": 17209, "unresolved": 5160, "total": 22369 })
                .to_string(),
        );
        write(
            &outputs,
            "reporting/result.json",
            &serde_json::json!({ "n_genes": 22369 }).to_string(),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "22,369 genes; 17,190 successfully mapped; 5,179 unmapped.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-4"));
        assert!(
            !report.passed(),
            "back-computed 5,179 must still block: {report:?}"
        );
        assert!(
            report
                .required_failures()
                .iter()
                .any(|f| f.contains("RP-4") && f.contains("5179")),
            "RP-4 must still name the narrative-only 5,179: {:?}",
            report.required_failures()
        );
    }

    // -- RP-2 over-block guards ------------------------------------------

    /// Case- and separator-varied collection labels between
    /// `pathway_summary.json` keys and the TSV `collection` column must NOT
    /// cause a REQUIRED failure when the counts actually agree.
    #[test]
    fn rp2_case_varied_labels_do_not_false_block() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        // TSV uses lowercase/underscore labels.
        write(
            &outputs,
            "pathway_enrichment/pathway_results.tsv",
            &pathway_results_tsv(&[("hallmark", 2), ("go_bp", 3)]),
        );
        // Summary uses upper-case + hyphen/space separators, same counts.
        write(
            &outputs,
            "pathway_enrichment/pathway_summary.json",
            &serde_json::json!({
                "gene_sets_tested": { "HALLMARK": 2, "GO-BP": 3, "total": 5 }
            })
            .to_string(),
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-2"));
        assert!(
            report.passed(),
            "a pure label-format difference (counts agree) must not block RP-2: {report:?}"
        );
    }

    /// A reported collection with no TSV counterpart after normalization is
    /// unverifiable — skipped, not a REQUIRED failure — while the TOTAL row
    /// count remains the hard REQUIRED check.
    #[test]
    fn rp2_unmatched_collection_is_skipped_total_still_checked() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "pathway_enrichment/pathway_results.tsv",
            &pathway_results_tsv(&[("HALLMARK", 2), ("GO_BP", 3)]),
        );
        // MYSTERY has no TSV rows: unverifiable-skip. total agrees (5).
        write(
            &outputs,
            "pathway_enrichment/pathway_summary.json",
            &serde_json::json!({
                "gene_sets_tested": { "HALLMARK": 2, "GO_BP": 3, "MYSTERY": 99, "total": 5 }
            })
            .to_string(),
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-2"));
        assert!(
            report.passed(),
            "an unmatched (unverifiable) collection label must not block; total agrees: {report:?}"
        );

        // But a wrong TOTAL is still a hard REQUIRED failure.
        write(
            &outputs,
            "pathway_enrichment/pathway_summary.json",
            &serde_json::json!({
                "gene_sets_tested": { "HALLMARK": 2, "GO_BP": 3, "MYSTERY": 99, "total": 10085 }
            })
            .to_string(),
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            !report.passed(),
            "a wrong TOTAL rowcount must still be a REQUIRED failure: {report:?}"
        );
        assert!(
            report
                .required_failures()
                .iter()
                .any(|f| f.contains("RP-2") && f.contains("total") && f.contains("10085")),
            "RP-2 total mismatch must still be named: {:?}",
            report.required_failures()
        );
    }

    // -- RP-5 over-block guards ------------------------------------------

    /// A truthful PROVENANCE mention ("derived from N samples") is not a
    /// claim about the figure's data shape and must NOT block deposit.
    #[test]
    fn rp5_provenance_sample_mention_does_not_false_block() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "differential_expression/figures/top_features_heatmap.png",
            "PNG",
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "- **top_features_heatmap** (differential_expression): single-column log2FC \
             of the top DE genes, derived from 8 samples.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-5"));
        assert!(
            report.passed(),
            "a provenance-only 'derived from 8 samples' mention must not block RP-5: {report:?}"
        );
    }

    /// The real "expression heatmap … across 8 samples" SHAPE assertion must
    /// STILL be flagged after the provenance-vs-assertion split.
    #[test]
    fn rp5_shape_assertion_still_flagged() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "differential_expression/figures/top_features_heatmap.png",
            "PNG",
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "- **top_features_heatmap** (differential_expression): expression heatmap of \
             top DE genes across 8 samples.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-5"));
        assert!(
            !report.passed(),
            "an 'across 8 samples' shape claim must still block: {report:?}"
        );
        assert!(
            report
                .required_failures()
                .iter()
                .any(|f| f.contains("RP-5") && f.contains('8')),
            "RP-5 must still name the false 8-sample claim: {:?}",
            report.required_failures()
        );
    }

    /// The "N-sample expression heatmap" phrasing is also a shape assertion.
    #[test]
    fn rp5_n_sample_heatmap_phrasing_flagged() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "differential_expression/figures/top_features_heatmap.png",
            "PNG",
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "- **top_features_heatmap**: an 8-sample expression heatmap of the top DE genes.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            !report.passed(),
            "'8-sample expression heatmap' must block: {report:?}"
        );
    }

    // -- vacuity ----------------------------------------------------------

    #[test]
    fn empty_package_runs_nothing_and_passes() {
        let tmp = TempDir::new().unwrap();
        outputs_dir(&tmp);
        let report = check_reporting_invariants(tmp.path());
        assert!(
            report.checked.is_empty(),
            "no inputs → no invariant runs: {report:?}"
        );
        assert!(report.passed());
        assert!(report.warnings().is_empty());
    }

    #[test]
    fn parse_grouped_int_handles_thousands_separators() {
        assert_eq!(parse_grouped_int("5,179"), Some(5179));
        assert_eq!(parse_grouped_int("17190"), Some(17190));
        assert_eq!(parse_grouped_int("nope"), None);
    }

    // -- RC-COUNT / RC-IDENTITY / RC-SECTIONS ------------------------------

    /// A synthetic signed result table (`gene\tlog2FoldChange\tpadj`) with
    /// exactly `up + down` significant (`padj < 0.05`) rows — `up` with a
    /// positive `log2FoldChange`, `down` with a negative one — plus `nonsig`
    /// additional non-significant (`padj = 0.9`) padding rows.
    fn signed_result_tsv(up: usize, down: usize, nonsig: usize) -> String {
        let mut s = String::from("gene\tlog2FoldChange\tpadj\n");
        for i in 0..up {
            s.push_str(&format!("GUP{i}\t2.0\t0.001\n"));
        }
        for i in 0..down {
            s.push_str(&format!("GDOWN{i}\t-2.0\t0.001\n"));
        }
        for i in 0..nonsig {
            s.push_str(&format!("GNS{i}\t0.01\t0.9\n"));
        }
        s
    }

    fn signed_de_schema_json() -> serde_json::Value {
        serde_json::json!({
            "artifact": "de_results.tsv",
            "entity_column": "gene",
            "significance": { "column": "padj", "threshold": 0.05, "comparator": "lt" },
            "signed_effect_column": "log2FoldChange"
        })
    }

    /// Writes a minimal `WORKFLOW.json` at `root` carrying, when present,
    /// the `assemble_report_data` task's `spec.report_schemas` and the
    /// `reporting` task's `spec.required_report_sections` — the two fields
    /// `read_report_schemas`/`read_required_report_sections` resolve.
    fn write_workflow_json(
        root: &Path,
        report_schemas: Option<serde_json::Value>,
        required_sections: Option<&[&str]>,
    ) {
        let mut tasks = serde_json::Map::new();
        if let Some(schemas) = report_schemas {
            tasks.insert(
                "assemble_report_data".to_string(),
                serde_json::json!({ "spec": { "report_schemas": schemas } }),
            );
        }
        if let Some(sections) = required_sections {
            tasks.insert(
                "reporting".to_string(),
                serde_json::json!({ "spec": { "required_report_sections": sections } }),
            );
        }
        let wf = serde_json::json!({ "tasks": tasks });
        std::fs::write(root.join("WORKFLOW.json"), wf.to_string()).unwrap();
    }

    /// Writes `runtime/outputs/reporting/report-data.json` carrying exactly
    /// one artifact summary — the minimal shape RC-COUNT/RC-IDENTITY read.
    fn write_report_data(
        outputs: &Path,
        stage_id: &str,
        artifact: &str,
        n_total: u64,
        n_significant: Option<u64>,
        direction_split: Option<(u64, u64)>,
    ) {
        let split = direction_split.map(|(up, down)| serde_json::json!({ "up": up, "down": down }));
        let payload = serde_json::json!({
            "artifacts": [{
                "stage_id": stage_id,
                "artifact": artifact,
                "n_total": n_total,
                "n_significant": n_significant,
                "direction_split": split,
                "effect_distribution": null,
                "significant_entities": [],
                "significant_table_path": "",
                "full_table_path": "",
                "spilled_to_attachment_only": false
            }],
            "literature": null
        });
        write(outputs, "reporting/report-data.json", &payload.to_string());
    }

    #[test]
    fn rc_count_flags_headline_that_disagrees_with_source() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        // Source recomputes to 4017 significant rows (2209 up + 1808 down).
        write(
            &outputs,
            "differential_expression/de_results.tsv",
            &signed_result_tsv(2209, 1808, 0),
        );
        write_workflow_json(
            tmp.path(),
            Some(serde_json::json!({ "differential_expression": signed_de_schema_json() })),
            None,
        );
        // report-data.json states the WRONG headline count (3993, not 4017).
        write_report_data(
            &outputs,
            "differential_expression",
            "de_results.tsv",
            4017,
            Some(3993),
            None,
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(
            report
                .required_failures()
                .iter()
                .any(|f| f.contains("RC-COUNT")),
            "a headline count disagreeing with the recomputed source must be a REQUIRED \
             failure: {report:?}"
        );
        assert!(!report.passed());
    }

    #[test]
    fn rc_identity_has_zero_tolerance() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        // direction_split sums to 4017 but n_significant states 3993 — off
        // by 24. No WORKFLOW.json / source table: isolates this test to
        // RC-IDENTITY (RC-COUNT has no schema map to run against).
        write_report_data(
            &outputs,
            "differential_expression",
            "de_results.tsv",
            4017,
            Some(3993),
            Some((2209, 1808)),
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(
            report
                .required_failures()
                .iter()
                .any(|f| f.contains("RC-IDENTITY")),
            "direction_split up+down disagreeing with n_significant must be a REQUIRED \
             failure, zero tolerance: {report:?}"
        );
    }

    #[test]
    fn rc_sections_flags_missing_required_section() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write_workflow_json(tmp.path(), None, Some(&["primary_results", "methods"]));
        // "methods" is present with content; "primary_results" never appears.
        write(
            &outputs,
            "final_reporting/final_report.md",
            "## Methods\n\nDESeq2 fixed-effects negative-binomial GLM (`~ cell + dex`).\n",
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(
            report
                .required_failures()
                .iter()
                .any(|f| f.contains("RC-SECTIONS")),
            "a missing required report section must be a REQUIRED failure: {report:?}"
        );
    }

    #[test]
    fn correct_comprehensive_report_passes() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "differential_expression/de_results.tsv",
            &signed_result_tsv(2209, 1808, 0),
        );
        write_workflow_json(
            tmp.path(),
            Some(serde_json::json!({ "differential_expression": signed_de_schema_json() })),
            Some(&["primary_results", "methods"]),
        );
        write_report_data(
            &outputs,
            "differential_expression",
            "de_results.tsv",
            4017,
            Some(4017),
            Some((2209, 1808)),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "## Primary Results\n\n4,017 genes were significant at padj < 0.05.\n\n\
             ## Methods\n\nDESeq2 fixed-effects negative-binomial GLM (`~ cell + dex`).\n",
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(
            report.passed(),
            "a fully consistent report must pass every RC-* check: {report:?}"
        );
        assert!(report.checked.contains(&"RC-COUNT"), "{report:?}");
        assert!(report.checked.contains(&"RC-IDENTITY"), "{report:?}");
        assert!(report.checked.contains(&"RC-SECTIONS"), "{report:?}");
    }

    #[test]
    fn rc_count_generalizes_to_unsigned_modality() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        // 50 rows with qual>30 (significant), 10 padding rows that aren't.
        let mut tsv = String::from("variant_id\tqual\n");
        for i in 0..50 {
            tsv.push_str(&format!("v{i}\t40\n"));
        }
        for i in 0..10 {
            tsv.push_str(&format!("ns{i}\t10\n"));
        }
        write(&outputs, "variant_calling/variants.tsv", &tsv);

        let unsigned_schema = serde_json::json!({
            "artifact": "variants.tsv",
            "entity_column": "variant_id",
            "significance": { "column": "qual", "threshold": 30, "comparator": "gt" }
        });
        write_workflow_json(
            tmp.path(),
            Some(serde_json::json!({ "variant_calling": unsigned_schema })),
            None,
        );
        write_report_data(&outputs, "variant_calling", "variants.tsv", 60, Some(50), None);

        let report = check_reporting_invariants(tmp.path());
        assert!(
            report.checked.contains(&"RC-COUNT"),
            "RC-COUNT must run for an unsigned (no direction_split) modality: {report:?}"
        );
        assert!(
            !report
                .required_failures()
                .iter()
                .any(|f| f.contains("RC-IDENTITY")),
            "an unsigned artifact with no direction_split must never trip RC-IDENTITY: {report:?}"
        );
        assert!(
            report.passed(),
            "a correctly-reported unsigned recompute must pass: {report:?}"
        );
    }

    // -- RC-SECTIONS robust matcher (F4) ----------------------------------

    #[test]
    fn rc_sections_matches_natural_heading_with_intervening_words() {
        // A natural heading whose words are separated by other words /
        // punctuation satisfies the required id — order-independent,
        // all-words-present, case-insensitive. The old consecutive-words
        // regex false-blocked these correct headings.
        assert_eq!(
            section_has_content(
                "## Provenance & Method-Selection Rationale\n\nThe aligner was chosen by the \
                 agent at runtime.\n",
                "provenance_method_rationale"
            ),
            Some(true)
        );
        assert_eq!(
            section_has_content(
                "## QC & Preprocessing\n\nAdapter trimming with fastp.\n",
                "qc_preprocessing"
            ),
            Some(true)
        );
    }

    #[test]
    fn rc_sections_missing_and_empty_still_fail() {
        // Genuinely-missing section → None (missing).
        assert_eq!(
            section_has_content("## Methods\n\nDESeq2 GLM.\n", "primary_results"),
            None
        );
        // Heading present but immediately followed by the next heading → empty.
        assert_eq!(
            section_has_content(
                "## Primary Results\n## Methods\n\nsome methods\n",
                "primary_results"
            ),
            Some(false)
        );
    }

    #[test]
    fn rc_sections_subheading_first_content_is_non_empty() {
        // A required `## Primary Results` whose FIRST content is a deeper
        // `### 3.1 ...` subheading is non-empty: the subheading is part of the
        // section (deeper level = content), so the span carries non-whitespace.
        // The old boundary (any `#`-line ends the section) false-flagged this
        // as "present but empty" and over-gated a complete deposit.
        assert_eq!(
            section_has_content(
                "## Primary Results\n### 3.1 Differential expression\n\n4030 genes at FDR<0.05.\n",
                "primary_results"
            ),
            Some(true)
        );
    }

    #[test]
    fn rc_sections_same_level_heading_immediately_after_is_empty() {
        // A `## X` immediately followed by a same-level `## Y` (no body between)
        // is empty — the equal-level heading ends the section.
        assert_eq!(
            section_has_content(
                "## Primary Results\n## Reproducibility\n\nSee the lockfile.\n",
                "primary_results"
            ),
            Some(false)
        );
    }

    #[test]
    fn rc_sections_direct_body_content_is_non_empty() {
        // A section whose content is direct body text (no subheading) stays
        // non-empty — the level-aware boundary preserves the ordinary case.
        assert_eq!(
            section_has_content(
                "## Primary Results\n\nDESeq2 identified 4030 significant genes.\n",
                "primary_results"
            ),
            Some(true)
        );
    }

    #[test]
    fn rc_sections_prose_mention_does_not_anchor() {
        // A prose line containing the id's words is NOT a heading, so it can
        // never anchor the section (kills the old false-anchor on a mention
        // that preceded the real heading). No `#`-heading → missing.
        assert_eq!(
            section_has_content(
                "The primary results are summarized in the paragraphs that follow.\n\nProse only.\n",
                "primary_results"
            ),
            None
        );
    }

    // -- RC-IDENTITY <= (F6) ----------------------------------------------

    #[test]
    fn rc_identity_allows_up_plus_down_below_n_significant() {
        // A correct report: 100 significant rows (padj passes), but 3 of them
        // have a zero/NA signed effect, so they count in neither up nor down.
        // up+down = 97 <= 100 → must NOT trip RC-IDENTITY. No WORKFLOW.json /
        // source table, so this isolates the check to RC-IDENTITY.
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write_report_data(
            &outputs,
            "differential_expression",
            "de_results.tsv",
            200,
            Some(100),
            Some((60, 37)),
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            report.checked.contains(&"RC-IDENTITY"),
            "RC-IDENTITY must run when a direction_split with n_significant is present: {report:?}"
        );
        assert!(
            !report
                .required_failures()
                .iter()
                .any(|f| f.contains("RC-IDENTITY")),
            "up+down (97) < n_significant (100) is a legitimate shortfall (zero/NA-effect \
             significant rows) and must NOT trip RC-IDENTITY: {report:?}"
        );
    }
}
