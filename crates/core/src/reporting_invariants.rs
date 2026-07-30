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
//!   * **RC-RANK** the retained enrichment ranking has the declared schema,
//!     one unique label per row, sequential ranks, finite scores, and the same
//!     row count as `n_genes_ranked` and any narrative claim that explicitly
//!     describes the vector supplied to enrichment.
//!   * **RC-COLLECTION** collection labels in pathway metadata must equal the
//!     labels in `pathway_results.tsv`; provider-specific source names belong
//!     in separate provenance fields.
//!   * **RC-STAGE-NARRATIVE** a pathway stage narrative that names a top
//!     enriched or depleted row must copy that row's NES and adjusted p-value
//!     from `pathway_results.tsv` within the package policy's tolerances.
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
//!   * **RC-LITERATURE** every literature entity count in `report-data.json`,
//!     the contextualization result, and any explicitly named narrative claim
//!     must equal a fresh recomputation from `claims_evidence_matrix.csv`.
//!     Evidence-row counts stay separate from distinct-entity counts, and
//!     missing entity labels fall back to the row's finding identifier.
//!   * **RC-IDENTITY** a `direction_split`'s `up + down` must not EXCEED
//!     `n_significant` (directional rows can't outnumber the significant
//!     set). A shortfall is legitimate — a significant row with a zero/NA
//!     effect counts in `n_significant` but in neither `up` nor `down`.
//!     Artifacts with no split (unsigned modalities, e.g. variant calling)
//!     are skipped, never faulted.
//!   * **RC-SECTIONS** every `required_report_sections` id declared on the
//!     `reporting`/`final_reporting` task specs must appear as a non-empty
//!     section in the emitted report.
//!   * **RC-ROW** every DATA ROW of a markdown table in the narrative must be
//!     re-derivable from the source artifact the table transcribes: its
//!     identifier must be a row of that artifact, and every cell whose column
//!     resolved to a role (effect / significance, via the one
//!     [`crate::report_contract::resolve_ranking_columns`] resolver) must match
//!     the source cell within the transcription tolerance the package's own
//!     `interpretation-policy.json` declares. A deposited report shipped a
//!     "Top 10 depleted pathways" table in which three terms existed in no
//!     source table at all and a fourth was reported at an INVERTED effect and
//!     a significant adjusted p when its real row was the opposite sign and not
//!     significant; RC-COUNT (JSON-only), RC-TABLE (presence-only) and
//!     RC-SECTIONS (headings-only) all pass such a report, and the only thing
//!     that caught it was a per-run agent-authored script that had
//!     hand-transcribed the rows as literals. A table whose columns do not
//!     resolve, or whose source artifact cannot be identified from the table's
//!     own contents, is SKIPPED with a warning — never a required failure,
//!     because a false positive here blocks a deposit. Ordering claims in a
//!     caption ("Top 10 …") are disclosed as unverified rather than
//!     re-derived; see [`check_rc_row`] for why.
//!   * **RC-TABLE** every significant entity embedded in `report-data.json`
//!     (for an artifact whose set is not `spilled_to_attachment_only`) must
//!     be rendered in the terminal report — the deterministic backstop for
//!     the otherwise prompt-only "inline the full significant table"
//!     obligation, so a summarized report can't silently ship an incomplete
//!     significant set that RC-COUNT (JSON-only) and RC-SECTIONS (headings)
//!     both miss.
//!   * **RP-PROV** every bibliographic / data-source assertion the narrative
//!     makes (journal, DOI, PMID, accession, "supplied by the SME" /
//!     local-copy phrasing) must be consistent with the package's OWN
//!     acquisition record — `per_accession_summary.json` plus
//!     `runtime/inputs.json`. A deposited report asserted an SME-supplied
//!     local copy that was never registered (no `runtime/inputs.json`; the
//!     stage actually read a Bioconductor data package) and cited it to the
//!     wrong journal, while the package's own record carried the correct
//!     journal, DOI and PMID. The system-owned provenance block
//!     ([`crate::report_contract::provenance_section`]) is excluded from the
//!     scan — it is the reference, not an assertion under test.
//!   * **RP-QC** an unqualified QC-negative assertion ("no outlier samples
//!     were identified") requires a RETAINED outlier / PCA / sample-distance
//!     artifact in the package. The deposited report asserted the absence of
//!     outliers while the package contained no sample-level QC artifact of
//!     any kind — the only sample statistic was a size-factor range.
//! * **Warn-only** — free-text prose invariants, so a brittle regex can
//!   never block a scientifically-correct deposit:
//!   * **RP-1** effect-abundance direction word (derived structurally from
//!     the sign of `top_effect_abundance_ratio`, and ANCHORED to the clause
//!     that actually describes that statistic so a stray direction word
//!     elsewhere in the narrative — a sign-convention sentence, say — is not
//!     read as an abundance claim), plus the prose that
//!     DESCRIBES that statistic: it is a ratio of the MEDIAN abundance of the
//!     top-K features over the median of the whole tested set, so narrating
//!     it as a "mean"/"average" or attributing it to N *samples* (rather than
//!     to features) misstates what was computed.
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
use unicode_normalization::UnicodeNormalization;

use crate::report_contract::provenance_section::{
    collect_data_provenance, strip_provenance_section, DataProvenance, DataProvenanceRecord,
};
use crate::report_contract::{
    load_policy_column_synonyms, resolve_ranking_columns, summarize_artifact, PathwayRanking,
    PolicyColumnSynonyms, RankingColumns, ReportData, ResultSchema, FULL_TABLE_END,
    FULL_TABLE_START,
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

    check_rp1_effect_direction(package_root, &outputs, &mut report);
    check_rp2_gene_sets_tested(&outputs, &mut report);
    check_rc_pathway_collections(&outputs, &mut report);
    check_rc_pathway_rank(&outputs, &mut report);
    check_rc_pathway_stage_narrative(package_root, &outputs, &mut report);
    check_rp3_fdr_family(&outputs, &mut report);
    check_rp4_mapping_reconciliation(&outputs, &mut report);
    check_rp5_figure_caption_shape(&outputs, &mut report);
    check_rp9_method_label(&outputs, &mut report);
    check_rp_prov_data_source(package_root, &outputs, &mut report);
    check_rp_qc_negative_claim(&outputs, &mut report);
    check_rc_count(package_root, &outputs, &mut report);
    check_rc_literature_counts(&outputs, &mut report);
    check_rc_identity(&outputs, &mut report);
    check_rc_sections(package_root, &outputs, &mut report);
    check_rc_table(&outputs, &mut report);
    check_rc_row(package_root, &outputs, &mut report);

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

/// The AGENT-authored report prose: [`read_reports`] with the system-owned
/// data-provenance block removed. A narrative scanner must never read the
/// block the system itself injected as an assertion under test — it is the
/// reference the assertions are checked against.
fn read_agent_report_prose(outputs: &Path) -> Option<String> {
    read_reports(outputs).map(|text| strip_provenance_section(&text))
}

/// A byte-bounded slice of `text` around `byte_at`, snapped outward to char
/// boundaries so a multi-byte character can never split the window.
fn byte_window(text: &str, byte_at: usize, before: usize, after: usize) -> &str {
    let mut start = byte_at.saturating_sub(before);
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = byte_at.saturating_add(after).min(text.len());
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }
    &text[start..end]
}

/// Byte offsets of every occurrence of `needle` in `haystack`.
fn find_all(haystack: &str, needle: &str) -> Vec<usize> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(needle) {
        let at = from + rel;
        out.push(at);
        from = at + needle.len();
    }
    out
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

fn json_string_set(value: &Value, key: &str) -> Option<BTreeSet<String>> {
    value.get(key)?.as_array().map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

/// RC-COLLECTION: metadata labels identify the exact groups retained in the
/// source table. A provider subcollection name is useful provenance, but it
/// cannot replace a different table label in a field that claims to enumerate
/// the table's collections.
fn check_rc_pathway_collections(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let pathway_dir = outputs.join("pathway_enrichment");
    let Ok((headers, rows)) =
        crate::report_contract::assemble::read_table(&pathway_dir.join("pathway_results.tsv"))
    else {
        return;
    };
    let Some(collection_idx) = headers.iter().position(|name| name == "collection") else {
        return;
    };
    let expected: BTreeSet<String> = rows
        .iter()
        .filter_map(|row| row.get(collection_idx))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();
    if expected.is_empty() {
        return;
    }
    report.checked.push("RC-COLLECTION");

    let sources = [
        (
            "result.json::gene_sets_collections",
            read_json(&pathway_dir.join("result.json"))
                .and_then(|value| json_string_set(&value, "gene_sets_collections")),
        ),
        (
            "pathway_summary.json::collections",
            read_json(&pathway_dir.join("pathway_summary.json"))
                .and_then(|value| json_string_set(&value, "collections")),
        ),
    ];
    let mut mismatches = Vec::new();
    for (source, observed) in sources {
        match observed {
            Some(observed) if observed == expected => {}
            Some(observed) => mismatches.push(format!(
                "{source}={:?}, pathway_results.tsv={expected:?}",
                observed
            )),
            None => mismatches.push(format!("{source} is missing or not a string array")),
        }
    }
    if !mismatches.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-COLLECTION",
            severity: Severity::Required,
            detail: format!(
                "pathway collection metadata does not match the retained source table: {}",
                mismatches.join("; ")
            ),
        });
    }
}

fn narrative_rank_count_claims(text: &str, expected: u64) -> Vec<u64> {
    let explicit = Regex::new(r"(?i)`?n_genes_ranked`?[^0-9]{0,64}([0-9][0-9,]*)")
        .expect("static RC-RANK field regex compiles");
    let gene_count = Regex::new(r"(?i)([0-9][0-9,]*)\s+(?:[a-z0-9_-]+\s+){0,6}genes?\b")
        .expect("static RC-RANK gene-count regex compiles");
    let mut mismatches = Vec::new();
    for capture in explicit.captures_iter(text) {
        if let Some(observed) = capture.get(1).and_then(|m| parse_grouped_int(m.as_str())) {
            if observed != expected {
                mismatches.push(observed);
            }
        }
    }
    for capture in gene_count.captures_iter(text) {
        let Some(matched) = capture.get(0) else {
            continue;
        };
        let clause = clause_around(text, matched.start()).to_ascii_lowercase();
        let enrichment_context = ["fgsea", "gsea", "wald", "ranking", "ranked"]
            .iter()
            .any(|term| clause.contains(term));
        let final_input_claim = [
            " included",
            " ranked",
            "supplied to",
            "used for",
            "run on",
            "ranking vector",
        ]
        .iter()
        .any(|term| clause.contains(term));
        if !enrichment_context || !final_input_claim {
            continue;
        }
        if let Some(observed) = capture.get(1).and_then(|m| parse_grouped_int(m.as_str())) {
            if observed != expected {
                mismatches.push(observed);
            }
        }
    }
    mismatches.sort_unstable();
    mismatches.dedup();
    mismatches
}

/// RC-RANK: validate the deposit-retained vector actually supplied to the
/// enrichment method, then bind its row count to structured metadata and
/// unambiguous narrative claims about the final ranking population.
fn check_rc_pathway_rank(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let pathway_dir = outputs.join("pathway_enrichment");
    let rank_path = pathway_dir.join("ranked_genes.tsv");
    if !rank_path.exists() {
        return;
    }
    report.checked.push("RC-RANK");

    let Ok((headers, rows)) = crate::report_contract::assemble::read_table(&rank_path) else {
        report.findings.push(ReportingFinding {
            invariant: "RC-RANK",
            severity: Severity::Required,
            detail: "ranked_genes.tsv cannot be parsed as a delimited table".to_string(),
        });
        return;
    };
    let required = ["rank", "gene_label", "source_id", "ranking_score"];
    let missing: Vec<&str> = required
        .iter()
        .copied()
        .filter(|name| !headers.iter().any(|header| header == *name))
        .collect();
    if !missing.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-RANK",
            severity: Severity::Required,
            detail: format!(
                "ranked_genes.tsv is missing required column(s): {}",
                missing.join(", ")
            ),
        });
        return;
    }
    let rank_idx = headers.iter().position(|name| name == "rank").unwrap_or(0);
    let label_idx = headers
        .iter()
        .position(|name| name == "gene_label")
        .unwrap_or(0);
    let source_idx = headers
        .iter()
        .position(|name| name == "source_id")
        .unwrap_or(0);
    let score_idx = headers
        .iter()
        .position(|name| name == "ranking_score")
        .unwrap_or(0);

    let mut structural = Vec::new();
    let mut labels = BTreeSet::new();
    for (offset, row) in rows.iter().enumerate() {
        let expected_rank = offset + 1;
        if row
            .get(rank_idx)
            .and_then(|value| value.trim().parse::<usize>().ok())
            != Some(expected_rank)
        {
            structural.push(format!(
                "row {} does not carry rank {expected_rank}",
                offset + 2
            ));
        }
        let label = row.get(label_idx).unwrap_or("").trim();
        if label.is_empty() {
            structural.push(format!("row {} has an empty gene_label", offset + 2));
        } else if !labels.insert(label.to_string()) {
            structural.push(format!("gene_label {label:?} appears more than once"));
        }
        if row.get(source_idx).unwrap_or("").trim().is_empty() {
            structural.push(format!("row {} has an empty source_id", offset + 2));
        }
        let finite_score = row
            .get(score_idx)
            .and_then(|value| value.trim().parse::<f64>().ok())
            .is_some_and(f64::is_finite);
        if !finite_score {
            structural.push(format!("row {} has a non-finite ranking_score", offset + 2));
        }
    }

    let expected_count = rows.len() as u64;
    let result = read_json(&pathway_dir.join("result.json"));
    match result
        .as_ref()
        .and_then(|value| json_named_u64(value, "n_genes_ranked"))
    {
        Some(observed) if observed == expected_count => {}
        Some(observed) => structural.push(format!(
            "result.json n_genes_ranked={observed}, ranked_genes.tsv rows={expected_count}"
        )),
        None => structural.push("result.json n_genes_ranked is missing".to_string()),
    }

    let mut narrative = read_reports(outputs).unwrap_or_default();
    if let Some(stage_narrative) = result
        .as_ref()
        .and_then(|value| value.get("narrative"))
        .and_then(Value::as_str)
    {
        narrative.push('\n');
        narrative.push_str(stage_narrative);
    }
    for observed in narrative_rank_count_claims(&narrative, expected_count) {
        structural.push(format!(
            "narrative final-ranking count={observed}, ranked_genes.tsv rows={expected_count}"
        ));
    }

    if !structural.is_empty() {
        structural.sort();
        structural.dedup();
        report.findings.push(ReportingFinding {
            invariant: "RC-RANK",
            severity: Severity::Required,
            detail: structural.join("; "),
        });
    }
}

