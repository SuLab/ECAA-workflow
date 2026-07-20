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
    cleaned.parse().ok()
}

/// Recursively collect every non-negative integer value in a JSON tree
/// into `out`. Used to build the set of stage-output-sourced gene counts
/// RP-4 reconciles a report's mapping claims against.
fn collect_ints(value: &Value, out: &mut BTreeSet<u64>) {
    match value {
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                out.insert(u);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_ints(v, out)),
        Value::Object(map) => map.values().for_each(|v| collect_ints(v, out)),
        _ => {}
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

    // Compare each reported entry (sorted for deterministic output) against
    // the recomputed value; `total` uses the row total, others the
    // per-collection rowcount.
    let mut mismatches: Vec<String> = Vec::new();
    for (key, reported_val) in reported {
        let Some(reported_n) = reported_val.as_u64() else {
            continue;
        };
        let actual = if key == "total" {
            recomputed_total
        } else {
            recomputed.get(key).copied().unwrap_or(0)
        };
        if reported_n != actual {
            mismatches.push(format!(
                "{key}: reported {reported_n} vs recomputed {actual}"
            ));
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
    // Build the set of stage-output-sourced integers from the mapping-bearing
    // stage outputs. Widening this set is the conservative direction for a
    // REQUIRED gate (fewer false-positive blocks).
    let mut sourced: BTreeSet<u64> = BTreeSet::new();
    for rel in [
        "pathway_enrichment/pathway_summary.json",
        "pathway_enrichment/result.json",
        "contextualize_findings_with_literature/result.json",
        "differential_expression/result.json",
    ] {
        if let Some(v) = read_json(&outputs.join(rel)) {
            collect_ints(&v, &mut sourced);
        }
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
/// per-sample expression matrix. A caption asserting it shows "N samples"
/// misrepresents its data shape — the deposited report captioned it as an
/// "expression heatmap … across 8 samples". Gated on the figure actually
/// being present so it only fires for packages that render it.
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
    let re_samples = Regex::new(r"(\d[\d,]*)\s+samples?\b").expect("static RP-5 regex compiles");
    for line in reports.lines() {
        if !line.contains("top_features_heatmap") {
            continue;
        }
        let Some(cap) = re_samples.captures(line) else {
            continue;
        };
        let contrast_note = contrast
            .as_deref()
            .map(|c| format!(" (its single column is the {c} log2FC)"))
            .unwrap_or_default();
        report.findings.push(ReportingFinding {
            invariant: "RP-5",
            severity: Severity::Required,
            detail: format!(
                "caption for top_features_heatmap asserts a {}-sample expression matrix, but \
                 the figure is a single-column log2FC heatmap{contrast_note}, not a per-sample \
                 expression matrix",
                &cap[1]
            ),
        });
        break;
    }
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

/// Warn when the report labels the DE model a "linear mixed model"; the
/// executed model is a fixed-effects negative-binomial GLM (DESeq2
/// `~ cell + dex`). Warn-only per §G-C1.
fn check_rp9_method_label(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(reports) = read_reports(outputs) else {
        return;
    };
    report.checked.push("RP-9");

    let lower = reports.to_lowercase();
    if lower.contains("linear mixed model")
        || lower.contains("linear mixed-effects")
        || lower.contains("linear mixed effects")
    {
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
}