/// RC-STAGE-NARRATIVE: bind explicit top-pathway claims in the stage's own
/// structured narrative to the retained pathway table. Final-report tables are
/// covered separately by RC-ROW; this closes the same gap for `result.json`.
fn check_rc_pathway_stage_narrative(
    package_root: &Path,
    outputs: &Path,
    report: &mut ReportingInvariantsReport,
) {
    let pathway_dir = outputs.join("pathway_enrichment");
    let Some(result) = read_json(&pathway_dir.join("result.json")) else {
        return;
    };
    let Some(narrative) = result.get("narrative").and_then(Value::as_str) else {
        return;
    };
    let Some(tolerances) = NarrativeTolerances::load(package_root) else {
        return;
    };
    let Ok((headers, rows)) =
        crate::report_contract::assemble::read_table(&pathway_dir.join("pathway_results.tsv"))
    else {
        return;
    };
    let Some(entity_idx) = headers.iter().position(|name| name == "pathway") else {
        return;
    };
    let Some(effect_idx) = headers
        .iter()
        .position(|name| name.eq_ignore_ascii_case("NES"))
    else {
        return;
    };
    let Some(significance_idx) = headers
        .iter()
        .position(|name| name.eq_ignore_ascii_case("padj"))
    else {
        return;
    };
    let source: BTreeMap<String, (f64, f64)> = rows
        .iter()
        .filter_map(|row| {
            Some((
                row.get(entity_idx)?.trim().to_string(),
                (
                    row.get(effect_idx)?.trim().parse().ok()?,
                    row.get(significance_idx)?.trim().parse().ok()?,
                ),
            ))
        })
        .collect();
    let re = Regex::new(
        r"(?ix)
          top\s+(?:enriched|depleted)[^.]{0,240}?
          \b([A-Z][A-Z0-9_]{2,})\s*
          \(\s*NES\s*=\s*
          ([+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:e[+-]?[0-9]+)?)
          \s*,\s*padj\s*=\s*
          ([+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:e[+-]?[0-9]+)?)
          \s*\)",
    )
    .expect("static RC-STAGE-NARRATIVE regex compiles");

    let effect_role = RoleCell {
        narrative: 0,
        source: 0,
        significance: false,
    };
    let significance_role = RoleCell {
        narrative: 0,
        source: 0,
        significance: true,
    };
    let mut mismatches = Vec::new();
    let mut ran = false;
    for capture in re.captures_iter(narrative) {
        ran = true;
        let entity = capture.get(1).map(|m| m.as_str()).unwrap_or("");
        let claimed_effect = capture.get(2).and_then(|m| m.as_str().parse::<f64>().ok());
        let claimed_significance = capture.get(3).and_then(|m| m.as_str().parse::<f64>().ok());
        let Some((observed_effect, observed_significance)) = source.get(entity).copied() else {
            mismatches.push(format!("{entity} is absent from pathway_results.tsv"));
            continue;
        };
        if claimed_effect
            .is_none_or(|claimed| !tolerances.agrees(&effect_role, claimed, observed_effect))
        {
            mismatches.push(format!(
                "{entity} NES={} vs source {observed_effect}",
                capture.get(2).map(|m| m.as_str()).unwrap_or("<missing>")
            ));
        }
        if claimed_significance.is_none_or(|claimed| {
            !tolerances.agrees(&significance_role, claimed, observed_significance)
        }) {
            mismatches.push(format!(
                "{entity} padj={} vs source {observed_significance}",
                capture.get(3).map(|m| m.as_str()).unwrap_or("<missing>")
            ));
        }
    }
    if !ran {
        return;
    }
    report.checked.push("RC-STAGE-NARRATIVE");
    if !mismatches.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-STAGE-NARRATIVE",
            severity: Severity::Required,
            detail: format!(
                "pathway result narrative disagrees with pathway_results.tsv: {}",
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

/// The authoritative definition of `top_effect_abundance_ratio`, kept verbatim
/// in one place so the finding text and the agent prompt cannot drift apart.
/// It is the MEDIAN abundance of the top-K-by-|effect| features over the
/// MEDIAN abundance of the whole tested set — a median-over-FEATURES ratio,
/// not a mean and not a per-sample statistic.
pub const EFFECT_ABUNDANCE_RATIO_DEFINITION: &str =
    "the median abundance of the top-K-by-|effect| features divided by the median abundance of \
     the whole tested set (a median/median ratio over FEATURES)";

/// Warn-only, but derived structurally from the SIGN of the computed
/// `top_effect_abundance_ratio` rather than from a free-text regex over the
/// prose: a ratio < 1 means the top effects sit BELOW background, so
/// narrative that calls them "above" is inverted (the deposited report did
/// exactly this at ratio 0.558). Left warn-only per §G-C1 so a prose
/// mismatch cannot block an otherwise-correct deposit.
///
/// The direction test is ANCHORED: a direction word only counts when it
/// occurs in the same clause as a citation of the statistic or a mention of
/// the abundance basis it is computed over ([`effect_abundance_anchors`],
/// the same anchoring [`check_effect_ratio_prose`] uses). Unanchored, a
/// bag-of-words scan of the whole narrative reads any stray "higher"
/// anywhere in the text as an abundance claim: a deposited run tripped this
/// on the sign-convention sentence "positive log2FC = higher in treated" —
/// a statement about which direction a positive effect means, in a narrative
/// that never mentions abundance at all — while the report it accompanied
/// stated the direction correctly. Both surfaces are scanned (the stage
/// narrative AND the emitted report), because the abundance sentence
/// normally lives in the report, not in the stage's `result.json`.
///
/// The same check also guards how the statistic is DESCRIBED, in the stage
/// narrative and in the emitted report: the deposited report called it an
/// "average normalized count ratio" / a "mean baseMean" and attributed it to
/// "the 15 samples" — reusing the top-K FEATURE count as a sample count in a
/// run with 8 samples. Both are misstatements of a median/median ratio over
/// features, so a `mean`/`average` word or an `across N samples` attribution
/// adjacent to the statistic warns.
fn check_rp1_effect_direction(
    package_root: &Path,
    outputs: &Path,
    report: &mut ReportingInvariantsReport,
) {
    let Some(de) = read_json(&outputs.join("differential_expression").join("result.json")) else {
        return;
    };
    let Some(ratio) = de.get("top_effect_abundance_ratio").and_then(Value::as_f64) else {
        return;
    };
    report.checked.push("RP-1");

    if let Some(narrative) = de.get("narrative_text").and_then(Value::as_str) {
        check_effect_direction_claim(
            narrative,
            ratio,
            "the differential-expression stage narrative",
            report,
        );
    }
    if let Some(prose) = read_agent_report_prose(outputs) {
        check_effect_direction_claim(&prose, ratio, "the emitted report", report);
    }

    check_effect_ratio_prose(package_root, outputs, ratio, report);
}

/// Direction words that place the top-effect abundance ABOVE the whole-set
/// median, and their BELOW counterparts. Compared against a single clause, not
/// the whole document (see [`check_effect_direction_claim`]).
const EFFECT_DIRECTION_ABOVE: &[&str] = &["above", "higher", "greater", "exceed"];
/// The BELOW counterparts of [`EFFECT_DIRECTION_ABOVE`].
const EFFECT_DIRECTION_BELOW: &[&str] = &["below", "lower", "less than", "beneath"];

/// Words that name the ABUNDANCE BASIS `top_effect_abundance_ratio` is computed
/// over, so a direction word next to one of them is describing that statistic.
/// Modality-neutral: this is the general abundance/information-basis vocabulary
/// the validation contract itself enumerates for the recomputed ratio ("base
/// mean / mean count / mean expression / logCPM"), not a per-modality result
/// vocabulary — nothing here names a feature type, an effect column, or a
/// statistical method.
const EFFECT_ABUNDANCE_NOUNS: &[&str] = &[
    "abundance",
    "abundant",
    "background",
    "base mean",
    "basemean",
    "mean count",
    "mean expression",
    "expression level",
    "logcpm",
];

/// The clause-terminating punctuation [`clause_around`] splits on. A `.` only
/// terminates when the next byte is whitespace or the text ends, so a decimal
/// point inside a cited value ("0.208") never splits a clause.
fn is_clause_terminator(bytes: &[u8], at: usize) -> bool {
    match bytes[at] {
        b'\n' | b'\r' => true,
        b'.' | b';' | b'!' | b'?' => bytes.get(at + 1).is_none_or(u8::is_ascii_whitespace),
        _ => false,
    }
}

/// The clause containing `byte_at`: the span between the nearest clause
/// terminators either side. Every split point is an ASCII byte, so the returned
/// slice can never straddle a char boundary.
fn clause_around(text: &str, byte_at: usize) -> &str {
    let at = byte_at.min(text.len());
    let bytes = text.as_bytes();
    let mut start = 0;
    for i in (0..at).rev() {
        if is_clause_terminator(bytes, i) {
            start = i + 1;
            break;
        }
    }
    let mut end = text.len();
    for i in at..bytes.len() {
        if is_clause_terminator(bytes, i) {
            end = i;
            break;
        }
    }
    text[start..end].trim()
}

/// Byte offsets at which the top-effect abundance statistic is being DESCRIBED:
/// every [`effect_ratio_anchors`] citation site plus every mention of the
/// abundance basis in [`EFFECT_ABUNDANCE_NOUNS`]. A direction word is only
/// attributable to this statistic when it shares a clause with one of these.
fn effect_abundance_anchors(text_lc: &str, ratio: f64) -> Vec<usize> {
    let mut anchors = effect_ratio_anchors(text_lc, ratio);
    for noun in EFFECT_ABUNDANCE_NOUNS {
        anchors.extend(find_all(text_lc, noun));
    }
    anchors.sort_unstable();
    anchors.dedup();
    anchors
}

/// How much of the offending clause the finding quotes, so a reviewer can see
/// exactly what was matched without the finding growing unbounded.
const EFFECT_DIRECTION_QUOTE_BYTES: usize = 200;

/// Warn when a clause that DESCRIBES the top-effect abundance states a
/// direction contradicting the sign of the computed ratio. At most one finding
/// per surface: the same sentence is normally cited several times over (the
/// field name and the rendered value are both anchors), and one warning per
/// document is the actionable unit.
fn check_effect_direction_claim(
    text: &str,
    ratio: f64,
    surface: &str,
    report: &mut ReportingInvariantsReport,
) {
    let text_lc = text.to_lowercase();
    for anchor in effect_abundance_anchors(&text_lc, ratio) {
        let clause = clause_around(&text_lc, anchor);
        let says_above = EFFECT_DIRECTION_ABOVE.iter().any(|w| clause.contains(w));
        let says_below = EFFECT_DIRECTION_BELOW.iter().any(|w| clause.contains(w));
        let (claimed, actual, comparator) = if ratio < 1.0 && says_above && !says_below {
            ("ABOVE", "BELOW", "<")
        } else if ratio > 1.0 && says_below && !says_above {
            ("BELOW", "ABOVE", ">")
        } else {
            continue;
        };
        let quote = byte_window(clause, 0, 0, EFFECT_DIRECTION_QUOTE_BYTES);
        report.findings.push(ReportingFinding {
            invariant: "RP-1",
            severity: Severity::Warn,
            detail: format!(
                "{surface} describes the top-effect abundance as {claimed} background, but \
                 top_effect_abundance_ratio = {ratio:.4} ({comparator} 1) means the top effects \
                 sit {actual} the whole-set median abundance — clause: \"{quote}\""
            ),
        });
        return;
    }
}

/// Byte offsets in `text_lc` at which the top-effect abundance ratio is being
/// cited: the field name, the phrase "abundance ratio", or the value rendered
/// to 3 or 4 decimals (how a report prints it).
fn effect_ratio_anchors(text_lc: &str, ratio: f64) -> Vec<usize> {
    let mut anchors = Vec::new();
    for needle in ["top_effect_abundance_ratio", "abundance ratio"] {
        anchors.extend(find_all(text_lc, needle));
    }
    for rendered in [format!("{ratio:.3}"), format!("{ratio:.4}")] {
        anchors.extend(find_all(text_lc, &rendered));
    }
    anchors.sort_unstable();
    anchors.dedup();
    anchors
}

/// How far either side of a ratio citation counts as "adjacent" prose.
const EFFECT_RATIO_WINDOW_BYTES: usize = 400;

/// Warn when the prose adjacent to a `top_effect_abundance_ratio` citation
/// describes it as a mean/average, or attributes it to a number of SAMPLES.
/// Modality-neutral: it never asserts what the features are, only that the
/// statistic is a median-over-features ratio.
fn check_effect_ratio_prose(
    package_root: &Path,
    outputs: &Path,
    ratio: f64,
    report: &mut ReportingInvariantsReport,
) {
    let Some(prose) = read_agent_report_prose(outputs) else {
        return;
    };
    let text_lc = prose.to_lowercase();
    let anchors = effect_ratio_anchors(&text_lc, ratio);
    if anchors.is_empty() {
        return;
    }
    let mean_re = Regex::new(r"\b(?:mean|means|average|averaged|averages)\b")
        .expect("static RP-1 mean-word regex compiles");
    let samples_re =
        Regex::new(r"\b(?:across|among|over|within|of)\s+(?:the\s+)?(\d[\d,]*)\s+samples?\b")
            .expect("static RP-1 sample-attribution regex compiles");

    let mut mean_word: Option<String> = None;
    let mut sample_claim: Option<String> = None;
    for anchor in anchors {
        let win = byte_window(
            &text_lc,
            anchor,
            EFFECT_RATIO_WINDOW_BYTES,
            EFFECT_RATIO_WINDOW_BYTES,
        );
        if mean_word.is_none() {
            if let Some(m) = mean_re.find(win) {
                mean_word = Some(m.as_str().to_string());
            }
        }
        if sample_claim.is_none() {
            if let Some(c) = samples_re.captures(win) {
                sample_claim = Some(c[1].to_string());
            }
        }
        if mean_word.is_some() && sample_claim.is_some() {
            break;
        }
    }

    if let Some(word) = mean_word {
        report.findings.push(ReportingFinding {
            invariant: "RP-1",
            severity: Severity::Warn,
            detail: format!(
                "report describes top_effect_abundance_ratio ({ratio:.4}) with \"{word}\", but \
                 the statistic is {EFFECT_ABUNDANCE_RATIO_DEFINITION} — copy that definition \
                 rather than paraphrasing it as a mean/average"
            ),
        });
    }
    if let Some(claimed) = sample_claim {
        let recorded = collect_data_provenance(package_root)
            .records
            .iter()
            .find_map(|r| r.n_samples);
        let recorded_note = match recorded {
            Some(n) => format!(
                "; the package records {n} sample(s), and the claimed {claimed} is not a sample \
                 count at all"
            ),
            None => String::new(),
        };
        report.findings.push(ReportingFinding {
            invariant: "RP-1",
            severity: Severity::Warn,
            detail: format!(
                "report attributes top_effect_abundance_ratio ({ratio:.4}) to \"{claimed} \
                 samples\", but the statistic is {EFFECT_ABUNDANCE_RATIO_DEFINITION} — it is \
                 computed over features, not samples{recorded_note}"
            ),
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
        "not ",
        "n't ",
        "rather than",
        "instead of",
        "no ",
        "without",
        "isn't",
        "aren't",
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
// RP-PROV (Required) — narrative provenance vs the package's own record
// ---------------------------------------------------------------------------

/// Phrases asserting the analysed data was handed over by the SME / user
/// rather than fetched by the run. Matched on lowercased prose.
const SME_SUPPLIED_PHRASES: &[&str] = &[
    "supplied by the sme",
    "provided by the sme",
    "sme-supplied",
    "sme supplied",
    "supplied by the user",
    "provided by the user",
    "user-supplied",
    "from a local copy",
    "local copy of",
    "locally supplied",
];

/// Nouns that mark an SME-supply phrase as being about DATA rather than about
/// a method, threshold, or design choice the SME also "supplied".
const PROV_DATA_NOUNS: &[&str] = &[
    "data",
    "dataset",
    "matrix",
    "matrices",
    "count",
    "counts",
    "input",
    "file",
    "sample sheet",
    "table",
    "reads",
    "manifest",
];

/// Cues that mark an SME-supply phrase as a DISAVOWAL ("the SME path was not
/// supplied", "no local copy was available") rather than an assertion.
const PROV_NEGATION_CUES: &[&str] = &[
    "not ",
    "n't ",
    "no ",
    "rather than",
    "instead of",
    "absent",
    "never ",
    "without",
    "unavailable",
];

/// Cues that mark a LINE as asserting where this run's data came from. Used to
/// keep the accession-contradiction check off literature-context prose, which
/// legitimately names other accessions.
const PROV_SOURCE_CUES: &[&str] = &[
    "data source",
    "dataset",
    "accession",
    "downloaded",
    "download",
    "retrieved",
    "obtained",
    "fetched",
    "input path",
    "acquisition",
];

/// Journal-name tokens carrying no discriminating power for an initialism.
const JOURNAL_STOPWORDS: &[&str] = &["of", "the", "and", "for", "in", "on", "a", "an"];

fn norm_alnum(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn journal_tokens(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn journal_initialism(tokens: &[String]) -> String {
    tokens
        .iter()
        .filter(|t| !JOURNAL_STOPWORDS.contains(&t.as_str()))
        .filter_map(|t| t.chars().next())
        .collect()
}

/// Whether a journal name claimed in prose is a legitimate rendering of the
/// journal the package recorded. Deliberately generous — an abbreviation
/// ("Nat Genet" for "Nature Genetics") and an initialism ("NEJM" for "New
/// England Journal of Medicine") both pass — so only a genuinely different
/// journal can trip the REQUIRED gate.
fn journal_matches(claimed: &str, recorded: &str) -> bool {
    let (ct, rt) = (journal_tokens(claimed), journal_tokens(recorded));
    if ct.is_empty() || rt.is_empty() {
        return true;
    }
    if norm_alnum(claimed) == norm_alnum(recorded) {
        return true;
    }
    if ct.len() == rt.len()
        && ct
            .iter()
            .zip(rt.iter())
            .all(|(c, r)| r.starts_with(c.as_str()) || c.starts_with(r.as_str()))
    {
        return true;
    }
    norm_alnum(claimed) == journal_initialism(&rt)
        || norm_alnum(recorded) == journal_initialism(&ct)
}

/// The recorded first author's surname, lowercased, for anchoring a citation
/// match. `"Himes BE"` → `"himes"`.
fn first_author_surname(record: &DataProvenanceRecord) -> Option<String> {
    let raw = record.first_author.as_ref()?;
    let token = raw.split_whitespace().next()?;
    let norm = norm_alnum(token);
    (norm.len() >= 3).then_some(norm)
}

/// Split an accession id into `(alphabetic prefix, numeric suffix)`.
fn accession_parts(token: &str) -> Option<(String, String)> {
    let upper = token.to_uppercase();
    let split = upper.find(|c: char| c.is_ascii_digit())?;
    let (prefix, digits) = upper.split_at(split);
    let prefix = prefix.trim_end_matches(['-', '_']).to_string();
    if prefix.len() < 2 || digits.len() < 3 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((prefix, digits.to_string()))
}

/// True when the SME-supply phrase at `at` is negated or is not about data.
fn sme_phrase_is_inert(text_lc: &str, at: usize) -> bool {
    let before = byte_window(text_lc, at, 40, 0);
    if PROV_NEGATION_CUES.iter().any(|cue| before.contains(cue)) {
        return true;
    }
    let context = byte_window(text_lc, at, 160, 160);
    !PROV_DATA_NOUNS.iter().any(|n| context.contains(n))
}

/// RP-PROV: every bibliographic / data-source assertion in the emitted report
/// must be consistent with the package's own acquisition record.
///
/// Four contradictions are gated, each naming BOTH sides:
///
/// 1. **Journal.** A `<first-author> et al., <Journal> <Year>` citation whose
///    author surname and year match the package's recorded publication, but
///    whose journal is a different journal.
/// 2. **DOI / PMID.** A DOI or PMID stated on the SAME LINE as a recorded
///    accession that differs from the DOI / PMID recorded for it.
/// 3. **False local-copy claim.** "supplied by the SME" / "from a local copy"
///    phrasing about the data when `runtime/inputs.json` registers no local
///    input at all.
/// 4. **Accession.** A same-family, different-id accession (`GSE52779` where
///    the package records `GSE52778`) on a line asserting the data source.
///
/// Skipped entirely when the package records no accession metadata to compare
/// against — an absent input is never a failure. The system-owned provenance
/// block is stripped before scanning, so the section this check backstops can
/// never fault itself.
fn check_rp_prov_data_source(
    package_root: &Path,
    outputs: &Path,
    report: &mut ReportingInvariantsReport,
) {
    let Some(prose) = read_agent_report_prose(outputs) else {
        return;
    };
    let prov = collect_data_provenance(package_root);
    if prov.records.is_empty() {
        return;
    }
    report.checked.push("RP-PROV");

    let text_lc = prose.to_lowercase();
    let mut contradictions: Vec<String> = Vec::new();

    check_prov_citation(&text_lc, &prov, &mut contradictions);
    check_prov_identifiers(&text_lc, &prov, &mut contradictions);
    check_prov_local_copy_claim(&text_lc, &prov, &mut contradictions);
    check_prov_accession(&prose, &prov, &mut contradictions);

    if !contradictions.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RP-PROV",
            severity: Severity::Required,
            detail: format!(
                "the report asserts data provenance that contradicts the package's own \
                 acquisition record — {}",
                contradictions.join("; ")
            ),
        });
    }
}

fn check_prov_citation(text_lc: &str, prov: &DataProvenance, out: &mut Vec<String>) {
    let cite = Regex::new(
        r"([a-z][a-z'\-]{1,30})\s+et\s+al\.?,?\s+([a-z][a-z .&'\-]{1,40}?)[,\s]+((?:19|20)\d{2})",
    )
    .expect("static RP-PROV citation regex compiles");
    for record in &prov.records {
        let (Some(surname), Some(journal)) =
            (first_author_surname(record), record.journal.as_ref())
        else {
            continue;
        };
        for caps in cite.captures_iter(text_lc) {
            if norm_alnum(&caps[1]) != surname {
                continue;
            }
            // A different year is a different paper by the same author, not a
            // misattribution of THIS dataset's publication.
            if let Some(year) = &record.year {
                if year.trim() != &caps[3] {
                    continue;
                }
            }
            let claimed = caps[2].trim().trim_end_matches([',', '.', ';']).trim();
            if claimed.is_empty() || journal_matches(claimed, journal) {
                continue;
            }
            out.push(format!(
                "report cites the study as \"{} et al., {claimed} {}\", but the package's \
                 {} record states journal \"{journal}\"",
                &caps[1],
                &caps[3],
                record
                    .accession
                    .clone()
                    .unwrap_or_else(|| record.stage_id.clone()),
            ));
            break;
        }
    }
}

fn check_prov_identifiers(text_lc: &str, prov: &DataProvenance, out: &mut Vec<String>) {
    let doi_re =
        Regex::new(r#"10\.\d{4,9}/[^\s)\]\}",;]+"#).expect("static RP-PROV doi regex compiles");
    let pmid_re =
        Regex::new(r"pmid[^0-9a-z]{0,6}(\d{6,9})").expect("static RP-PROV pmid regex compiles");
    for record in &prov.records {
        let Some(accession) = record.accession.as_ref() else {
            continue;
        };
        let accession_lc = accession.to_lowercase();
        for line in text_lc.lines() {
            if !line.contains(&accession_lc) {
                continue;
            }
            if let Some(recorded) = record.doi.as_ref() {
                let recorded_lc = recorded.to_lowercase();
                for m in doi_re.find_iter(line) {
                    let claimed = m.as_str().trim_end_matches(['.', ',', ')']);
                    if claimed != recorded_lc {
                        out.push(format!(
                            "report states DOI \"{claimed}\" alongside accession {accession}, \
                             but the package records DOI \"{recorded}\""
                        ));
                        break;
                    }
                }
            }
            if let Some(recorded) = record.pmid.as_ref() {
                if let Some(caps) = pmid_re.captures(line) {
                    if &caps[1] != recorded.trim() {
                        out.push(format!(
                            "report states PMID {} alongside accession {accession}, but the \
                             package records PMID {recorded}",
                            &caps[1]
                        ));
                    }
                }
            }
        }
    }
}

fn check_prov_local_copy_claim(text_lc: &str, prov: &DataProvenance, out: &mut Vec<String>) {
    if prov.has_sme_registered_inputs() {
        return;
    }
    for phrase in SME_SUPPLIED_PHRASES {
        let Some(at) = find_all(text_lc, phrase)
            .into_iter()
            .find(|at| !sme_phrase_is_inert(text_lc, *at))
        else {
            continue;
        };
        let actual = prov
            .records
            .iter()
            .map(|r| match (&r.source_package, &r.accession) {
                (Some(pkg), _) => format!("{} read from software package `{pkg}`", r.stage_id),
                (None, Some(acc)) => format!("{} fetched accession {acc}", r.stage_id),
                (None, None) => r.stage_id.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let registration_state = if prov.inputs_json_present {
            "runtime/inputs.json registers no input"
        } else {
            "runtime/inputs.json is absent"
        };
        let excerpt = byte_window(text_lc, at, 60, 120).trim().to_string();
        out.push(format!(
            "report claims the data was \"{phrase}\" (…{excerpt}…), but {registration_state}, so \
             no SME local input was ever registered; the package records instead: {actual}"
        ));
        return;
    }
}

fn check_prov_accession(prose: &str, prov: &DataProvenance, out: &mut Vec<String>) {
    let recorded: BTreeMap<String, String> = prov
        .records
        .iter()
        .filter_map(|r| r.accession.as_ref())
        .filter_map(|a| accession_parts(a).map(|(prefix, _)| (prefix, a.clone())))
        .collect();
    if recorded.is_empty() {
        return;
    }
    let acc_re = Regex::new(r"\b([A-Za-z]{2,6})[-_]?(\d{3,12})\b")
        .expect("static RP-PROV accession regex compiles");
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for line in prose.lines() {
        let line_lc = line.to_lowercase();
        if !PROV_SOURCE_CUES.iter().any(|cue| line_lc.contains(cue)) {
            continue;
        }
        for caps in acc_re.captures_iter(line) {
            let Some((prefix, digits)) = accession_parts(&caps[0]) else {
                continue;
            };
            let Some(recorded_id) = recorded.get(&prefix) else {
                continue;
            };
            let Some((_, recorded_digits)) = accession_parts(recorded_id) else {
                continue;
            };
            if digits == recorded_digits {
                continue;
            }
            let claimed = format!("{prefix}{digits}");
            if seen.insert(claimed.clone()) {
                out.push(format!(
                    "report names accession {claimed} in a data-source statement, but the \
                     package records {recorded_id} for that repository"
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RP-QC (Required) — an unqualified QC-negative claim needs a retained artifact
// ---------------------------------------------------------------------------

/// How deep under `runtime/outputs/` the sample-QC artifact scan descends, and
/// how many directory entries it will visit — bounds so the scan is cheap and
/// terminates on a pathological tree.
const QC_SCAN_MAX_DEPTH: usize = 5;
const QC_SCAN_MAX_ENTRIES: usize = 20_000;
/// Largest JSON the scan will parse looking for an outlier-shaped key.
const QC_JSON_MAX_BYTES: u64 = 4 * 1024 * 1024;

/// Filename markers of a retained sample-level QC artifact. Compared against
/// the filename with every non-alphanumeric character removed, so
/// `sample-distances.tsv`, `sample_distance_matrix.png`, and
/// `sampleDistances.pdf` all match.
const QC_ARTIFACT_MARKERS: &[&str] = &[
    "outlier",
    "sampledistance",
    "sampledist",
    "samplecorrelation",
    "cooksdistance",
    "cooksd",
    "distancematrix",
    "sampleclustering",
    "hierarchicalclustering",
    "samplepca",
    "pcaplot",
    "pcascores",
    "pcaloadings",
    "mdsplot",
];

/// True when `file_name` names a retained outlier / PCA / sample-distance
/// artifact. Modality-agnostic: it recognizes the artifact CLASS, never a
/// domain-specific entity.
fn is_sample_qc_artifact(file_name: &str) -> bool {
    let lower = file_name.to_lowercase();
    let squashed: String = lower
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if QC_ARTIFACT_MARKERS.iter().any(|m| squashed.contains(m)) {
        return true;
    }
    // `pca` / `mds` are too short for a substring test (they occur inside
    // unrelated words), so require them as a whole filename token.
    lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|t| matches!(t, "pca" | "mds"))
}

/// The first object key containing "outlier" anywhere in a JSON document — a
/// recorded outlier verdict counts as a retained artifact even when no file is
/// named for it.
fn find_outlier_key(v: &Value) -> Option<String> {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if k.to_lowercase().contains("outlier") {
                    return Some(k.clone());
                }
                if let Some(hit) = find_outlier_key(val) {
                    return Some(hit);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(find_outlier_key),
        _ => None,
    }
}

fn json_outlier_key(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > QC_JSON_MAX_BYTES {
        return None;
    }
    find_outlier_key(&read_json(path)?)
}

/// Depth-first, name-sorted scan for a retained sample-QC artifact. Returns
/// the package-relative path of the first hit. Deterministic: entries are
/// visited in sorted order, files before subdirectories.
fn scan_for_qc_artifact(
    root: &Path,
    dir: &Path,
    depth: usize,
    budget: &mut usize,
) -> Option<String> {
    if depth > QC_SCAN_MAX_DEPTH || *budget == 0 {
        return None;
    }
    let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.file_name())
        .collect();
    names.sort();
    let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
    for name in names {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let path = dir.join(&name);
        if path.is_dir() {
            subdirs.push(path);
            continue;
        }
        let file_name = name.to_string_lossy().to_string();
        let rel = || {
            path.strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string()
        };
        if is_sample_qc_artifact(&file_name) {
            return Some(rel());
        }
        if file_name.to_lowercase().ends_with(".json") {
            if let Some(key) = json_outlier_key(&path) {
                return Some(format!("{} (key `{key}`)", rel()));
            }
        }
    }
    for sub in subdirs {
        if let Some(hit) = scan_for_qc_artifact(root, &sub, depth + 1, budget) {
            return Some(hit);
        }
    }
    None
}

/// RP-QC: an unqualified QC-NEGATIVE assertion — "no outlier samples were
/// identified", "the cohort was outlier-free" — is a claim about a computation
/// that must have left an artifact behind. The deposited report asserted it
/// while the package contained no outlier / PCA / sample-distance artifact of
/// any kind: nothing in the deposit could corroborate or refute the sentence,
/// and no downstream check noticed, because every other invariant recomputes
/// numbers rather than reading claims about absent computations.
///
/// Required, because an unsupported QC-negative assertion is the kind of claim
/// a reader most reasonably relies on. It is satisfied by ANY retained
/// sample-level QC artifact (a PCA / MDS plot or score table, a
/// sample-distance or sample-correlation matrix, a Cook's-distance output, or
/// any JSON recording an outlier-shaped key) — the check never demands a
/// particular tool or figure id, so a modality with a different QC idiom
/// satisfies it by retaining its own artifact.
fn check_rp_qc_negative_claim(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(prose) = read_agent_report_prose(outputs) else {
        return;
    };
    report.checked.push("RP-QC");

    let patterns = [
        r"\bno\s+outlier\s+samples?\s+(?:were|was)\s+\w+",
        r"\bno\s+(?:sample\s+)?outliers?\s+(?:were|was)\s+(?:identified|detected|found|observed|flagged|apparent|present|evident|seen)\b",
        r"\bno\s+(?:sample\s+)?outliers?\s*[.;]",
        r"\bno\s+samples?\s+(?:were\s+)?(?:flagged|identified|detected|excluded|removed)\s+as\s+(?:an\s+)?outliers?\b",
        r"\boutlier[-\s]free\b",
    ];
    let text_lc = prose.to_lowercase();
    let mut claim: Option<String> = None;
    for pattern in patterns {
        let re = Regex::new(pattern).expect("static RP-QC regex compiles");
        if let Some(m) = re.find(&text_lc) {
            claim = Some(m.as_str().trim().to_string());
            break;
        }
    }
    let Some(claim) = claim else {
        return;
    };
    let mut budget = QC_SCAN_MAX_ENTRIES;
    if scan_for_qc_artifact(outputs, outputs, 0, &mut budget).is_some() {
        return;
    }
    report.findings.push(ReportingFinding {
        invariant: "RP-QC",
        severity: Severity::Required,
        detail: format!(
            "report asserts \"{claim}\", but the package retains no sample-level QC artifact \
             supporting it — no outlier table/verdict, PCA or MDS output, sample-distance or \
             sample-correlation matrix, or Cook's-distance output is present under \
             runtime/outputs/. Either retain the artifact the conclusion was drawn from or \
             drop the claim"
        ),
    });
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
        let Ok((headers, rows)) = crate::report_contract::assemble::read_table(&source_path) else {
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
        if let (Some(reported), Some(actual)) = (&artifact.direction_split, &stats.direction_split)
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

#[derive(Debug, Clone, Copy)]
struct LiteratureCounts {
    novel_count: u64,
    n_entities_assessed: u64,
    n_entities_not_assessed: u64,
    n_evidence_rows_assessed: u64,
    n_evidence_rows_total: u64,
}

impl LiteratureCounts {
    fn named(self) -> [(&'static str, u64); 6] {
        [
            ("novel_count", self.novel_count),
            ("not_assessed_count", self.n_entities_not_assessed),
            ("n_entities_assessed", self.n_entities_assessed),
            ("n_entities_not_assessed", self.n_entities_not_assessed),
            ("n_evidence_rows_assessed", self.n_evidence_rows_assessed),
            ("n_evidence_rows_total", self.n_evidence_rows_total),
        ]
    }
}

fn parse_bool_cell(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" => Some(true),
        "false" | "0" | "no" => Some(false),
        _ => None,
    }
}

fn recompute_literature_counts(matrix_path: &Path) -> Option<LiteratureCounts> {
    let (headers, rows) = crate::report_contract::assemble::read_table(matrix_path).ok()?;
    let finding_idx = headers.iter().position(|name| name == "finding_id")?;
    let entity_idx = headers.iter().position(|name| name == "entity")?;
    let flag_idx = headers.iter().position(|name| name == "concordance_flag")?;
    let searched_idx = headers.iter().position(|name| name == "searched");

    let mut novel = BTreeSet::new();
    let mut assessed = BTreeSet::new();
    let mut not_assessed = BTreeSet::new();
    let mut assessed_rows = 0u64;
    for row in &rows {
        let flag = row.get(flag_idx).unwrap_or("").trim();
        let entity = crate::report_contract::report_data::matrix_entity_label(
            row.get(entity_idx).unwrap_or(""),
            row.get(finding_idx).unwrap_or(""),
        );
        if entity.is_empty() {
            continue;
        }
        let searched = searched_idx
            .and_then(|idx| row.get(idx))
            .and_then(parse_bool_cell)
            .unwrap_or({
                matches!(
                    flag,
                    "same_direction" | "opposite_direction" | "unverifiable" | "no_prior_finding"
                )
            });
        if searched {
            assessed_rows += 1;
            assessed.insert(entity.clone());
        } else {
            not_assessed.insert(entity.clone());
        }
        if flag == "no_prior_finding" {
            novel.insert(entity);
        }
    }
    for entity in &assessed {
        not_assessed.remove(entity);
    }

    Some(LiteratureCounts {
        novel_count: novel.len() as u64,
        n_entities_assessed: assessed.len() as u64,
        n_entities_not_assessed: not_assessed.len() as u64,
        n_evidence_rows_assessed: assessed_rows,
        n_evidence_rows_total: rows.len() as u64,
    })
}

fn json_named_u64(value: &Value, name: &str) -> Option<u64> {
    value.get(name).and_then(|raw| {
        raw.as_u64().or_else(|| {
            raw.as_str()
                .and_then(|text| text.trim().replace(',', "").parse().ok())
        })
    })
}

/// RC-LITERATURE: keep distinct-entity and evidence-row denominators aligned
/// across the deterministic report contract, the contextualization summary,
/// and narrative claims that explicitly name one of those machine fields.
///
/// The matrix is the source artifact. Entity labels that contain a conventional
/// missing-value marker fall back to `finding_id`, matching the contextualizer,
/// so 242 unresolved accessions cannot become one entity named `NA`. The
/// narrative scan is deliberately limited to explicit field names; an
/// unlabelled free-text number is too ambiguous to block a deposit.
fn check_rc_literature_counts(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let matrix_path = outputs
        .join("contextualize_findings_with_literature")
        .join("claims_evidence_matrix.csv");
    let Some(expected) = recompute_literature_counts(&matrix_path) else {
        return;
    };
    report.checked.push("RC-LITERATURE");

    let mut mismatches = Vec::new();
    if let Some(report_data) = read_report_data(outputs) {
        if let Some(literature) = report_data.literature {
            let observed = [
                ("novel_count", literature.novel_count),
                ("not_assessed_count", literature.not_assessed_count),
                ("n_entities_assessed", literature.n_entities_assessed),
                (
                    "n_entities_not_assessed",
                    literature.n_entities_not_assessed,
                ),
                (
                    "n_evidence_rows_assessed",
                    literature.n_evidence_rows_assessed,
                ),
                ("n_evidence_rows_total", literature.n_evidence_rows_total),
            ];
            for ((name, expected_value), (_, observed_value)) in
                expected.named().into_iter().zip(observed)
            {
                if observed_value != expected_value {
                    mismatches.push(format!(
                        "report-data.json {name}={observed_value}, recomputed={expected_value}"
                    ));
                }
            }
        }
    }

    let result_path = outputs
        .join("contextualize_findings_with_literature")
        .join("result.json");
    if let Some(result) = read_json(&result_path) {
        for (name, expected_value) in expected.named() {
            if name == "not_assessed_count" || name == "novel_count" {
                continue;
            }
            if let Some(observed) = json_named_u64(&result, name) {
                if observed != expected_value {
                    mismatches.push(format!(
                        "contextualization result.json {name}={observed}, \
                         recomputed={expected_value}"
                    ));
                }
            }
        }
    }

    if let Some(narrative) = read_reports(outputs) {
        for (name, expected_value) in expected.named() {
            let pattern = format!(
                r"(?i)`?{}`?[^0-9]{{0,64}}([0-9][0-9,]*)",
                regex::escape(name)
            );
            let Ok(re) = Regex::new(&pattern) else {
                continue;
            };
            for capture in re.captures_iter(&narrative) {
                let Some(observed) = capture
                    .get(1)
                    .and_then(|value| value.as_str().replace(',', "").parse::<u64>().ok())
                else {
                    continue;
                };
                if observed != expected_value {
                    mismatches.push(format!(
                        "narrative {name}={observed}, recomputed={expected_value}"
                    ));
                }
            }
        }
    }

    if !mismatches.is_empty() {
        mismatches.sort();
        mismatches.dedup();
        report.findings.push(ReportingFinding {
            invariant: "RC-LITERATURE",
            severity: Severity::Required,
            detail: format!(
                "literature entity/evidence-row counts disagree with \
                 claims_evidence_matrix.csv: {}",
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
/// UNIVERSAL (domain-agnostic) spelled-out forms of an abbreviated section-id
/// token, so a natural report heading that EXPANDS the abbreviation still
/// satisfies the requirement. A token matches a heading if the heading contains
/// the token itself OR any form returned here. Deliberately tiny and universal
/// — NOT a per-modality heading vocabulary: `qc` is the one cross-domain
/// abbreviation ECAA's section ids use for a term reports spell out (a
/// `qc_preprocessing` section titled "Quality Control and Preprocessing").
fn section_word_aliases(word: &str) -> &'static [&'static str] {
    match word {
        "qc" => &["quality control", "quality-control"],
        _ => &[],
    }
}

fn heading_matches_section(line: &str, words: &[String]) -> bool {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return false;
    }
    let lc = trimmed.to_lowercase();
    // Each id-word must be present, either verbatim or via a universal
    // spelled-out alias. Keeping the "ALL words present" requirement preserves
    // the strictness that stops an unrelated heading from anchoring a section;
    // aliases only bridge abbreviation ↔ expansion, never widen the match.
    words
        .iter()
        .all(|w| lc.contains(w.as_str()) || section_word_aliases(w).iter().any(|a| lc.contains(a)))
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

/// RC-TABLE: every significant entity recorded in `report-data.json` must
/// actually appear in the rendered TERMINAL report. `report-data.json` is the
/// deterministic single source of truth for the significant set, and RC-COUNT
/// guarantees its counts are correct — but nothing otherwise guarantees the
/// human-readable report the SME lands on renders that set in full. The
/// "render the full significant set as a table" obligation lives ONLY in the
/// agent prompt (`scripts/agent-prompts/task-execution.md`), so an agent can
/// silently ship a summarized report — e.g. a 39-row digest of a 4030-row set
/// — that still passes RC-COUNT (reads only `report-data.json`) and RC-SECTIONS
/// (checks heading presence, not row coverage). This check is the deterministic
/// backstop for that gap: for every artifact whose full significant set is
/// embedded (`spilled_to_attachment_only == false`), every `EntityRow::entity`
/// must appear as a substring of the terminal report text.
///
/// Modality-agnostic: it iterates whatever entities the assembler resolved
/// (gene ids, peak ids, variant loci, pathway names, …) — it never assumes a
/// domain-specific entity vocabulary. Spilled artifacts (the degenerate-output
/// guard tripped, `> SPILL_THRESHOLD` rows) are skipped, matching the prompt's
/// explicit carve-out that a spilled set may be summarized rather than inlined.
/// Uses [`read_terminal_report`] (prefers `final_reporting/final_report.md`),
/// so a complete intermediate `reporting/report.md` cannot mask a truncated
/// terminal report. Substring membership is deliberately lenient (an entity id
/// that is a prefix of another can read as present) so the check biases toward
/// never false-blocking a correct deposit; the gross truncation it targets
/// (thousands of rows absent) is caught regardless.
fn check_rc_table(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(report_data) = read_report_data(outputs) else {
        return;
    };
    // Only artifacts whose full significant set is embedded and non-empty have
    // a table to verify; a spilled or empty artifact has nothing to inline.
    let has_checkable = report_data
        .artifacts
        .iter()
        .any(|a| !a.spilled_to_attachment_only && !a.significant_entities.is_empty());
    if !has_checkable {
        return;
    }
    let Some(text) = read_terminal_report(outputs) else {
        return;
    };
    report.checked.push("RC-TABLE");

    let mut offenders: Vec<String> = Vec::new();
    for artifact in &report_data.artifacts {
        if artifact.spilled_to_attachment_only || artifact.significant_entities.is_empty() {
            continue;
        }
        let total = artifact.significant_entities.len();
        let mut missing = 0usize;
        let mut examples: Vec<&str> = Vec::new();
        for row in &artifact.significant_entities {
            if row.entity.is_empty() {
                continue;
            }
            if !text.contains(row.entity.as_str()) {
                missing += 1;
                if examples.len() < 3 {
                    examples.push(row.entity.as_str());
                }
            }
        }
        if missing > 0 {
            offenders.push(format!(
                "{}: {missing} of {total} significant entities absent from the terminal \
                 report (e.g. {})",
                artifact.stage_id,
                examples.join(", ")
            ));
        }
    }
    if !offenders.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-TABLE",
            severity: Severity::Required,
            detail: format!(
                "the terminal report does not render every significant entity recorded in \
                 report-data.json (an un-spilled significant set must be inlined in full, not \
                 summarized) — {}",
                offenders.join("; ")
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// RC-ROW (Required) — a narrative table row must exist in its source table
// ---------------------------------------------------------------------------

/// One markdown table lifted out of a report, with the caption line above it.
struct NarrativeTable {
    /// 1-based line number of the header row, so a finding names its site.
    line: usize,
    /// Nearest preceding non-blank line — the table's caption.
    heading: String,
    /// Header cells, in column order.
    header: Vec<String>,
    /// Data rows (the `|---|` alignment row excluded), in table order.
    rows: Vec<Vec<String>>,
}

/// Split one markdown table line into trimmed cells, dropping the leading and
/// trailing pipe. A cell cannot itself contain a pipe in this syntax, so a
/// header like `| |log2FC| range | Count |` splits into four cells — and then
/// simply resolves to no entity role, which is the skip path.
fn split_markdown_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let inner = trimmed
        .strip_prefix('|')
        .unwrap_or(trimmed)
        .strip_suffix('|')
        .unwrap_or_else(|| trimmed.strip_prefix('|').unwrap_or(trimmed));
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

/// `true` for a markdown alignment row (`|---|:--:|`): at least one non-empty
/// cell, and every non-empty cell is dashes with optional bounding colons.
fn is_markdown_alignment_row(cells: &[String]) -> bool {
    let mut saw = false;
    for cell in cells {
        if cell.is_empty() {
            continue;
        }
        saw = true;
        let core = cell.trim_start_matches(':').trim_end_matches(':');
        if core.is_empty() || !core.chars().all(|c| c == '-') {
            return false;
        }
    }
    saw
}

/// Every markdown table in `text`, in document order. A table is a pipe line
/// followed by an alignment row followed by zero or more pipe lines.
fn find_markdown_tables(text: &str) -> Vec<NarrativeTable> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let is_table_start = lines[i].trim_start().starts_with('|')
            && lines
                .get(i + 1)
                .is_some_and(|next| next.trim_start().starts_with('|'))
            && is_markdown_alignment_row(&split_markdown_row(lines[i + 1]));
        if !is_table_start {
            i += 1;
            continue;
        }
        let header = split_markdown_row(lines[i]);
        let heading = lines[..i]
            .iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .unwrap_or_default();
        let mut rows = Vec::new();
        let mut j = i + 2;
        while j < lines.len() && lines[j].trim_start().starts_with('|') {
            let cells = split_markdown_row(lines[j]);
            if !is_markdown_alignment_row(&cells) {
                rows.push(cells);
            }
            j += 1;
        }
        out.push(NarrativeTable {
            line: i + 1,
            heading,
            header,
            rows,
        });
        i = j;
    }
    out
}

/// Remove a `<!-- marker START -->…<!-- marker END -->` block, repeatedly.
/// Mirrors [`strip_provenance_section`], which is hardcoded to the provenance
/// marker pair and so cannot be reused for the full-table pair.
fn strip_marked_block(text: &str, start: &str, end: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let (Some(s), Some(e)) = (rest.find(start), rest.find(end)) {
        if e < s {
            break;
        }
        out.push_str(&rest[..s]);
        rest = &rest[e + end.len()..];
    }
    out.push_str(rest);
    out
}

/// The AGENT-authored tables of one report: the two SYSTEM-owned marker blocks
/// (the deterministically rendered complete significant-entities tables and the
/// data-provenance block) are excluded. Those are generated from
/// `report-data.json`, which RC-COUNT and the report-data→source transcription
/// check already gate — they are the reference, not an assertion under test.
fn agent_authored_tables(report_text: &str) -> Vec<NarrativeTable> {
    let text = strip_marked_block(
        &strip_provenance_section(report_text),
        FULL_TABLE_START,
        FULL_TABLE_END,
    );
    find_markdown_tables(&text)
}

/// Canonical form for comparing a narrative cell against a source cell: NFC
/// composition then ASCII casefold, mirroring the normalization
/// [`crate::claim_verifier`] applies between narrative text and table cells.
fn normalize_cell(s: &str) -> String {
    s.trim().nfc().collect::<String>().to_ascii_lowercase()
}

/// Characters a renderer substitutes for ASCII hyphen-minus in a negative
/// number: U+2212 MINUS SIGN, U+2010 HYPHEN, U+2011 NON-BREAKING HYPHEN. An
/// en/em dash is deliberately NOT translated — those denote a range or an
/// absent value, and neither is a point assertion.
const MINUS_FORMS: &[char] = &['\u{2212}', '\u{2010}', '\u{2011}'];

/// Parse a numeric assertion out of a markdown table cell: strips markdown
/// emphasis/code markers and thousands separators, normalizes the minus forms
/// above. Returns `None` — "no point assertion here", never a mismatch — for an
/// empty/NA cell and for a bound or approximation (`< 0.001`, `~2`, `≥ 2`),
/// which asserts a range rather than a value.
fn parse_markdown_number(cell: &str) -> Option<f64> {
    let cleaned: String = cell
        .chars()
        .filter(|c| !matches!(c, '*' | '`' | '_' | ','))
        .map(|c| if MINUS_FORMS.contains(&c) { '-' } else { c })
        .collect();
    let s = cleaned.trim();
    if matches!(
        s.chars().next()?,
        '<' | '>' | '=' | '~' | '\u{2264}' | '\u{2265}'
    ) {
        return None;
    }
    s.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// A source result artifact indexed for narrative-row lookup.
struct SourceRowIndex {
    headers: csv::StringRecord,
    rows: Vec<csv::StringRecord>,
    /// Roles resolved by [`resolve_ranking_columns`] — the ONE column-role
    /// resolver, shared with the report-data assembler and the ranking module,
    /// so RC-ROW can never disagree with them about which column is which.
    cols: RankingColumns,
    /// Normalized cell value → the row indices carrying it, built over the
    /// columns that can NAME a row. The resolved measurement columns are
    /// excluded, and so is any other column's cell that parses as a number: a
    /// bare measurement is not a row identifier, and indexing one would let a
    /// narrative effect value resolve an unrelated row. The entity and declared
    /// grouping columns are indexed unconditionally, so a modality whose row
    /// identifier IS numeric (a taxon id, a genomic position) still resolves.
    keys: BTreeMap<String, Vec<usize>>,
}

impl SourceRowIndex {
    fn build(
        headers: csv::StringRecord,
        rows: Vec<csv::StringRecord>,
        schema: &ResultSchema,
        synonyms: &PolicyColumnSynonyms,
    ) -> Option<Self> {
        let cols = resolve_ranking_columns(&headers, schema, synonyms)?;
        let grouping = schema.grouping_column.as_deref().and_then(|g| {
            headers
                .iter()
                .position(|h| h.trim().eq_ignore_ascii_case(g.trim()))
        });
        let mut keys: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (row_index, row) in rows.iter().enumerate() {
            for (ci, cell) in row.iter().enumerate() {
                if Some(ci) == cols.effect || Some(ci) == cols.significance {
                    continue;
                }
                let identifier = ci == cols.entity || Some(ci) == grouping;
                if !identifier && parse_markdown_number(cell).is_some() {
                    continue;
                }
                let key = normalize_cell(cell);
                if key.is_empty() {
                    continue;
                }
                let bucket = keys.entry(key).or_default();
                if bucket.last() != Some(&row_index) {
                    bucket.push(row_index);
                }
            }
        }
        Some(Self {
            headers,
            rows,
            cols,
            keys,
        })
    }

    /// Row indices whose cells include `value` — the same ANY-CELL resolution
    /// `claim_verifier::verify_keyed_cell` performs, deliberately not an
    /// entity-column-only lookup: a narrative table routinely names a row by a
    /// column the schema does not declare as the entity (a bare term where the
    /// entity column carries a `COLLECTION: term` composite, an accession where
    /// the entity column carries a label, or the reverse).
    fn lookup(&self, value: &str) -> &[usize] {
        self.keys
            .get(&normalize_cell(value))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn number(&self, row_index: usize, column: usize) -> Option<f64> {
        let raw = self.rows.get(row_index)?.get(column)?;
        raw.trim().parse::<f64>().ok().filter(|v| v.is_finite())
    }
}

/// Every header spelling the schema + policy accept for a role, keyed by its
/// casefolded form AND its space→underscore variant, so a markdown caption
/// (`| Pathway | Collection | NES | padj |`) can be rewritten into the exact
/// spellings [`resolve_ranking_columns`] matches on.
///
/// This is a case/space FOLDER feeding the one existing resolver, not a second
/// resolver: which candidate wins which role still lives entirely in
/// `resolve_ranking_columns`. Making `pathway_ranking::resolve_column`
/// case-insensitive would delete this function outright.
fn schema_header_spellings(
    schema: &ResultSchema,
    synonyms: &PolicyColumnSynonyms,
) -> BTreeMap<String, String> {
    let names = std::iter::once(schema.entity_column.as_str())
        .chain(schema.entity_column_aliases.iter().map(String::as_str))
        .chain(schema.signed_effect_column.as_deref())
        .chain(schema.signed_effect_aliases.iter().map(String::as_str))
        .chain(schema.significance.as_ref().map(|s| s.column.as_str()))
        .chain(schema.grouping_column.as_deref())
        .chain(synonyms.entity.iter().map(String::as_str))
        .chain(synonyms.effect.iter().map(String::as_str))
        .chain(synonyms.significance.iter().map(String::as_str));
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for name in names {
        let lower = name.trim().to_ascii_lowercase();
        out.entry(lower.replace(' ', "_"))
            .or_insert_with(|| name.trim().to_string());
        out.entry(lower).or_insert_with(|| name.trim().to_string());
    }
    out
}

/// Rewrite a markdown header row into the schema's own column spellings so the
/// exact-match [`resolve_ranking_columns`] can read it. A cell matching no
/// candidate is passed through unchanged (it resolves to no role).
fn canonicalize_header(
    header: &[String],
    spellings: &BTreeMap<String, String>,
) -> csv::StringRecord {
    let cells: Vec<String> = header
        .iter()
        .map(|cell| {
            let lower = cell.trim().to_ascii_lowercase();
            spellings
                .get(&lower)
                .or_else(|| spellings.get(&lower.replace(' ', "_")))
                .cloned()
                .unwrap_or_else(|| cell.trim().to_string())
        })
        .collect();
    csv::StringRecord::from(cells)
}

/// The transcription tolerances RC-ROW compares under, read from the package's
/// own `interpretation-policy.json` through [`crate::claim_extractor::ExtractorConfig`]
/// — the same two policy fields (`tolerance.log2FcAbsoluteDelta` and
/// `tolerance.pvalueRelativeDelta`) `claim_verifier` compares a narrative number
/// against a table cell with. RC-ROW cannot invent a tolerance of its own: a
/// package whose policy declares none is skipped entirely.
struct NarrativeTolerances {
    effect_absolute: f64,
    significance_relative: f64,
}

/// A claimed significance of exactly `0` is display rounding ("padj 0.000" in a
/// narrative table rounds a true tiny positive p to zero): it agrees iff the
/// observed value is itself under this reporting floor. Kept byte-identical to
/// `claim_verifier`'s `PVALUE_ZERO_DISPLAY_FLOOR`.
const SIGNIFICANCE_ZERO_DISPLAY_FLOOR: f64 = 1e-3;

impl NarrativeTolerances {
    fn load(package_root: &Path) -> Option<Self> {
        for dir in [package_root.join("policies"), package_root.join("config")] {
            let Some(path) =
                crate::claim_extractor::resolve_policy_file(&dir, "interpretation-policy.json")
            else {
                continue;
            };
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(policy) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let Ok(cfg) = crate::claim_extractor::ExtractorConfig::from_policy(&policy) else {
                continue;
            };
            return Some(Self {
                effect_absolute: cfg.log2fc_tolerance,
                significance_relative: cfg.pvalue_relative_tolerance,
            });
        }
        None
    }

    /// `true` when a narrative cell agrees with its source cell for this role:
    /// an absolute delta for an effect, and the same order-of-magnitude band
    /// `|ln(claimed / observed)| <= ln(1 + rel)` for a significance value, so a
    /// legitimately re-rendered `7.06e-132` still agrees with `7.05596e-132`.
    ///
    /// The significance branch MIRRORS `claim_verifier::pvalue_within_tolerance`
    /// (private there) so one policy field cannot mean two things on two paths.
    /// Making that function `pub(crate)` deletes this branch in favour of a
    /// direct call.
    fn agrees(&self, role: &RoleCell, claimed: f64, observed: f64) -> bool {
        if !role.significance {
            return (claimed - observed).abs() <= self.effect_absolute;
        }
        if claimed == observed {
            return true;
        }
        if claimed == 0.0 {
            return observed > 0.0 && observed <= SIGNIFICANCE_ZERO_DISPLAY_FLOOR;
        }
        if claimed <= 0.0 || observed <= 0.0 {
            return false;
        }
        (claimed / observed).ln().abs() <= (1.0 + self.significance_relative).ln()
    }
}

/// One numeric cell RC-ROW compares: its column in the narrative table, the
/// same role's column in the source table, and which tolerance applies.
#[derive(Clone, Copy)]
struct RoleCell {
    narrative: usize,
    source: usize,
    /// `true` → significance (relative band); `false` → effect (absolute delta).
    significance: bool,
}

/// A narrative table bound to one stage's source artifact.
struct TableBinding {
    stage_id: String,
    artifact: String,
    /// Narrative column whose values resolve source rows.
    lookup: usize,
    roles: Vec<RoleCell>,
    /// Rows whose lookup value resolved to EXACTLY ONE source row.
    resolved: usize,
    /// Of those, the rows whose every compared cell agreed.
    agreeing: usize,
}

/// The outcome of trying to identify which source artifact a narrative table
/// transcribes.
enum TableBindingOutcome {
    /// No declared schema exposes this table's roles — there is no numeric
    /// assertion to re-derive.
    Unresolved,
    /// A schema's roles resolved, but too few rows corroborate the binding to
    /// justify faulting the rest.
    Uncorroborated {
        stage_id: String,
        resolved: usize,
        agreeing: usize,
    },
    Bound(TableBinding),
}

/// Rows that must both RESOLVE and AGREE before RC-ROW trusts a binding enough
/// to fault the remaining rows. Two independently corroborated rows is the
/// floor: one accidental match is not evidence that a table transcribes an
/// artifact.
const RC_ROW_MIN_CORROBORATING_ROWS: usize = 2;

/// Identify the source artifact a narrative table transcribes, from the table's
/// OWN contents rather than from its caption.
///
/// A candidate (stage, schema) requires (1) the table's header to resolve the
/// schema's entity role through [`resolve_ranking_columns`], and (2) at least
/// one numeric role present on BOTH sides. Among candidates, the winner is the
/// (stage, lookup-column) pair resolving the most rows uniquely AND agreeing on
/// the most of them — ties broken by stage id then column index, so the choice
/// is deterministic.
///
/// The agreement term is what keeps this from false-positive faulting: a table
/// about something else entirely can coincidentally share a few identifiers with
/// a result artifact, but its numbers will not agree with that artifact's, so it
/// is reported as uncorroborated and skipped instead of having every row faulted.
fn bind_narrative_table(
    table: &NarrativeTable,
    schemas: &BTreeMap<String, ResultSchema>,
    sources: &BTreeMap<String, SourceRowIndex>,
    synonyms: &PolicyColumnSynonyms,
    tol: &NarrativeTolerances,
) -> TableBindingOutcome {
    let mut best: Option<TableBinding> = None;
    for (stage_id, source) in sources {
        let Some(schema) = schemas.get(stage_id) else {
            continue;
        };
        let header = canonicalize_header(&table.header, &schema_header_spellings(schema, synonyms));
        let Some(cols) = resolve_ranking_columns(&header, schema, synonyms) else {
            continue;
        };
        let mut roles: Vec<RoleCell> = Vec::new();
        if let (Some(narrative), Some(source_col)) = (cols.effect, source.cols.effect) {
            roles.push(RoleCell {
                narrative,
                source: source_col,
                significance: false,
            });
        }
        if let (Some(narrative), Some(source_col)) = (cols.significance, source.cols.significance) {
            roles.push(RoleCell {
                narrative,
                source: source_col,
                significance: true,
            });
        }
        if roles.is_empty() {
            continue;
        }
        for lookup in 0..table.header.len() {
            if roles.iter().any(|r| r.narrative == lookup) {
                continue;
            }
            let mut resolved = 0usize;
            let mut agreeing = 0usize;
            for cells in &table.rows {
                let Some(key) = cells.get(lookup) else {
                    continue;
                };
                let hits = source.lookup(key);
                if hits.len() != 1 {
                    continue;
                }
                resolved += 1;
                if roles.iter().all(|role| {
                    match (
                        cells
                            .get(role.narrative)
                            .and_then(|c| parse_markdown_number(c)),
                        source.number(hits[0], role.source),
                    ) {
                        (Some(claimed), Some(observed)) => tol.agrees(role, claimed, observed),
                        // Nothing comparable in this cell: it neither
                        // corroborates nor contradicts the binding.
                        _ => true,
                    }
                }) {
                    agreeing += 1;
                }
            }
            let better = best
                .as_ref()
                .is_none_or(|b| (agreeing, resolved) > (b.agreeing, b.resolved));
            if better {
                best = Some(TableBinding {
                    stage_id: stage_id.clone(),
                    artifact: schema.artifact.clone(),
                    lookup,
                    roles: roles.clone(),
                    resolved,
                    agreeing,
                });
            }
        }
    }
    let Some(binding) = best else {
        return TableBindingOutcome::Unresolved;
    };
    let n_rows = table.rows.len();
    let corroborated = binding.agreeing >= RC_ROW_MIN_CORROBORATING_ROWS
        && binding.resolved * 2 > n_rows
        && binding.agreeing * 2 > binding.resolved;
    if corroborated {
        TableBindingOutcome::Bound(binding)
    } else {
        TableBindingOutcome::Uncorroborated {
            stage_id: binding.stage_id,
            resolved: binding.resolved,
            agreeing: binding.agreeing,
        }
    }
}

fn narrative_table_declares_result_roles(
    table: &NarrativeTable,
    schemas: &BTreeMap<String, ResultSchema>,
    synonyms: &PolicyColumnSynonyms,
) -> bool {
    schemas.values().any(|schema| {
        let header = canonicalize_header(&table.header, &schema_header_spellings(schema, synonyms));
        resolve_ranking_columns(&header, schema, synonyms)
            .is_some_and(|columns| columns.effect.is_some() || columns.significance.is_some())
    })
}

/// Disambiguate a narrative row that matched SEVERAL source rows, using the
/// row's other non-numeric cells as additional keys — the composite-key
/// resolution `claim_verifier::verify_keyed_cell` performs (a collection cell
/// plus a term cell), generalized to whatever extra columns the table carries.
/// `None` when the extra cells do not single out exactly one row: an ambiguous
/// row is skipped, never faulted.
fn narrow_by_context(
    cells: &[String],
    binding: &TableBinding,
    source: &SourceRowIndex,
    hits: &[usize],
) -> Option<usize> {
    let context: Vec<&String> = cells
        .iter()
        .enumerate()
        .filter(|(ci, cell)| {
            *ci != binding.lookup
                && !binding.roles.iter().any(|r| r.narrative == *ci)
                && !cell.is_empty()
                && parse_markdown_number(cell).is_none()
        })
        .map(|(_, cell)| cell)
        .collect();
    if context.is_empty() {
        return None;
    }
    let mut narrowed = hits.iter().copied().filter(|row_index| {
        source.rows.get(*row_index).is_some_and(|row| {
            context.iter().all(|want| {
                let want = normalize_cell(want);
                row.iter().any(|v| normalize_cell(v) == want)
            })
        })
    });
    let first = narrowed.next()?;
    narrowed.next().is_none().then_some(first)
}

/// Re-derive every data row of a BOUND narrative table against its source
/// artifact, appending required failures and skip warnings.
fn verify_bound_table(
    site: &str,
    table: &NarrativeTable,
    binding: &TableBinding,
    source: &SourceRowIndex,
    tol: &NarrativeTolerances,
    failures: &mut Vec<String>,
    skipped: &mut Vec<String>,
) {
    let artifact = format!("runtime/outputs/{}/{}", binding.stage_id, binding.artifact);
    for cells in &table.rows {
        let key = cells
            .get(binding.lookup)
            .map(String::as_str)
            .unwrap_or_default();
        if key.is_empty() {
            continue;
        }
        let hits = source.lookup(key);
        if hits.is_empty() {
            failures.push(format!("row `{key}` is not a row of `{artifact}`"));
            continue;
        }
        let row_index = if hits.len() == 1 {
            hits[0]
        } else {
            match narrow_by_context(cells, binding, source, hits) {
                Some(row_index) => row_index,
                None => {
                    skipped.push(format!(
                        "{site} row `{key}` matches {} rows of `{artifact}` — ambiguous, its \
                         numeric cells were not checked",
                        hits.len()
                    ));
                    continue;
                }
            }
        };
        for role in &binding.roles {
            let Some(claimed) = cells
                .get(role.narrative)
                .and_then(|c| parse_markdown_number(c))
            else {
                continue;
            };
            let Some(observed) = source.number(row_index, role.source) else {
                continue;
            };
            if !tol.agrees(role, claimed, observed) {
                failures.push(format!(
                    "row `{key}` states {} = {claimed} but `{artifact}` holds {observed}",
                    source.headers.get(role.source).unwrap_or_default()
                ));
            }
        }
    }
}

enum RankedTableCheck {
    Pass,
    Failure(String),
    Skipped(String),
}

/// Re-derive a narrative "Top N" table from the canonical ranking retained in
/// `report-data.json`. Row keys are first resolved back to source rows, so a
/// table that displays a term alias is compared through the source entity
/// column rather than through presentation spelling.
fn verify_ranked_table(
    table: &NarrativeTable,
    binding: &TableBinding,
    source: &SourceRowIndex,
    ranking: &PathwayRanking,
    claimed_n: usize,
) -> RankedTableCheck {
    if claimed_n == 0 {
        return RankedTableCheck::Failure(
            "caption claims Top 0, which has no ranking meaning".into(),
        );
    }
    let heading = table.heading.to_ascii_lowercase();
    let positive = [
        "enriched",
        "positive",
        "upregulated",
        "up-associated",
        "up associated",
    ]
    .iter()
    .any(|cue| heading.contains(cue));
    let negative = [
        "depleted",
        "negative",
        "downregulated",
        "down-associated",
        "down associated",
    ]
    .iter()
    .any(|cue| heading.contains(cue));

    let (ranked, eligible, class) = if ranking.directional {
        match (positive, negative) {
            (true, false) => (
                ranking.enriched.as_slice(),
                ranking.eligible_enriched,
                "enriched",
            ),
            (false, true) => (
                ranking.depleted.as_slice(),
                ranking.eligible_depleted,
                "depleted",
            ),
            (false, false) => {
                return RankedTableCheck::Skipped(
                    "directional ranking caption does not identify the enriched or depleted class"
                        .into(),
                );
            }
            (true, true) => {
                return RankedTableCheck::Failure(
                    "ranking caption mixes positive and negative classes in one Top-N claim".into(),
                );
            }
        }
    } else {
        if positive || negative {
            return RankedTableCheck::Failure(
                "caption asserts a direction but the source artifact has no resolved signed-effect column"
                    .into(),
            );
        }
        (
            ranking.undirected.as_slice(),
            ranking.eligible_undirected,
            "undirected",
        )
    };

    if claimed_n > ranking.retained_per_class && eligible > ranking.retained_per_class {
        return RankedTableCheck::Failure(format!(
            "caption requests Top {claimed_n} {class} rows but report-data.json retains only the \
             canonical first {} of {eligible} eligible rows",
            ranking.retained_per_class
        ));
    }

    let mut actual: Vec<String> = Vec::new();
    for cells in &table.rows {
        let Some(key) = cells.get(binding.lookup) else {
            return RankedTableCheck::Skipped("ranking row has no lookup cell".into());
        };
        let hits = source.lookup(key);
        let row_index = if hits.len() == 1 {
            hits[0]
        } else if hits.len() > 1 {
            let Some(index) = narrow_by_context(cells, binding, source, hits) else {
                return RankedTableCheck::Skipped(format!(
                    "ranking row `{key}` is ambiguous in the source artifact"
                ));
            };
            index
        } else {
            return RankedTableCheck::Skipped(format!(
                "ranking row `{key}` does not resolve in the source artifact"
            ));
        };
        let Some(entity) = source
            .rows
            .get(row_index)
            .and_then(|row| row.get(source.cols.entity))
        else {
            return RankedTableCheck::Skipped(format!(
                "ranking row `{key}` has no resolved source entity"
            ));
        };
        actual.push(entity.trim().to_string());
    }

    let expected_len = claimed_n.min(eligible);
    let expected: Vec<String> = ranked
        .iter()
        .take(expected_len)
        .map(|term| term.entity.clone())
        .collect();
    if actual == expected {
        RankedTableCheck::Pass
    } else {
        RankedTableCheck::Failure(format!(
            "Top {claimed_n} {class} rows disagree with the canonical ranking; expected [{}], \
             observed [{}]",
            expected.join(", "),
            actual.join(", ")
        ))
    }
}

/// RC-ROW: every data row of an agent-authored markdown table in the narrative
/// must be re-derivable from the source artifact the table transcribes.
///
/// A row whose identifier is absent from that artifact, or whose role-resolved
/// numeric cell contradicts the source cell beyond the policy's transcription
/// tolerance, is a REQUIRED failure — this is the gap RC-COUNT explicitly leaves
/// open (it never parses the narrative), and the failure mode it lets through is
/// the worst one a deposit can ship: a results table that reads as data and is
/// not.
///
/// Every path that cannot establish what a table transcribes is a SKIP with a
/// warning, never a failure: columns that resolve no role, a source artifact
/// absent from disk, a policy declaring no tolerance, a row matching several
/// source rows, a cell that is a bound rather than a value. A false positive
/// here would block a scientifically-correct deposit, which is strictly worse
/// than a miss that the surrounding checks may still catch.
///
/// ORDERING CLAIMS ARE RE-DERIVED from the canonical `ranking` object in
/// `report-data.json`. The assembler owns eligibility and ordering under the
/// declared result schema, so the validator need only resolve the caption's
/// sign class and compare the source entities against the retained prefix.
///
/// Modality-agnostic: the entity / effect / significance roles are resolved by
/// the single [`resolve_ranking_columns`] resolver from the atom's own
/// `result_schema` plus the policy's column synonyms, and the tolerances come
/// from the policy. Nothing here names a feature type, a column, or a method.
fn check_rc_row(package_root: &Path, outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(schemas) = read_report_schemas(package_root) else {
        return;
    };
    let Some(tol) = NarrativeTolerances::load(package_root) else {
        return;
    };
    let synonyms = load_policy_column_synonyms(package_root);
    let report_data = read_report_data(outputs);

    let mut sources: BTreeMap<String, SourceRowIndex> = BTreeMap::new();
    for (stage_id, schema) in &schemas {
        let path = outputs.join(stage_id).join(&schema.artifact);
        if !path.exists() {
            continue;
        }
        let Ok((headers, rows)) = crate::report_contract::assemble::read_table(&path) else {
            continue;
        };
        if let Some(index) = SourceRowIndex::build(headers, rows, schema, &synonyms) {
            sources.insert(stage_id.clone(), index);
        }
    }
    if sources.is_empty() {
        return;
    }

    let ranked_caption = Regex::new(r"(?i)\btop[\s\-]+(\d+)\b")
        .expect("static RC-ROW ranked-caption regex compiles");

    let mut ran = false;
    let mut offenders: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for rel in ["reporting/report.md", "final_reporting/final_report.md"] {
        let (dir, file) = rel
            .split_once('/')
            .expect("static RC-ROW report path has a slash");
        let Ok(text) = std::fs::read_to_string(outputs.join(dir).join(file)) else {
            continue;
        };
        for table in agent_authored_tables(&text) {
            if table.rows.is_empty() {
                continue;
            }
            let site = format!("{rel}:{}", table.line);
            let outcome = bind_narrative_table(&table, &schemas, &sources, &synonyms, &tol);
            if matches!(&outcome, TableBindingOutcome::Unresolved)
                && !narrative_table_declares_result_roles(&table, &schemas, &synonyms)
            {
                continue;
            }
            ran = true;
            match outcome {
                TableBindingOutcome::Unresolved => skipped.push(format!(
                    "{site} table columns resolve no declared result-schema role — its rows were \
                     not checked"
                )),
                TableBindingOutcome::Uncorroborated {
                    stage_id,
                    resolved,
                    agreeing,
                } => skipped.push(format!(
                    "{site} table could not be identified as a transcription of any declared \
                     artifact (closest: {stage_id}, {resolved} of {} row(s) resolved uniquely, \
                     {agreeing} agreeing) — its rows were not checked",
                    table.rows.len()
                )),
                TableBindingOutcome::Bound(binding) => {
                    let source = &sources[&binding.stage_id];
                    if let Some(captures) = ranked_caption.captures(&table.heading) {
                        let claimed_n = captures
                            .get(1)
                            .and_then(|capture| capture.as_str().parse::<usize>().ok());
                        let ranking = report_data
                            .as_ref()
                            .and_then(|data| {
                                data.artifacts
                                    .iter()
                                    .find(|artifact| artifact.stage_id == binding.stage_id)
                            })
                            .and_then(|artifact| artifact.ranking.as_ref());
                        match (claimed_n, ranking) {
                            (Some(n), Some(ranking)) => {
                                match verify_ranked_table(&table, &binding, source, ranking, n) {
                                    RankedTableCheck::Pass => {}
                                    RankedTableCheck::Failure(detail) => {
                                        offenders.push(format!("{site} — {detail}"));
                                    }
                                    RankedTableCheck::Skipped(detail) => skipped.push(format!(
                                        "{site} ranking could not be re-derived — {detail}"
                                    )),
                                }
                            }
                            (None, _) => skipped.push(format!(
                                "{site} ranking caption has no parseable Top-N count"
                            )),
                            (_, None) => skipped.push(format!(
                                "{site} caption asserts a ranking but report-data.json has no \
                                 canonical ranking for `{}`",
                                binding.stage_id
                            )),
                        }
                    }
                    let mut failures: Vec<String> = Vec::new();
                    verify_bound_table(
                        &site,
                        &table,
                        &binding,
                        source,
                        &tol,
                        &mut failures,
                        &mut skipped,
                    );
                    if !failures.is_empty() {
                        offenders.push(format!("{site} — {}", failures.join("; ")));
                    }
                }
            }
        }
    }
    if !ran {
        return;
    }
    report.checked.push("RC-ROW");
    if !offenders.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-ROW",
            severity: Severity::Required,
            detail: format!(
                "a narrative results table asserts row(s) that its own source artifact does not \
                 support (absent identifier, or a cell contradicting the source beyond the \
                 policy's transcription tolerance) — {}",
                offenders.join(" | ")
            ),
        });
    }
    if !skipped.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-ROW",
            severity: Severity::Warn,
            detail: format!(
                "narrative table(s) RC-ROW could not fully re-derive, skipped rather than \
                 faulted — {}",
                skipped.join(" | ")
            ),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report_contract::{rank_artifact, Comparator, Significance};
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

    fn seed_literature_count_package(outputs: &Path, entity_count: u64) {
        write(
            outputs,
            "contextualize_findings_with_literature/claims_evidence_matrix.csv",
            "finding_id,entity,concordance_flag,searched\n\
             F1,GENE1,same_direction,true\n\
             F1,GENE1,same_direction,true\n\
             F2,NA,not_assessed,false\n\
             F3,NA,not_assessed,false\n",
        );
        write(
            outputs,
            "contextualize_findings_with_literature/result.json",
            &serde_json::json!({
                "n_entities_assessed": 1,
                "n_entities_not_assessed": entity_count,
                "n_evidence_rows_assessed": 2,
                "n_evidence_rows_total": 4
            })
            .to_string(),
        );
        write(
            outputs,
            "reporting/report-data.json",
            &serde_json::json!({
                "artifacts": [],
                "literature": {
                    "concordant": [],
                    "discordant": [],
                    "unverifiable": [],
                    "non_replications": [],
                    "novel_count": 0,
                    "not_assessed_count": entity_count,
                    "n_entities_assessed": 1,
                    "n_entities_not_assessed": entity_count,
                    "n_evidence_rows_assessed": 2,
                    "n_evidence_rows_total": 4,
                    "retrieved_sources": []
                }
            })
            .to_string(),
        );
        write(
            outputs,
            "final_reporting/final_report.md",
            &format!(
                "- `n_entities_assessed`: 1\n\
                 - `n_entities_not_assessed`: {entity_count}\n\
                 - `n_evidence_rows_assessed`: 2\n\
                 - `n_evidence_rows_total`: 4\n"
            ),
        );
    }

    #[test]
    fn rc_literature_distinguishes_entities_from_evidence_rows() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        seed_literature_count_package(&outputs, 2);

        let report = check_reporting_invariants(tmp.path());

        assert!(report.checked.contains(&"RC-LITERATURE"));
        assert!(
            report
                .required_failures()
                .iter()
                .all(|failure| !failure.starts_with("RC-LITERATURE:")),
            "{report:?}"
        );
    }

    #[test]
    fn rc_literature_rejects_na_collapse_and_row_count_as_entity_count() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        seed_literature_count_package(&outputs, 4);

        let report = check_reporting_invariants(tmp.path());
        let failures = report.required_failures();

        assert!(
            failures.iter().any(|failure| {
                failure.starts_with("RC-LITERATURE:")
                    && failure.contains("n_entities_not_assessed=4")
                    && failure.contains("recomputed=2")
            }),
            "{failures:?}"
        );
    }

    #[test]
    fn rc_row_ranked_table_must_match_canonical_prefix() {
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
            grouping_column: None,
        };
        let headers = csv::StringRecord::from(vec!["pathway", "NES", "padj"]);
        let rows = vec![
            csv::StringRecord::from(vec!["A", "2.0", "0.01"]),
            csv::StringRecord::from(vec!["B", "3.0", "0.02"]),
            csv::StringRecord::from(vec!["C", "4.0", "0.03"]),
        ];
        let synonyms = PolicyColumnSynonyms::default();
        let ranking =
            rank_artifact(&rows, &headers, &schema, &synonyms, 25).expect("ranking resolves");
        let source =
            SourceRowIndex::build(headers, rows, &schema, &synonyms).expect("source index");
        let binding = TableBinding {
            stage_id: "pathway_enrichment".into(),
            artifact: "pathway_results.tsv".into(),
            lookup: 0,
            roles: vec![
                RoleCell {
                    narrative: 1,
                    source: source.cols.effect.expect("effect"),
                    significance: false,
                },
                RoleCell {
                    narrative: 2,
                    source: source.cols.significance.expect("significance"),
                    significance: true,
                },
            ],
            resolved: 2,
            agreeing: 2,
        };
        let table = |entities: &[(&str, &str, &str)]| NarrativeTable {
            line: 1,
            heading: "Top 2 enriched pathways by canonical ranking".into(),
            header: vec!["Pathway".into(), "NES".into(), "padj".into()],
            rows: entities
                .iter()
                .map(|(entity, effect, significance)| {
                    vec![(*entity).into(), (*effect).into(), (*significance).into()]
                })
                .collect(),
        };

        assert!(matches!(
            verify_ranked_table(
                &table(&[("A", "2.0", "0.01"), ("B", "3.0", "0.02")]),
                &binding,
                &source,
                &ranking,
                2
            ),
            RankedTableCheck::Pass
        ));
        match verify_ranked_table(
            &table(&[("B", "3.0", "0.02"), ("C", "4.0", "0.03")]),
            &binding,
            &source,
            &ranking,
            2,
        ) {
            RankedTableCheck::Failure(detail) => {
                assert!(detail.contains("expected [A, B]"), "{detail}");
                assert!(detail.contains("observed [B, C]"), "{detail}");
            }
            _ => panic!("a noncanonical Top-N prefix must fail"),
        }
    }

    #[test]
    fn rc_row_ignores_metadata_tables_but_keeps_result_tables_in_scope() {
        let schema = ResultSchema {
            artifact: "pathway_results.tsv".into(),
            entity_column: "pathway".into(),
            entity_column_aliases: Vec::new(),
            significance: Some(Significance {
                column: "padj".into(),
                threshold: 0.25,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: Some("NES".into()),
            signed_effect_aliases: Vec::new(),
            grouping_column: Some("collection".into()),
        };
        let schemas = BTreeMap::from([("pathway_enrichment".to_string(), schema)]);
        let synonyms = PolicyColumnSynonyms::default();
        let table = |header: &[&str]| NarrativeTable {
            line: 1,
            heading: String::new(),
            header: header.iter().map(|cell| (*cell).to_string()).collect(),
            rows: vec![vec!["x".into(); header.len()]],
        };

        assert!(!narrative_table_declares_result_roles(
            &table(&["Collection", "Total tested", "Significant"]),
            &schemas,
            &synonyms,
        ));
        assert!(!narrative_table_declares_result_roles(
            &table(&["Absolute log2FC bin", "Count"]),
            &schemas,
            &synonyms,
        ));
        assert!(narrative_table_declares_result_roles(
            &table(&["Pathway", "NES", "padj"]),
            &schemas,
            &synonyms,
        ));
    }

    /// Write a path relative to the PACKAGE ROOT (rather than to
    /// `runtime/outputs`) — needed for `runtime/inputs.json`.
    fn write_root(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// The accession record the himes deposit actually shipped: the study was
    /// published in PLOS ONE, and the bytes came from a Bioconductor data
    /// package rather than from a repository download or an SME local copy.
    fn himes_accession_summary() -> String {
        serde_json::json!({
            "accession": "GSE52778",
            "study_title": "RNA-Seq Transcriptome Profiling Identifies CRISPLD2",
            "publication": {
                "pmid": "24926665",
                "doi": "10.1371/journal.pone.0099625",
                "journal": "PLOS ONE",
                "year": 2014,
                "first_author": "Himes BE"
            },
            "source_package": "airway (Bioconductor)",
            "package_version": "1.30.0",
            "n_samples": 8
        })
        .to_string()
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

    #[test]
    fn rc_collection_requires_exact_table_labels_in_metadata() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "pathway_enrichment/pathway_results.tsv",
            &pathway_results_tsv(&[("HALLMARK", 1), ("KEGG", 1)]),
        );
        write(
            &outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "gene_sets_collections": ["HALLMARK", "KEGG_LEGACY"]
            })
            .to_string(),
        );
        write(
            &outputs,
            "pathway_enrichment/pathway_summary.json",
            &serde_json::json!({
                "collections": ["HALLMARK", "KEGG"]
            })
            .to_string(),
        );

        let report = check_reporting_invariants(tmp.path());
        let failures = report.required_failures();

        assert!(report.checked.contains(&"RC-COLLECTION"));
        assert!(
            failures.iter().any(|failure| {
                failure.starts_with("RC-COLLECTION:") && failure.contains("KEGG_LEGACY")
            }),
            "{failures:?}"
        );
    }

    #[test]
    fn rc_collection_passes_when_both_metadata_surfaces_match() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "pathway_enrichment/pathway_results.tsv",
            &pathway_results_tsv(&[("HALLMARK", 1), ("KEGG", 1)]),
        );
        for (path, key) in [
            ("pathway_enrichment/result.json", "gene_sets_collections"),
            ("pathway_enrichment/pathway_summary.json", "collections"),
        ] {
            write(
                &outputs,
                path,
                &format!(r#"{{"{key}":["HALLMARK","KEGG"]}}"#),
            );
        }

        let report = check_reporting_invariants(tmp.path());

        assert!(report.checked.contains(&"RC-COLLECTION"));
        assert!(
            report
                .required_failures()
                .iter()
                .all(|failure| !failure.starts_with("RC-COLLECTION:")),
            "{report:?}"
        );
    }

    fn seed_ranked_genes(outputs: &Path, narrative_count: u64) {
        write(
            outputs,
            "pathway_enrichment/ranked_genes.tsv",
            "rank\tgene_label\tsource_id\tranking_score\n\
             1\tGENE1\tENSG1\t4.2\n\
             2\tGENE2\tENSG2\t-3.1\n",
        );
        write(
            outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "n_genes_ranked": 2,
                "narrative": format!(
                    "Preranked fgsea used the Wald statistic. {narrative_count} genes with a \
                     valid Wald statistic were included."
                )
            })
            .to_string(),
        );
    }

    fn seed_pathway_stage_narrative(root: &Path, outputs: &Path, claimed_padj: &str) {
        write_root(
            root,
            "policies/interpretation-policy.json",
            include_str!("../../../config/downstream-policy/interpretation-policy.json"),
        );
        write(
            outputs,
            "pathway_enrichment/pathway_results.tsv",
            "pathway\tpval\tpadj\tES\tNES\tsize\tleadingEdge\tcollection\n\
             GENE_SET_A\t1e-6\t6.82e-05\t-0.5\t-1.9024\t20\tA|B\tGO_BP\n",
        );
        write(
            outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "gene_sets_collections": ["GO_BP"],
                "narrative": format!(
                    "The top depleted gene set was GENE_SET_A \
                     (NES=-1.9024, padj={claimed_padj})."
                )
            })
            .to_string(),
        );
        write(
            outputs,
            "pathway_enrichment/pathway_summary.json",
            r#"{"collections":["GO_BP"]}"#,
        );
    }

    #[test]
    fn rc_stage_narrative_rejects_coarse_wrong_adjusted_pvalue() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        seed_pathway_stage_narrative(tmp.path(), &outputs, "1.0e-04");

        let report = check_reporting_invariants(tmp.path());
        let failures = report.required_failures();

        assert!(report.checked.contains(&"RC-STAGE-NARRATIVE"));
        assert!(
            failures.iter().any(|failure| {
                failure.starts_with("RC-STAGE-NARRATIVE:")
                    && failure.contains("GENE_SET_A padj=1.0e-04")
            }),
            "{failures:?}"
        );
    }

    #[test]
    fn rc_stage_narrative_accepts_source_precision() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        seed_pathway_stage_narrative(tmp.path(), &outputs, "6.82e-05");

        let report = check_reporting_invariants(tmp.path());

        assert!(report.checked.contains(&"RC-STAGE-NARRATIVE"));
        assert!(
            report
                .required_failures()
                .iter()
                .all(|failure| !failure.starts_with("RC-STAGE-NARRATIVE:")),
            "{report:?}"
        );
    }

    #[test]
    fn rc_rank_binds_narrative_to_the_retained_vector() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        seed_ranked_genes(&outputs, 3);

        let report = check_reporting_invariants(tmp.path());
        let failures = report.required_failures();

        assert!(report.checked.contains(&"RC-RANK"));
        assert!(
            failures.iter().any(|failure| {
                failure.starts_with("RC-RANK:")
                    && failure.contains("final-ranking count=3")
                    && failure.contains("rows=2")
            }),
            "{failures:?}"
        );
    }

    #[test]
    fn rc_rank_accepts_matching_structured_and_narrative_counts() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        seed_ranked_genes(&outputs, 2);

        let report = check_reporting_invariants(tmp.path());

        assert!(report.checked.contains(&"RC-RANK"));
        assert!(
            report
                .required_failures()
                .iter()
                .all(|failure| !failure.starts_with("RC-RANK:")),
            "{report:?}"
        );
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
        write_report_data(
            &outputs,
            "variant_calling",
            "variants.tsv",
            60,
            Some(50),
            None,
        );

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
    fn rc_sections_matches_spelled_out_qc_abbreviation() {
        // The reporting agent commonly titles the section "Quality Control and
        // Preprocessing" (spelling out QC) rather than "QC Preprocessing". The
        // `qc` id-token must match its universal spelled-out alias so a complete
        // report is not false-flagged as missing the `qc_preprocessing` section
        // (the himes deposit regression).
        assert_eq!(
            section_has_content(
                "## Quality Control and Preprocessing\n\n### Count Matrix\n\n63k genes.\n",
                "qc_preprocessing"
            ),
            Some(true)
        );
        // A heading that mentions neither "qc" nor "quality control" still fails.
        assert_eq!(
            section_has_content("## Preprocessing Notes\n\ntrimming.\n", "qc_preprocessing"),
            None
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

    // -- RC-TABLE ---------------------------------------------------------

    /// A `report-data.json` with one DE-shaped artifact carrying the given
    /// significant entities, `spilled_to_attachment_only` as specified.
    fn report_data_json(entities: &[&str], spilled: bool) -> String {
        let rows: Vec<_> = entities
            .iter()
            .map(|e| {
                serde_json::json!({
                    "entity": e, "effect": 1.0, "significance": 0.01,
                    "literature": {"status": "novel"}
                })
            })
            .collect();
        serde_json::json!({
            "artifacts": [{
                "stage_id": "differential_expression",
                "artifact": "de_results.tsv",
                "n_total": 100,
                "n_significant": entities.len(),
                "direction_split": null,
                "effect_distribution": null,
                "significant_entities": rows,
                "significant_table_path": "reporting/de.significant.tsv",
                "full_table_path": "reporting/de.full.tsv",
                "spilled_to_attachment_only": spilled
            }],
            "literature": null
        })
        .to_string()
    }

    #[test]
    fn rc_table_truncated_terminal_report_is_required_failure() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "reporting/report-data.json",
            &report_data_json(&["ENSG_AAA", "ENSG_BBB", "ENSG_CCC"], false),
        );
        // The terminal report renders only ONE of the three significant entities.
        write(
            &outputs,
            "final_reporting/final_report.md",
            "# Final\n## Primary Results\n| entity |\n| --- |\n| ENSG_AAA |\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            report.checked.contains(&"RC-TABLE"),
            "RC-TABLE must run when an un-spilled artifact and a terminal report are present: {report:?}"
        );
        assert!(
            !report.passed(),
            "a terminal report that renders only 1 of 3 significant entities must be a \
             REQUIRED failure: {report:?}"
        );
        let failures = report.required_failures();
        assert!(
            failures.iter().any(|f| f.contains("RC-TABLE")
                && f.contains("differential_expression")
                && f.contains("2 of 3")),
            "RC-TABLE failure must name the stage and the 2-of-3 missing count: {failures:?}"
        );
    }

    #[test]
    fn rc_table_full_terminal_report_passes() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "reporting/report-data.json",
            &report_data_json(&["ENSG_AAA", "ENSG_BBB", "ENSG_CCC"], false),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "# Final\n## Primary Results\n| ENSG_AAA |\n| ENSG_BBB |\n| ENSG_CCC |\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RC-TABLE"));
        assert!(
            !report
                .required_failures()
                .iter()
                .any(|f| f.contains("RC-TABLE")),
            "a terminal report rendering every significant entity must pass RC-TABLE: {report:?}"
        );
    }

    #[test]
    fn rc_table_spilled_artifact_is_skipped() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        // Spilled: the degenerate-output guard tripped, so a summary is allowed.
        write(
            &outputs,
            "reporting/report-data.json",
            &report_data_json(&["ENSG_AAA", "ENSG_BBB"], true),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "# Final\n## Primary Results\nSummarized: 2 significant genes (set spilled to attachment).\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            !report.checked.contains(&"RC-TABLE"),
            "RC-TABLE must be skipped when the only artifact's set is spilled: {report:?}"
        );
        assert!(report
            .required_failures()
            .iter()
            .all(|f| !f.contains("RC-TABLE")));
    }

    #[test]
    fn rc_table_complete_intermediate_does_not_mask_truncated_terminal() {
        // The exact observed bug: reporting/report.md carries the full table,
        // but the terminal final_reporting/final_report.md was summarized.
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "reporting/report-data.json",
            &report_data_json(&["ENSG_AAA", "ENSG_BBB", "ENSG_CCC"], false),
        );
        // Intermediate report HAS all three...
        write(
            &outputs,
            "reporting/report.md",
            "# Report\n| ENSG_AAA |\n| ENSG_BBB |\n| ENSG_CCC |\n",
        );
        // ...but the terminal report the SME lands on has only one.
        write(
            &outputs,
            "final_reporting/final_report.md",
            "# Final\n## Primary Results\n| ENSG_AAA |\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            !report.passed(),
            "a complete intermediate report must NOT mask a truncated terminal report: {report:?}"
        );
        assert!(
            report
                .required_failures()
                .iter()
                .any(|f| f.contains("RC-TABLE")),
            "RC-TABLE must fault the truncated terminal report despite the complete \
             intermediate: {report:?}"
        );
    }

    // -- RP-PROV ----------------------------------------------------------

    #[test]
    fn rp_prov_flags_journal_contradiction() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "data_acquisition/per_accession_summary.json",
            &himes_accession_summary(),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "## Provenance\n\nGene-level counts for GSE52778 (Himes et al., NEJM 2014) were \
             analysed.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            report.checked.contains(&"RP-PROV"),
            "RP-PROV must run when the package records accession metadata: {report:?}"
        );
        assert!(
            !report.passed(),
            "a journal contradicting the package's own record must block: {report:?}"
        );
        let failures = report.required_failures().join(" | ").to_lowercase();
        assert!(
            failures.contains("rp-prov")
                && failures.contains("nejm")
                && failures.contains("plos one"),
            "RP-PROV must name BOTH the claimed (NEJM) and recorded (PLOS ONE) journal: {failures}"
        );
    }

    #[test]
    fn rp_prov_flags_false_local_copy_claim() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        // No runtime/inputs.json — nothing was ever registered by the SME.
        write(
            &outputs,
            "data_acquisition/per_accession_summary.json",
            &himes_accession_summary(),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "### Data source\n\nGene-level count matrix and sample sheet supplied by the SME \
             from a local copy of GSE52778. Input path: `/home/a/.ecaa-workflow/himes-inputs` \
             (counts.tsv + samples.csv).\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-PROV"));
        assert!(
            !report.passed(),
            "asserting an SME local copy that was never registered must block: {report:?}"
        );
        let failures = report.required_failures().join(" | ");
        assert!(
            failures.contains("RP-PROV")
                && failures.contains("runtime/inputs.json is absent")
                && failures.contains("airway (Bioconductor)"),
            "RP-PROV must name BOTH the false claim and the real recorded source: {failures}"
        );
    }

    #[test]
    fn rp_prov_passes_when_the_narrative_matches_the_package_record() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "data_acquisition/per_accession_summary.json",
            &himes_accession_summary(),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "### Data source\n\nCounts for GSE52778 (Himes et al., PLOS ONE 2014; \
             doi 10.1371/journal.pone.0099625; PMID 24926665) were read from the airway \
             Bioconductor data package.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-PROV"));
        assert!(
            report
                .required_failures()
                .iter()
                .all(|f| !f.contains("RP-PROV")),
            "a faithful provenance narrative must pass: {report:?}"
        );
    }

    #[test]
    fn rp_prov_accepts_an_abbreviated_journal_and_a_registered_local_input() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "data_import/per_accession_summary.json",
            &serde_json::json!({
                "accession": "LOCAL-7",
                "publication": {
                    "journal": "Nature Genetics", "year": 2019, "first_author": "Doe J"
                }
            })
            .to_string(),
        );
        write_root(
            tmp.path(),
            "runtime/inputs.json",
            &serde_json::json!([{
                "input_id": "sme-1", "label": "cohort", "kind": "local_path",
                "root_path": "/data/cohort", "files": [{ "relpath": "a.tsv" }]
            }])
            .to_string(),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "Data supplied by the SME from a local copy (Doe et al., Nat Genet 2019).\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            report
                .required_failures()
                .iter()
                .all(|f| !f.contains("RP-PROV")),
            "an abbreviated journal and a REGISTERED local input must not block: {report:?}"
        );
    }

    #[test]
    fn rp_prov_is_skipped_without_accession_metadata() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "final_reporting/final_report.md",
            "Counts supplied by the SME from a local copy of GSE1 (Doe et al., NEJM 2014).\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            !report.checked.contains(&"RP-PROV"),
            "no accession metadata to compare against → skip, never fail: {report:?}"
        );
        assert!(report.passed());
    }

    #[test]
    fn rp_prov_does_not_fault_the_system_generated_provenance_block() {
        use crate::report_contract::provenance_section::provenance_section;
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "data_acquisition/per_accession_summary.json",
            &himes_accession_summary(),
        );
        write(
            &outputs,
            "data_acquisition/result.json",
            &serde_json::json!({
                "provenance_note": "The SME-specified local copy was absent; data supplied by \
                                    the SME could not be read, so the airway package was used."
            })
            .to_string(),
        );
        let block = provenance_section(tmp.path()).expect("section renders");
        write(
            &outputs,
            "final_reporting/final_report.md",
            &format!("# Final report\n\nNarrative.\n\n{block}"),
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            report
                .required_failures()
                .iter()
                .all(|f| !f.contains("RP-PROV")),
            "the system's own block is the reference, never an assertion under test: {report:?}"
        );
    }

    // -- RP-QC ------------------------------------------------------------

    #[test]
    fn rp_qc_flags_unsupported_no_outlier_claim() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "differential_expression/result.json",
            &serde_json::json!({ "contrast": "treated_vs_control" }).to_string(),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "### QC\n\nSize factors ranged from 0.89 to 1.14. No outlier samples were \
             identified. All eight samples were retained for analysis.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-QC"));
        assert!(
            !report.passed(),
            "a QC-negative claim with no retained artifact must block: {report:?}"
        );
        let failures = report.required_failures().join(" | ");
        assert!(
            failures.contains("RP-QC") && failures.contains("no outlier samples were identified"),
            "RP-QC must quote the unsupported claim: {failures}"
        );
    }

    #[test]
    fn rp_qc_passes_when_outlier_artifact_present() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "final_reporting/final_report.md",
            "### QC\n\nNo outlier samples were identified; see the sample PCA.\n",
        );
        write(&outputs, "normalization/figures/sample_pca.png", "PNG");
        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RP-QC"));
        assert!(
            report
                .required_failures()
                .iter()
                .all(|f| !f.contains("RP-QC")),
            "a retained sample-PCA artifact corroborates the claim: {report:?}"
        );

        // A recorded outlier VERDICT (no file named for it) satisfies it too.
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "final_reporting/final_report.md",
            "The cohort was outlier-free.\n",
        );
        write(
            &outputs,
            "quality_control/result.json",
            &serde_json::json!({ "outlier_samples": [] }).to_string(),
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            report
                .required_failures()
                .iter()
                .all(|f| !f.contains("RP-QC")),
            "a recorded outlier verdict corroborates the claim: {report:?}"
        );
    }

    #[test]
    fn rp_qc_does_not_fire_without_a_qc_negative_claim() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "final_reporting/final_report.md",
            "We did not test for sample outliers, so no outlier removal was attempted.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            report
                .required_failures()
                .iter()
                .all(|f| !f.contains("RP-QC")),
            "an honest 'we did not test' caveat is not a QC-negative assertion: {report:?}"
        );
    }

    // -- RP-1 effect-abundance-ratio prose --------------------------------

    #[test]
    fn rp1_mean_and_sample_attribution_of_the_ratio_warn() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "data_acquisition/per_accession_summary.json",
            &himes_accession_summary(),
        );
        write(
            &outputs,
            "differential_expression/result.json",
            &serde_json::json!({ "top_effect_abundance_ratio": 0.558, "top_effect_k": 15 })
                .to_string(),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "The top 15 genes by effect size had an average normalized count ratio of 0.558 \
             relative to the median (i.e., their mean baseMean sits at ~55.8% of the dataset \
             median), indicating these extreme effects arise in genes with relatively lower \
             average abundance across the 15 samples.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        let warnings = report.warnings().join(" | ");
        assert!(
            warnings.contains("RP-1") && warnings.contains("average"),
            "a mean/average paraphrase of a median/median ratio must warn: {warnings}"
        );
        assert!(
            warnings.contains("15 samples") && warnings.contains("8 sample"),
            "the sample attribution must warn and name the recorded sample count: {warnings}"
        );
        assert!(
            report
                .required_failures()
                .iter()
                .all(|f| !f.contains("RP-1")),
            "RP-1 stays warn-only and must never block: {report:?}"
        );
    }

    #[test]
    fn rp1_faithful_ratio_prose_does_not_warn() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "differential_expression/result.json",
            &serde_json::json!({ "top_effect_abundance_ratio": 0.558 }).to_string(),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "The median abundance of the top-15 features by |effect| is 0.558x the median \
             abundance of the whole tested set, so these extreme effects sit below the \
             background level.\n",
        );
        let report = check_reporting_invariants(tmp.path());
        assert!(
            report.warnings().iter().all(|w| !w.contains("RP-1")),
            "prose that states the median/median definition faithfully must not warn: {report:?}"
        );
    }
}
