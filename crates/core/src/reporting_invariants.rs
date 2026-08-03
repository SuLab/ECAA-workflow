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
//!   * **RC-STAGE-NARRATIVE** numeric row claims in any report-schema stage's
//!     standard result prose must agree with that stage's declared result
//!     artifact under the package policy's role aliases and tolerances.
//!   * **RC-METHOD** a method name embedded in a pathway-level significance
//!     label must match the implementation recorded by the pathway stage.
//!   * **RC-SOFTWARE-VERSION** every package-version pair asserted in report
//!     prose must agree with a version retained in an executed stage's
//!     `env.lock`, `result.json`, or the package-level install/dependency log.
//!     This prevents a reporting agent from filling a reproducibility table
//!     with a plausible version recalled from model memory.
//!   * **RC-METHOD-SELECTION** SME/spec preference and automatic-advance claims
//!     must match the booleans retained by the corresponding `discover_*`
//!     decision for every method family.
//!   * **RC-FILTER-POPULATION** when any stage records distinct source and
//!     retained population counts, the report may not introduce the retained
//!     count as the unqualified input population. The retained population
//!     must be identified as filtered, analysis-ready, tested, or as input to
//!     a specifically named downstream model.
//!   * **RC-FINAL-FIDELITY** when both report stages ran, the agent-authored
//!     portion of terminal `final_report.md` must contain the complete
//!     agent-authored `reporting/report.md` byte-for-byte as one contiguous
//!     block. Deterministic system-owned blocks are removed from both files
//!     before comparison because they are injected after task execution. The
//!     final stage may wrap the validated block with non-scientific navigation,
//!     but may not silently rewrite a validated row or sentence.
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
//!   * **RC-METRIC-DEFINITION** every reported stage metric that retains a
//!     sibling definition and computation basis must preserve that definition,
//!     agree with the basis value, and state the value's relation to any
//!     explicitly retained neutral reference. Discovery is structural at any
//!     JSON object depth and does not depend on a task id, modality, metric
//!     name, or a hard-coded neutral value.
//!   * **RC-LITERATURE** every literature entity count in `report-data.json`,
//!     the contextualization result, and any explicitly named narrative claim
//!     must equal a fresh recomputation from `claims_evidence_matrix.csv`.
//!     Evidence-row counts stay separate from distinct-entity counts, and
//!     missing entity labels fall back to the row's finding identifier.
//!   * **RC-LITERATURE-LINK** every explicit entity-to-literature-source
//!     attribution in report prose must resolve to that exact pair in
//!     `claims_evidence_matrix.csv`. A source carried by one entity cannot be
//!     distributed across a prose list of neighboring entities.
//!   * **RC-IDENTITY** a `direction_split`'s `up + down` must not EXCEED
//!     `n_significant` (directional rows can't outnumber the significant
//!     set). A shortfall is legitimate — a significant row with a zero/NA
//!     effect counts in `n_significant` but in neither `up` nor `down`.
//!     Artifacts with no split (unsigned modalities, e.g. variant calling)
//!     are skipped, never faulted.
//!   * **RC-SECTIONS** every `required_report_sections` id declared on the
//!     `reporting`/`final_reporting` task specs must appear as a non-empty
//!     section in the emitted report.
//!   * **RC-ATTACHMENT** a generated `.significant.tsv` attachment must be
//!     described with the `ResultSchema::significance` filter that actually
//!     selected its rows, without an invented additional effect-size cutoff.
//!   * **RC-THRESHOLD** an unambiguous narrative selection rule must preserve
//!     the significance field, strict comparator, and cutoff declared by the
//!     executable result schema. Per-entity bounds and markdown data rows are
//!     not interpreted as selection rules.
//!   * **RC-ROW** every DATA ROW of a markdown table in the narrative must be
//!     re-derivable from the source artifact the table transcribes: its
//!     identifier must be a row of that artifact, and every cell whose column
//!     resolved to a role (effect / significance, via the one
//!     [`crate::report_contract::resolve_ranking_columns`] resolver) must match
//!     the source cell within the transcription tolerance the package's own
//!     `interpretation-policy.json` declares. A missing-value placeholder is a
//!     failure when the source cell is finite. A deposited report shipped a
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
//!     caption ("Top 10 …" or "Top enriched …") are re-derived from the
//!     canonical `report-data.json::ranking` prefix; without an explicit N,
//!     the table's displayed row count supplies N.
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
//!     artifact in the package, and a report may not deny a particular QC
//!     artifact that the package retains. The deposited report asserted the
//!     absence of outliers while the package contained no sample-level QC
//!     artifact of any kind — the only sample statistic was a size-factor
//!     range.
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
    load_policy_column_synonyms, resolve_ranking_columns, summarize_artifact, Comparator,
    PathwayRanking, PolicyColumnSynonyms, RankingColumns, ReportData, ResultSchema, FULL_TABLE_END,
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

    check_rc_self_describing_metrics(&outputs, &mut report);
    check_rp1_effect_direction(package_root, &outputs, &mut report);
    check_rp2_gene_sets_tested(&outputs, &mut report);
    check_rc_pathway_collections(&outputs, &mut report);
    check_rc_pathway_rank(&outputs, &mut report);
    check_rc_stage_narrative(package_root, &outputs, &mut report);
    check_rc_pathway_method(&outputs, &mut report);
    check_rc_software_versions(&outputs, &mut report);
    check_rc_method_selection_provenance(&outputs, &mut report);
    check_rc_filter_population(&outputs, &mut report);
    check_rc_final_report_fidelity(&outputs, &mut report);
    check_rp3_fdr_family(&outputs, &mut report);
    check_rp4_mapping_reconciliation(&outputs, &mut report);
    check_rp5_figure_caption_shape(&outputs, &mut report);
    check_rp9_method_label(&outputs, &mut report);
    check_rp_prov_data_source(package_root, &outputs, &mut report);
    check_rp_qc_negative_claim(&outputs, &mut report);
    check_rc_attachment_filter_claim(package_root, &outputs, &mut report);
    check_rc_threshold_contract(package_root, &outputs, &mut report);
    check_rc_count(package_root, &outputs, &mut report);
    check_rc_literature_counts(package_root, &outputs, &mut report);
    check_rc_literature_claim_links(package_root, &outputs, &mut report);
    check_rc_identity(&outputs, &mut report);
    check_rc_sections(package_root, &outputs, &mut report);
    check_rc_table(&outputs, &mut report);
    check_rc_row(package_root, &outputs, &mut report);

    report
}

// ---------------------------------------------------------------------------
// RC-THRESHOLD (Required) — prose preserves the executable comparator
// ---------------------------------------------------------------------------

/// RC-THRESHOLD: every unambiguous selection-threshold statement must preserve
/// the exact field, comparator, and cutoff from the artifact's executable
/// [`ResultSchema`]. In particular, strict `lt`/`gt` contracts cannot be
/// narrated as inclusive `≤`/`≥` rules.
///
/// Significance-column aliases come from the package policy, so this check is
/// independent of modality and analysis archetype. If one display name maps
/// to genuinely different schema contracts in the same workflow, the check
/// abstains unless the prose disambiguates it elsewhere; it never guesses
/// which stage the writer meant.
fn check_rc_threshold_contract(
    package_root: &Path,
    outputs: &Path,
    report: &mut ReportingInvariantsReport,
) {
    let Some(report_data) = read_report_data(outputs) else {
        return;
    };
    let Some(prose) = read_agent_report_prose(outputs) else {
        return;
    };
    let workflow_schemas = read_report_schemas(package_root).unwrap_or_default();
    let synonyms = load_policy_column_synonyms(package_root);

    #[derive(Clone)]
    struct ThresholdContract {
        stage: String,
        field: String,
        comparator: Comparator,
        cutoff: f64,
    }

    let mut exact_by_name: BTreeMap<String, Vec<ThresholdContract>> = BTreeMap::new();
    let mut alias_by_name: BTreeMap<String, Vec<ThresholdContract>> = BTreeMap::new();
    for artifact in &report_data.artifacts {
        let schema = workflow_schemas
            .get(&artifact.stage_id)
            .or(artifact.result_schema.as_ref());
        let Some(significance) = schema.and_then(|schema| schema.significance.as_ref()) else {
            continue;
        };
        let contract = ThresholdContract {
            stage: artifact.stage_id.clone(),
            field: significance.column.clone(),
            comparator: significance.comparator,
            cutoff: significance.threshold,
        };
        exact_by_name
            .entry(significance.column.to_ascii_lowercase())
            .or_default()
            .push(contract.clone());
        for name in &synonyms.significance {
            alias_by_name
                .entry(name.to_ascii_lowercase())
                .or_default()
                .push(contract.clone());
        }
    }
    // An exact executable column name is stage-specific evidence. A generic
    // policy alias is used only when no schema declares that spelling, so a
    // second stage with a different cutoff cannot make the first stage's
    // canonical field name spuriously ambiguous.
    for (name, contracts) in alias_by_name {
        exact_by_name.entry(name).or_insert(contracts);
    }
    let by_name = exact_by_name;
    if by_name.is_empty() {
        return;
    }
    report.checked.push("RC-THRESHOLD");

    let broad_selection_cue = Regex::new(
        r"(?i)\b(?:significan\w*|threshold|cutoff|criterion|criteria|selected|selection|filter(?:ed|ing)?|fdr)\b",
    )
    .expect("static threshold-selection cue regex compiles");
    let explicit_rule_cue = Regex::new(
        r"(?i)\b(?:threshold|cutoff|criterion|criteria|selection|filter(?:ed|ing)?|defined|declared|classified|qualif(?:y|ied|ication)|required)\b",
    )
    .expect("static explicit threshold-rule cue regex compiles");
    let mut offenders = BTreeSet::new();
    for (name, contracts) in by_name {
        let escaped = regex::escape(&name);
        let pattern = format!(
            r"(?i)\b{escaped}\b[\s`*_()\[\]-]{{0,16}}(<=|>=|≤|≥|<|>)\s*([+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?)"
        );
        let Ok(threshold_re) = Regex::new(&pattern) else {
            continue;
        };

        let unique_contracts: BTreeSet<(&'static str, u64)> = contracts
            .iter()
            .map(|contract| {
                (
                    match contract.comparator {
                        Comparator::Lt => "lt",
                        Comparator::Gt => "gt",
                    },
                    contract.cutoff.to_bits(),
                )
            })
            .collect();
        if unique_contracts.len() != 1 {
            continue;
        }
        let contract = &contracts[0];
        let expected_symbol = match contract.comparator {
            Comparator::Lt => "<",
            Comparator::Gt => ">",
        };

        for line in prose.lines().filter(|line| {
            broad_selection_cue.is_match(line) && !line.trim_start().starts_with('|')
        }) {
            for captures in threshold_re.captures_iter(line) {
                let Some(claimed_symbol) = captures.get(1).map(|value| value.as_str()) else {
                    continue;
                };
                let Some(claimed_cutoff) = captures
                    .get(2)
                    .and_then(|value| value.as_str().parse::<f64>().ok())
                else {
                    continue;
                };
                let scale = contract.cutoff.abs().max(claimed_cutoff.abs()).max(1.0);
                let cutoff_agrees =
                    (claimed_cutoff - contract.cutoff).abs() <= f64::EPSILON * 8.0 * scale;
                let comparator_disagrees = claimed_symbol != expected_symbol;
                // A different numeric bound in entity-level prose can be the
                // observed value rather than the selection cutoff. Require an
                // explicit rule cue before treating that case as a contract
                // assertion. When the stated cutoff equals the executable
                // cutoff, a broad significance cue is sufficient to catch an
                // inclusive rendering of a strict rule.
                let asserts_selection_rule =
                    explicit_rule_cue.is_match(line) || (cutoff_agrees && comparator_disagrees);
                if asserts_selection_rule && (comparator_disagrees || !cutoff_agrees) {
                    let stages = contracts
                        .iter()
                        .map(|candidate| candidate.stage.as_str())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>()
                        .join(", ");
                    offenders.insert(format!(
                        "`{name}` is declared as `{}` {expected_symbol} {} for stage(s) \
                         {stages}, but the report states `{name} {claimed_symbol} \
                         {claimed_cutoff}` in: {}",
                        contract.field,
                        contract.cutoff,
                        line.trim()
                    ));
                }
            }
        }
    }

    if !offenders.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-THRESHOLD",
            severity: Severity::Required,
            detail: format!(
                "report threshold prose disagrees with the executable result schema: {}",
                offenders.into_iter().collect::<Vec<_>>().join(" | ")
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// RC-ATTACHMENT (Required) — attachment description matches its generator
// ---------------------------------------------------------------------------

/// RC-ATTACHMENT: a report may describe the deterministically generated
/// `<artifact>.significant.tsv` only with the atom-declared significance
/// filter. [`crate::report_contract::report_data::write_supplementary`] writes
/// exactly the rows selected by `ResultSchema::significance`; it never adds an
/// effect-size cutoff. A report that labels this attachment with an additional
/// absolute-effect threshold therefore misdescribes the file even when some
/// separate stage summary happens to use that stricter threshold.
fn check_rc_attachment_filter_claim(
    package_root: &Path,
    outputs: &Path,
    report: &mut ReportingInvariantsReport,
) {
    let Some(schemas) = read_report_schemas(package_root) else {
        return;
    };
    let Some(report_data) = read_report_data(outputs) else {
        return;
    };
    let Some(prose) = read_agent_report_prose(outputs) else {
        return;
    };
    let synonyms = load_policy_column_synonyms(package_root);
    let mut offenders = BTreeSet::new();

    for artifact in &report_data.artifacts {
        let Some(schema) = schemas.get(&artifact.stage_id) else {
            continue;
        };
        let Some(significance) = schema.significance.as_ref() else {
            continue;
        };
        let Some(file_name) = Path::new(&artifact.significant_table_path)
            .file_name()
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        let mut effect_names = Vec::new();
        if let Some(name) = schema.signed_effect_column.as_ref() {
            effect_names.push(name.clone());
        }
        effect_names.extend(schema.signed_effect_aliases.iter().cloned());
        effect_names.extend(synonyms.effect.iter().cloned());
        effect_names.sort_by_key(|name| name.to_ascii_lowercase());
        effect_names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
        if effect_names.is_empty() {
            continue;
        }

        for line in prose.lines().filter(|line| line.contains(file_name)) {
            let asserts_effect_cutoff = effect_names.iter().any(|name| {
                let escaped = regex::escape(name);
                let pattern = format!(
                    r"(?i)(?:\|\s*{escaped}\s*\||\babs(?:olute)?(?:\s+value\s+of)?\s+{escaped}\b)(?:\s+(?:magnitude|value))?\s*(?:<=|>=|<|>|≤|≥)"
                );
                Regex::new(&pattern)
                    .expect("escaped effect name yields a valid regex")
                    .is_match(line)
            });
            if asserts_effect_cutoff {
                let comparator = match significance.comparator {
                    Comparator::Lt => "<",
                    Comparator::Gt => ">",
                };
                offenders.insert(format!(
                    "`{}` is generated with {} {comparator} {} only, but the report adds an \
                     effect-size cutoff in: {}",
                    artifact.significant_table_path,
                    significance.column,
                    significance.threshold,
                    line.trim()
                ));
            }
        }
    }

    if report_data
        .artifacts
        .iter()
        .any(|artifact| !artifact.significant_table_path.is_empty())
    {
        report.checked.push("RC-ATTACHMENT");
    }
    if !offenders.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-ATTACHMENT",
            severity: Severity::Required,
            detail: format!(
                "report misdescribes a generated significant-results attachment: {}",
                offenders.into_iter().collect::<Vec<_>>().join(" | ")
            ),
        });
    }
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
    let expected_normalized: BTreeSet<String> = expected
        .iter()
        .map(|value| normalize_label(value))
        .collect();
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
            Some(observed)
                if observed
                    .iter()
                    .map(|value| normalize_label(value))
                    .collect::<BTreeSet<_>>()
                    == expected_normalized => {}
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
        let matched_text = matched.as_str().to_ascii_lowercase();
        if matched_text.contains("duplicate") {
            // A clause such as "removed 32 duplicate gene-symbol labels,
            // 17,177 genes were ranked" contains two gene counts.  The first
            // describes deduplication loss, not the final ranking population.
            continue;
        }
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
    let mut expected_duplicate_removals = None;
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

    let mapping_path = pathway_dir.join("annotation").join("symbol_map.tsv");
    if mapping_path.exists() {
        match crate::report_contract::assemble::read_table(&mapping_path) {
            Err(error) => structural.push(format!(
                "annotation/symbol_map.tsv cannot be parsed as a delimited table: {error}"
            )),
            Ok((mapping_headers, mapping_rows)) => {
                let observed_headers: Vec<&str> = mapping_headers.iter().collect();
                if observed_headers != ["symbol", "ensembl_gene_id"] {
                    structural.push(format!(
                        "annotation/symbol_map.tsv header is {:?}, expected [symbol, \
                         ensembl_gene_id] in that order",
                        observed_headers
                    ));
                }
                let symbol_idx = mapping_headers.iter().position(|header| header == "symbol");
                let accession_idx = mapping_headers
                    .iter()
                    .position(|header| header == "ensembl_gene_id");
                if let (Some(symbol_idx), Some(accession_idx)) = (symbol_idx, accession_idx) {
                    let mut previous_accession: Option<&str> = None;
                    let mut mappings = BTreeSet::new();
                    for (offset, row) in mapping_rows.iter().enumerate() {
                        let symbol = row.get(symbol_idx).unwrap_or("").trim();
                        let accession = row.get(accession_idx).unwrap_or("").trim();
                        if symbol.is_empty() || accession.is_empty() {
                            structural.push(format!(
                                "annotation/symbol_map.tsv row {} has an empty symbol or \
                                 ensembl_gene_id",
                                offset + 2
                            ));
                        }
                        if !mappings.insert((symbol.to_string(), accession.to_string())) {
                            structural.push(format!(
                                "annotation/symbol_map.tsv repeats mapping {symbol:?} -> \
                                 {accession:?}"
                            ));
                        }
                        if previous_accession.is_some_and(|previous| previous > accession) {
                            structural.push(
                                "annotation/symbol_map.tsv is not sorted by ensembl_gene_id"
                                    .to_string(),
                            );
                        }
                        previous_accession = Some(accession);
                    }
                }

                let mapped = mapping_rows.len() as u64;
                let pre_mapping = result
                    .as_ref()
                    .and_then(|value| json_named_u64(value, "n_genes_pre_mapping"));
                match pre_mapping {
                    None => {
                        structural.push("result.json n_genes_pre_mapping is missing".to_string())
                    }
                    Some(pre_mapping) if pre_mapping < mapped => structural.push(format!(
                        "result.json n_genes_pre_mapping={pre_mapping} is smaller than \
                         annotation/symbol_map.tsv rows={mapped}"
                    )),
                    Some(pre_mapping) => {
                        let expected_unmapped = pre_mapping - mapped;
                        for (field, expected) in [
                            ("n_genes_mapped", mapped),
                            ("n_genes_unmapped", expected_unmapped),
                        ] {
                            match result
                                .as_ref()
                                .and_then(|value| json_named_u64(value, field))
                            {
                                Some(observed) if observed == expected => {}
                                Some(observed) => structural.push(format!(
                                    "result.json {field}={observed}, recomputed={expected}"
                                )),
                                None => {
                                    structural.push(format!("result.json {field} is missing"));
                                }
                            }
                        }
                    }
                }
                if mapped < expected_count {
                    structural.push(format!(
                        "annotation/symbol_map.tsv rows={mapped} is smaller than \
                         ranked_genes.tsv rows={expected_count}"
                    ));
                } else {
                    let expected_duplicates = mapped - expected_count;
                    expected_duplicate_removals = Some(expected_duplicates);
                    match result
                        .as_ref()
                        .and_then(|value| json_named_u64(value, "n_duplicate_gene_labels_removed"))
                    {
                        Some(observed) if observed == expected_duplicates => {}
                        Some(observed) => structural.push(format!(
                            "result.json n_duplicate_gene_labels_removed={observed}, \
                             recomputed={expected_duplicates}"
                        )),
                        None => structural.push(
                            "result.json n_duplicate_gene_labels_removed is missing".to_string(),
                        ),
                    }
                }
            }
        }
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
    if let Some(expected_duplicates) = expected_duplicate_removals {
        let duplicate_claim = Regex::new(
            r"(?i)\b(?:removing|removed|removal\s+of)\s+(\d[\d,]*)\s+duplicate(?:\s+gene[\s-]?symbol)?\s+labels?\b",
        )
        .expect("static duplicate-removal regex compiles");
        for captures in duplicate_claim.captures_iter(&narrative) {
            let Some(observed) = captures
                .get(1)
                .and_then(|value| parse_grouped_int(value.as_str()))
            else {
                continue;
            };
            if observed != expected_duplicates {
                structural.push(format!(
                    "narrative duplicate-label removal count={observed}, recomputed={expected_duplicates}"
                ));
            }
        }
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

/// RC-STAGE-NARRATIVE: bind numeric row claims in every report-schema stage's
/// own standard result prose (`narrative`, `summary`, or `interpretation`) to
/// that stage's declared result artifact. Final-report tables are covered
/// separately by RC-ROW; this closes the same gap for compute-stage prose
/// without naming a modality, entity kind, method, or scientific column.
fn check_rc_stage_narrative(
    package_root: &Path,
    outputs: &Path,
    report: &mut ReportingInvariantsReport,
) {
    let Some(schemas) = read_report_schemas(package_root) else {
        return;
    };
    let cfg = [package_root.join("policies"), package_root.join("config")]
        .into_iter()
        .find_map(|dir| {
            let path =
                crate::claim_extractor::resolve_policy_file(&dir, "interpretation-policy.json")?;
            let raw = std::fs::read_to_string(path).ok()?;
            let policy = serde_json::from_str::<Value>(&raw).ok()?;
            crate::claim_extractor::ExtractorConfig::from_policy(&policy).ok()
        });
    let Some(mut cfg) = cfg else {
        return;
    };
    for schema in schemas.values() {
        for name in std::iter::once(schema.entity_column.as_str())
            .chain(schema.entity_column_aliases.iter().map(String::as_str))
        {
            if !cfg.entity_columns.iter().any(|column| column == name) {
                cfg.entity_columns.push(name.to_string());
            }
        }
        for name in schema
            .signed_effect_column
            .iter()
            .map(String::as_str)
            .chain(schema.signed_effect_aliases.iter().map(String::as_str))
        {
            if !cfg.effect_size_columns.iter().any(|column| column == name) {
                cfg.effect_size_columns.push(name.to_string());
            }
        }
        if let Some(significance) = schema.significance.as_ref() {
            if !cfg
                .pvalue_columns
                .iter()
                .any(|column| column == &significance.column)
            {
                cfg.pvalue_columns.push(significance.column.clone());
            }
        }
    }

    let mut failures = BTreeSet::new();
    let mut ran = false;
    for (stage_id, schema) in schemas {
        let stage_dir = outputs.join(&stage_id);
        if !stage_dir.join(&schema.artifact).is_file() {
            continue;
        }
        let Some(result) = read_json(&stage_dir.join("result.json")) else {
            continue;
        };
        let prose = ["narrative", "narrative_text", "summary", "interpretation"]
            .into_iter()
            .filter_map(|field| result.get(field).and_then(Value::as_str))
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if prose.is_empty() {
            continue;
        }
        let mut claims: Vec<_> = crate::claim_extractor::extract_claims(&prose, &cfg)
            .into_iter()
            .filter(|claim| claim.effect_size.is_some() || claim.pvalue.is_some())
            .collect();
        for claim in &mut claims {
            claim.source_table = Some(schema.artifact.clone());
            // This invariant checks the stage's numeric row assertion against
            // its declared result artifact. A PMID in the same sentence can
            // make the general extractor classify the whole sentence as
            // LiteratureGrounded; retain literature adjudication in the main
            // claim verifier, but use the numeric contract here so absence of
            // a literature matrix cannot mask or falsely fail the row check.
            if claim.contract == crate::claim_contract::ClaimContract::LiteratureGrounded {
                claim.contract = crate::claim_contract::ClaimContract::NumericTableLookup;
            }
        }
        if claims.is_empty() {
            continue;
        }
        ran = true;
        for verdict in crate::claim_verifier::verify_claims_with_discovery(
            &claims,
            &stage_dir,
            package_root,
            &cfg,
        ) {
            match verdict.status {
                crate::claim_verifier::ClaimStatus::Verified => {}
                crate::claim_verifier::ClaimStatus::Mismatch { detail } => {
                    failures.insert(format!(
                        "{stage_id} entity `{}`: {detail}",
                        verdict.claim.entity
                    ));
                }
                crate::claim_verifier::ClaimStatus::Unverifiable { reason }
                | crate::claim_verifier::ClaimStatus::Pending { reason }
                | crate::claim_verifier::ClaimStatus::Suspicious { reason } => {
                    failures.insert(format!(
                        "{stage_id} entity `{}` could not be checked: {reason}",
                        verdict.claim.entity
                    ));
                }
            }
        }
    }
    if !ran {
        return;
    }
    report.checked.push("RC-STAGE-NARRATIVE");
    if !failures.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-STAGE-NARRATIVE",
            severity: Severity::Required,
            detail: format!(
                "compute-stage result prose disagrees with its declared result artifact: {}",
                failures.into_iter().collect::<Vec<_>>().join("; ")
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
// RC-METRIC-DEFINITION (Required) — self-describing metrics stay source-exact
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct SelfDescribingMetric {
    key: String,
    field_path: String,
    value: f64,
    description: String,
    basis: serde_json::Map<String, Value>,
}

/// Discover self-describing metrics at any object depth. Sibling lookup stays
/// local to each object, so an unrelated description or basis elsewhere in a
/// stage result can never be attached to the numeric field.
fn collect_self_describing_metrics(
    value: &Value,
    prefix: &mut Vec<String>,
    out: &mut Vec<SelfDescribingMetric>,
) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if let Some(metric_value) = child.as_f64().filter(|value| value.is_finite()) {
                    let description = object
                        .get(&format!("{key}_description"))
                        .and_then(Value::as_str)
                        .filter(|description| !description.trim().is_empty());
                    let basis = object
                        .get(&format!("{key}_basis"))
                        .and_then(Value::as_object);
                    if let (Some(description), Some(basis)) = (description, basis) {
                        let mut field_path = prefix.clone();
                        field_path.push(key.clone());
                        out.push(SelfDescribingMetric {
                            key: key.clone(),
                            field_path: field_path.join("."),
                            value: metric_value,
                            description: description.to_string(),
                            basis: basis.clone(),
                        });
                    }
                }
                prefix.push(key.clone());
                collect_self_describing_metrics(child, prefix, out);
                prefix.pop();
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                prefix.push(index.to_string());
                collect_self_describing_metrics(child, prefix, out);
                prefix.pop();
            }
        }
        _ => {}
    }
}

/// Enforce the reporting contract for any stage metric that retains both a
/// sibling `<metric>_description` string and `<metric>_basis` object.
///
/// The producing stage has already named the statistic, numerator,
/// denominator, populations, and units. A report that cites the metric key must
/// therefore carry that retained definition verbatim after whitespace
/// normalization. This is discovered from result structure, not from task,
/// modality, or column names. A metric whose basis explicitly declares a
/// finite `neutral_reference` must also state whether its value is above,
/// below, or equal to that reference.
fn check_rc_self_describing_metrics(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let mut surfaces: Vec<(String, String)> = [
        ("reporting/report.md", outputs.join("reporting/report.md")),
        (
            "final_reporting/final_report.md",
            outputs.join("final_reporting/final_report.md"),
        ),
    ]
    .into_iter()
    .filter_map(|(label, path)| {
        std::fs::read_to_string(path)
            .ok()
            .map(|text| (label.to_string(), strip_provenance_section(&text)))
    })
    .collect();
    if surfaces.is_empty() {
        return;
    }
    surfaces.sort_by(|left, right| left.0.cmp(&right.0));

    let Ok(entries) = std::fs::read_dir(outputs) else {
        return;
    };
    let mut task_dirs: Vec<_> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    task_dirs.sort();

    let normalize_whitespace = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut findings = BTreeSet::new();
    let mut checked = false;
    for task_dir in task_dirs {
        let Some(task_id) = task_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(result) = read_json(&task_dir.join("result.json")) else {
            continue;
        };
        let mut metrics = Vec::new();
        collect_self_describing_metrics(&result, &mut Vec::new(), &mut metrics);
        metrics.sort_by(|left, right| left.field_path.cmp(&right.field_path));
        for retained in metrics {
            let SelfDescribingMetric {
                key: metric,
                field_path,
                value,
                description,
                basis,
            } = retained;
            if basis.get("computed").and_then(Value::as_bool) == Some(false) {
                continue;
            }

            if let Some(basis_value) = basis.get("value").and_then(Value::as_f64) {
                if !basis_value.is_finite()
                    || (basis_value - value).abs()
                        > f64::EPSILON * value.abs().max(basis_value.abs()).max(1.0)
                {
                    checked = true;
                    findings.insert(format!(
                        "stage `{task_id}` metric `{field_path}` has value {value}, but its \
                         retained basis records {basis_value}"
                    ));
                }
            }

            let normalized_description = normalize_whitespace(&description);
            let neutral_reference = basis
                .get("neutral_reference")
                .and_then(Value::as_f64)
                .filter(|reference| reference.is_finite());
            for (surface, prose) in &surfaces {
                let prose_lower = prose.to_ascii_lowercase();
                let metric_lower = metric.to_ascii_lowercase();
                let field_path_lower = field_path.to_ascii_lowercase();
                let anchors = {
                    let mut anchors = find_all(&prose_lower, &field_path_lower);
                    if field_path_lower != metric_lower {
                        anchors.extend(find_all(&prose_lower, &metric_lower));
                    }
                    // Human prose normally renders each JSON-key separator as
                    // either whitespace or a hyphen, and writers can mix the
                    // two in one label (for example `effect-size ratio`).
                    // Treat every such rendering as the same metric anchor so
                    // prettifying a field name cannot evade its retained
                    // definition.
                    let flexible_metric = metric_lower
                        .split('_')
                        .filter(|part| !part.is_empty())
                        .map(regex::escape)
                        .collect::<Vec<_>>()
                        .join(r"[\s_-]+");
                    if let Ok(pattern) = Regex::new(&flexible_metric) {
                        anchors.extend(pattern.find_iter(&prose_lower).map(|hit| hit.start()));
                    }
                    anchors.sort_unstable();
                    anchors.dedup();
                    anchors
                };
                if anchors.is_empty() {
                    continue;
                }
                checked = true;
                let normalized_prose = normalize_whitespace(prose);
                if !normalized_prose.contains(&normalized_description) {
                    findings.insert(format!(
                        "{surface} cites `{metric}` but does not preserve the producing stage's \
                         retained definition verbatim"
                    ));
                }

                let retained_semantics = format!(
                    "{} {}",
                    description.to_ascii_lowercase(),
                    serde_json::to_string(&basis)
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                );
                for anchor in &anchors {
                    let clause = clause_around(&prose_lower, *anchor);
                    for cue in [
                        "reliable",
                        "reliability",
                        "precise",
                        "precision",
                        "high confidence",
                        "well-estimated",
                        "well estimated",
                        "robust estimate",
                    ] {
                        if clause.contains(cue) && !retained_semantics.contains(cue) {
                            findings.insert(format!(
                                "{surface} infers `{cue}` from metric `{metric}`, but that \
                                 interpretation is absent from the producing stage's retained \
                                 definition and basis"
                            ));
                        }
                    }
                }

                if let Some(neutral_reference) = neutral_reference {
                    let mut says_above = false;
                    let mut says_below = false;
                    let mut says_equal = false;
                    for anchor in anchors {
                        let window = clause_around(&prose_lower, anchor);
                        says_above |= EFFECT_DIRECTION_ABOVE
                            .iter()
                            .any(|word| window.contains(word));
                        says_below |= EFFECT_DIRECTION_BELOW
                            .iter()
                            .any(|word| window.contains(word));
                        says_equal |= ["equal to", "equals", "the same as"]
                            .iter()
                            .any(|word| window.contains(word));
                    }
                    let relation_ok = if value < neutral_reference {
                        says_below && !says_above
                    } else if value > neutral_reference {
                        says_above && !says_below
                    } else {
                        says_equal && !says_above && !says_below
                    };
                    if !relation_ok {
                        let expected = if value < neutral_reference {
                            "below"
                        } else if value > neutral_reference {
                            "above"
                        } else {
                            "equal to"
                        };
                        findings.insert(format!(
                            "{surface} cites metric `{metric}` = {value} without an unambiguous \
                             statement that it is {expected} its retained neutral reference \
                             {neutral_reference}"
                        ));
                    }
                }
            }
        }
    }

    if checked {
        report.checked.push("RC-METRIC-DEFINITION");
    }
    if !findings.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-METRIC-DEFINITION",
            severity: Severity::Required,
            detail: findings.into_iter().collect::<Vec<_>>().join(" | "),
        });
    }
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
// RC-METHOD (Required) — pathway significance label matches implementation
// ---------------------------------------------------------------------------

/// Return the canonical implementation token recorded by the pathway stage.
/// `GSEA` itself names an analysis family rather than one implementation, so
/// only unambiguous implementation names participate in this check.
fn recorded_pathway_implementation(outputs: &Path) -> Option<&'static str> {
    for filename in ["result.json", "pathway_summary.json"] {
        let Some(value) = read_json(&outputs.join("pathway_enrichment").join(filename)) else {
            continue;
        };
        let Some(method) = value.get("method").and_then(Value::as_str) else {
            continue;
        };
        let lower = method.to_ascii_lowercase();
        for implementation in ["clusterprofiler", "fgsea", "enrichr"] {
            if lower.contains(implementation) {
                return Some(implementation);
            }
        }
    }
    None
}

/// Concatenate only narrative-bearing reporting fields. Artifact names and
/// method metadata are deliberately excluded: they may legitimately mention a
/// tool without asserting that it produced the pathway p-values.
fn reporting_method_surfaces(outputs: &Path) -> Option<String> {
    let mut text = read_reports(outputs).unwrap_or_default();
    for stage in ["reporting", "final_reporting"] {
        let Some(value) = read_json(&outputs.join(stage).join("result.json")) else {
            continue;
        };
        for pointer in ["/summary", "/narrative_text", "/pathway_summary/threshold"] {
            if let Some(fragment) = value.pointer(pointer).and_then(Value::as_str) {
                text.push('\n');
                text.push_str(fragment);
            }
        }
    }
    (!text.is_empty()).then_some(text)
}

/// Fail when a pathway-level FDR label names a different implementation than
/// the pathway stage actually ran. This is structural, not a broad prose
/// heuristic: the regex is anchored to the method slot inside the explicit
/// `pathway-level (<implementation>) FDR` construction.
fn check_rc_pathway_method(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(recorded) = recorded_pathway_implementation(outputs) else {
        return;
    };
    let Some(surfaces) = reporting_method_surfaces(outputs) else {
        return;
    };
    report.checked.push("RC-METHOD");

    let label = Regex::new(
        r"(?i)pathway-level\s*\(\s*(clusterprofiler|fgsea|enrichr)\s*\)\s*(?:fdr|adjusted)",
    )
    .expect("static RC-METHOD regex compiles");
    let mismatched: BTreeSet<String> = label
        .captures_iter(&surfaces)
        .filter_map(|captures| captures.get(1))
        .map(|matched| matched.as_str().to_ascii_lowercase())
        .filter(|claimed| claimed != recorded)
        .collect();
    if !mismatched.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-METHOD",
            severity: Severity::Required,
            detail: format!(
                "pathway-level significance label names implementation(s) {}, but \
                 pathway_enrichment records `{recorded}`",
                mismatched.into_iter().collect::<Vec<_>>().join(", ")
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// RC-SOFTWARE-VERSION (Required) — narrative versions match runtime evidence
// ---------------------------------------------------------------------------

fn normalize_package_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn record_software_version(
    versions: &mut BTreeMap<String, (String, BTreeSet<String>)>,
    package: &str,
    version: &str,
) {
    let package = package.trim();
    let version = version.trim().trim_start_matches(['v', 'V']);
    let key = normalize_package_name(package);
    if key.len() < 3 || version.is_empty() || !version.chars().any(|c| c == '.') {
        return;
    }
    let entry = versions
        .entry(key)
        .or_insert_with(|| (package.to_string(), BTreeSet::new()));
    entry.1.insert(version.to_ascii_lowercase());
}

fn collect_result_software_versions(
    value: &Value,
    versions: &mut BTreeMap<String, (String, BTreeSet<String>)>,
) {
    if let (Some(method), Some(version)) = (
        value.get("method").and_then(Value::as_str),
        value.get("method_version").and_then(Value::as_str),
    ) {
        record_software_version(versions, method, version);
    }
    if let Some(packages) = value
        .get("language_packages_installed")
        .and_then(Value::as_array)
    {
        for package in packages {
            let name = package
                .get("name")
                .or_else(|| package.get("package"))
                .and_then(Value::as_str);
            let version = package
                .get("version")
                .or_else(|| package.get("resolved_version"))
                .and_then(Value::as_str);
            if let (Some(name), Some(version)) = (name, version) {
                record_software_version(versions, name, version);
            }
        }
    }
}

fn collect_software_versions(outputs: &Path) -> BTreeMap<String, (String, BTreeSet<String>)> {
    let mut versions = BTreeMap::new();
    let r_package = Regex::new(
        r"(?:^|[\s\[])([A-Za-z][A-Za-z0-9.]*)_([0-9]+(?:\.[0-9A-Za-z]+)+(?:[-+][0-9A-Za-z.]+)?)\b",
    )
    .expect("static R package-version regex compiles");

    let mut stage_dirs: Vec<_> = std::fs::read_dir(outputs)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    stage_dirs.sort();
    for stage in stage_dirs {
        if let Some(value) = read_json(&stage.join("result.json")) {
            collect_result_software_versions(&value, &mut versions);
        }
        if let Ok(lock) = std::fs::read_to_string(stage.join("env.lock")) {
            for captures in r_package.captures_iter(&lock) {
                if let (Some(package), Some(version)) = (captures.get(1), captures.get(2)) {
                    record_software_version(&mut versions, package.as_str(), version.as_str());
                }
            }
        }
    }

    let runtime = outputs.parent().unwrap_or(outputs);
    if let Ok(log) = std::fs::read_to_string(runtime.join("install-log.jsonl")) {
        for line in log.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let (Some(package), Some(version)) = (
                value.get("package").and_then(Value::as_str),
                value.get("resolved_version").and_then(Value::as_str),
            ) {
                record_software_version(&mut versions, package, version);
            }
        }
    }
    if let Some(lock) = read_json(&runtime.join("dependency-lock.json")) {
        for registry in ["r", "python", "conda"] {
            let Some(packages) = lock.get(registry).and_then(Value::as_array) else {
                continue;
            };
            for package in packages {
                if let (Some(name), Some(version)) = (
                    package.get("name").and_then(Value::as_str),
                    package.get("resolved").and_then(Value::as_str),
                ) {
                    record_software_version(&mut versions, name, version);
                }
            }
        }
    }
    versions
}

/// Fail when report prose pairs a known executed package with a version that
/// no retained runtime source records. The scan is intentionally narrow: it
/// constructs patterns only for packages found in this package's own runtime
/// evidence and only recognizes an immediately adjacent dotted version.
fn check_rc_software_versions(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let versions = collect_software_versions(outputs);
    let Some(prose) = read_agent_report_prose(outputs) else {
        return;
    };
    if versions.is_empty() {
        return;
    }
    report.checked.push("RC-SOFTWARE-VERSION");

    let mut mismatches = BTreeSet::new();
    for (_key, (package, recorded)) in versions {
        let pattern = format!(
            r"(?i)(?:^|[^A-Za-z0-9.])({})\s*(?:[:=]\s*|\(\s*)?(?:v(?:ersion)?\s*)?([0-9]+(?:\.[0-9A-Za-z]+)+(?:[-+][0-9A-Za-z.]+)?)",
            regex::escape(&package)
        );
        let matcher = Regex::new(&pattern).expect("escaped package name yields valid regex");
        for captures in matcher.captures_iter(&prose) {
            let Some(claimed) = captures.get(2) else {
                continue;
            };
            let claimed = claimed.as_str().trim_end_matches('.').to_ascii_lowercase();
            if !recorded.contains(&claimed) {
                mismatches.insert(format!(
                    "{package} claims version {}, retained runtime evidence records {}",
                    claimed,
                    recorded.iter().cloned().collect::<Vec<_>>().join(" or ")
                ));
            }
        }
    }

    if !mismatches.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-SOFTWARE-VERSION",
            severity: Severity::Required,
            detail: format!(
                "report software-version claim disagrees with retained runtime evidence: {}",
                mismatches.into_iter().collect::<Vec<_>>().join(" | ")
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// RC-METHOD-SELECTION (Required) — report preserves typed decision provenance
// ---------------------------------------------------------------------------

fn normalized_method_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A report may name the selected method, but it may not invent an SME/spec
/// preference or automatic advance that the retained discover decision records
/// as false. The check discovers every `discover_*` directory and its chosen
/// method, so it applies unchanged to new modalities and method families.
fn check_rc_method_selection_provenance(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(prose) = read_agent_report_prose(outputs) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(outputs) else {
        return;
    };
    let mut decisions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(task_id) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !path.is_dir() || !task_id.starts_with("discover_") {
            continue;
        }
        let Some(decision) = read_json(&path.join("decision.json")) else {
            continue;
        };
        let Some(chosen) = decision
            .get("chosen")
            .and_then(Value::as_str)
            .filter(|chosen| !chosen.trim().is_empty())
        else {
            continue;
        };
        decisions.push((
            task_id.to_string(),
            chosen.to_string(),
            decision
                .get("spec_preference_applied")
                .and_then(Value::as_bool),
            decision.get("auto_advanced").and_then(Value::as_bool),
        ));
    }
    if decisions.is_empty() {
        return;
    }
    report.checked.push("RC-METHOD-SELECTION");
    let mut failures = BTreeSet::new();
    for sentence in prose.split(['.', '\n']) {
        let lower = sentence.to_ascii_lowercase();
        let compact = normalized_method_text(sentence);
        for (task_id, chosen, spec_applied, auto_advanced) in &decisions {
            if !compact.contains(&normalized_method_text(chosen)) {
                continue;
            }
            let denies_preference = lower.contains("no spec")
                || lower.contains("without spec")
                || lower.contains("not sme-preferred")
                || lower.contains("not sme preferred")
                || lower.contains("no sme preference")
                || lower.contains("without sme preference")
                || lower.contains("without an sme preference")
                || lower.contains("not intake-preferred")
                || lower.contains("not intake preferred");
            let asserts_preference = lower.contains("spec-preference")
                || lower.contains("spec preference")
                || lower.contains("sme-preferred")
                || lower.contains("sme preferred")
                || lower.contains("intake-preferred")
                || lower.contains("intake preferred");
            if asserts_preference && !denies_preference && *spec_applied != Some(true) {
                let recorded =
                    spec_applied.map_or("missing", |value| if value { "true" } else { "false" });
                failures.insert(format!(
                    "{task_id} records spec_preference_applied={recorded} for `{chosen}` rather than true, but report says: {}",
                    sentence.trim()
                ));
            }
            if denies_preference && *spec_applied == Some(true) {
                failures.insert(format!(
                    "{task_id} records spec_preference_applied=true for `{chosen}`, but report denies that preference: {}",
                    sentence.trim()
                ));
            }
            let denies_auto = lower.contains("not auto")
                || lower.contains("no auto")
                || lower.contains("without auto");
            let asserts_auto = lower.contains("auto-advance")
                || lower.contains("auto advance")
                || lower.contains("auto-advanced")
                || lower.contains("automatically selected")
                || lower.contains("automatically advanced")
                || lower.contains("automatically chosen")
                || lower.contains("automatic selection");
            if asserts_auto && !denies_auto && *auto_advanced != Some(true) {
                let recorded =
                    auto_advanced.map_or("missing", |value| if value { "true" } else { "false" });
                failures.insert(format!(
                    "{task_id} records auto_advanced={recorded} for `{chosen}` rather than true, but report says: {}",
                    sentence.trim()
                ));
            }
            if denies_auto && *auto_advanced == Some(true) {
                failures.insert(format!(
                    "{task_id} records auto_advanced=true for `{chosen}`, but report denies automatic advance: {}",
                    sentence.trim()
                ));
            }
        }
    }
    if !failures.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-METHOD-SELECTION",
            severity: Severity::Required,
            detail: format!(
                "report method-selection provenance disagrees with retained discover decisions: {}",
                failures.into_iter().collect::<Vec<_>>().join(" | ")
            ),
        });
    }
}

// ---------------------------------------------------------------------------
// RC-FILTER-POPULATION (Required) — source and retained populations stay distinct
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FilterPopulation {
    field_path: String,
    source_count: u64,
    retained_count: u64,
    criterion: Option<String>,
}

fn count_value(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|raw| raw.parse::<u64>().ok()))
}

/// Derive the lifecycle-compatible retained keys for one input/pre-filter key.
/// This is structural rather than entity-specific: `n_cells_input`,
/// `n_variants_input`, `n_spectra_input`, and future domain nouns all resolve
/// through the same suffix/prefix transformations.
fn retained_population_keys(source_key: &str) -> Vec<String> {
    let mut keys = Vec::new();
    if let Some(stem) = source_key.strip_suffix("_input") {
        keys.extend([
            format!("{stem}_retained"),
            format!("{stem}_post_filter"),
            format!("{stem}_after_filter"),
        ]);
    }
    if let Some(stem) = source_key.strip_suffix("_pre_filter") {
        keys.extend([format!("{stem}_post_filter"), format!("{stem}_retained")]);
    }
    if let Some(stem) = source_key.strip_suffix("_before_filter") {
        keys.extend([format!("{stem}_after_filter"), format!("{stem}_retained")]);
    }
    if let Some(stem) = source_key.strip_prefix("n_input_") {
        keys.extend([
            format!("n_retained_{stem}"),
            format!("n_post_filter_{stem}"),
        ]);
    }
    keys.sort();
    keys.dedup();
    keys
}

fn population_terms(field_path: &str) -> BTreeSet<String> {
    let key = field_path.rsplit('.').next().unwrap_or(field_path);
    key.split('_')
        .map(str::to_ascii_lowercase)
        .filter(|token| {
            token.len() > 2
                && !matches!(
                    token.as_str(),
                    "input"
                        | "retained"
                        | "filtered"
                        | "filter"
                        | "before"
                        | "after"
                        | "pre"
                        | "post"
                        | "count"
                )
        })
        .flat_map(|token| {
            let singular = token.strip_suffix('s').unwrap_or(&token).to_string();
            [token, singular]
        })
        .filter(|token| !token.is_empty())
        .collect()
}

fn filter_criterion_for(map: &serde_json::Map<String, Value>, source_key: &str) -> Option<String> {
    let terms = population_terms(source_key);
    let mut generic = None;
    for (key, value) in map {
        let Some(text) = value.as_str() else {
            continue;
        };
        let lower = key.to_ascii_lowercase();
        if !lower.contains("filter")
            || ["path", "file", "artifact", "output", "matrix", "table"]
                .iter()
                .any(|excluded| lower.contains(excluded))
        {
            continue;
        }
        if terms.iter().any(|term| lower.contains(term)) {
            return Some(text.to_string());
        }
        generic.get_or_insert_with(|| text.to_string());
    }
    generic
}

/// Collect every before/after population pair owned by the same JSON object.
/// Keeping pairs local prevents unrelated nested counts from being joined;
/// collecting all pairs prevents an unchanged population from hiding another
/// filtered population in the same task result.
fn collect_filter_populations(
    value: &Value,
    prefix: &mut Vec<String>,
    out: &mut Vec<FilterPopulation>,
) {
    match value {
        Value::Object(map) => {
            for (source_key, source_value) in map {
                let Some(source_count) = count_value(source_value) else {
                    continue;
                };
                for retained_key in retained_population_keys(source_key) {
                    let Some(retained_count) = map.get(&retained_key).and_then(count_value) else {
                        continue;
                    };
                    let mut path = prefix.clone();
                    path.push(source_key.clone());
                    out.push(FilterPopulation {
                        field_path: path.join("."),
                        source_count,
                        retained_count,
                        criterion: filter_criterion_for(map, source_key),
                    });
                    break;
                }
            }
            for (key, child) in map {
                prefix.push(key.clone());
                collect_filter_populations(child, prefix, out);
                prefix.pop();
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                prefix.push(index.to_string());
                collect_filter_populations(child, prefix, out);
                prefix.pop();
            }
        }
        _ => {}
    }
}

/// Reject the ambiguity where a report calls a post-filter population the
/// unqualified input population even though a producing stage records a larger
/// source population. Calling the retained population filtered/tested, or the
/// input to a named downstream model, remains valid and is not matched.
fn check_rc_filter_population(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let Some(prose) = read_agent_report_prose(outputs) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(outputs) else {
        return;
    };
    let mut populations = Vec::new();
    for entry in entries.flatten() {
        let task_dir = entry.path();
        let Some(task_id) = task_dir.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(result) = read_json(&task_dir.join("result.json")) else {
            continue;
        };
        let mut found = Vec::new();
        collect_filter_populations(&result, &mut Vec::new(), &mut found);
        populations.extend(found.into_iter().map(|mut population| {
            population.field_path = format!("{task_id}.{}", population.field_path);
            population
        }));
    }
    populations.sort();
    populations.dedup();
    populations.retain(|population| population.source_count != population.retained_count);
    if populations.is_empty() {
        return;
    }
    report.checked.push("RC-FILTER-POPULATION");

    let mut details = BTreeSet::new();
    for population in populations {
        let source_count = population.source_count;
        let retained_count = population.retained_count;
        let retained_digits = retained_count.to_string();
        let entity_terms = population_terms(&population.field_path);
        let mut unqualified = BTreeSet::new();
        let mut false_attributions = BTreeSet::new();
        for sentence in prose.split(['.', '\n']) {
            let normalized = sentence.replace(',', "").to_ascii_lowercase();
            if !normalized.contains(&retained_digits)
                || !entity_terms.iter().any(|term| normalized.contains(term))
            {
                continue;
            }
            let qualifies_population = [
                "filtered",
                "retained",
                "post-filter",
                "postfilter",
                "after preprocessing",
                "after filtering",
                "input to",
                "input for",
            ]
            .iter()
            .any(|phrase| normalized.contains(phrase));
            if normalized.contains("input") && !qualifies_population {
                unqualified.insert(sentence.trim().to_string());
            }
        }
        if !unqualified.is_empty() {
            details.insert(format!(
                "{} records source population {source_count} and retained population \
                 {retained_count}, but report presents the retained count as an unqualified \
                 input population: {}",
                population.field_path,
                unqualified.into_iter().collect::<Vec<_>>().join(" | ")
            ));
        }

        let Some(criterion) = population.criterion else {
            continue;
        };
        let criterion_numbers: BTreeSet<String> = criterion
            .split(|character: char| !character.is_ascii_alphanumeric())
            .filter(|token| !token.is_empty() && token.chars().all(|c| c.is_ascii_digit()))
            .map(str::to_string)
            .collect();
        let criterion_terms: BTreeSet<String> = criterion
            .split(|character: char| !character.is_ascii_alphanumeric())
            .map(str::to_ascii_lowercase)
            .filter(|token| {
                token.len() > 3
                    && !matches!(
                        token.as_str(),
                        "filter" | "input" | "retained" | "entity" | "feature" | "genes"
                    )
            })
            .collect();
        for sentence in prose.split(['.', '\n']) {
            let normalized = sentence.replace(',', "").to_ascii_lowercase();
            if !normalized.contains(&retained_digits)
                || !normalized.contains("filter")
                || !(normalized.contains("after")
                    || normalized.contains("following")
                    || normalized.contains("using")
                    || normalized.contains("applying")
                    || normalized.contains("internal")
                    || normalized.contains("pre-filter")
                    || normalized.contains("prefilter"))
            {
                continue;
            }
            let numbers_present = criterion_numbers.is_empty()
                || criterion_numbers
                    .iter()
                    .all(|token| normalized.contains(token));
            let terms_present = criterion_terms.is_empty()
                || criterion_terms
                    .iter()
                    .any(|token| normalized.contains(token));
            if !numbers_present || !terms_present {
                false_attributions.insert(sentence.trim().to_string());
            }
        }
        if !false_attributions.is_empty() {
            details.insert(format!(
                "{} records retained population {retained_count}, but report attributes it to a \
                 filter without the retained criterion `{criterion}`: {}",
                population.field_path,
                false_attributions
                    .into_iter()
                    .collect::<Vec<_>>()
                    .join(" | ")
            ));
        }
    }
    if !details.is_empty() {
        report.findings.push(ReportingFinding {
            invariant: "RC-FILTER-POPULATION",
            severity: Severity::Required,
            detail: details.into_iter().collect::<Vec<_>>().join("; "),
        });
    }
}

// ---------------------------------------------------------------------------
// RC-FINAL-FIDELITY (Required) — terminal report preserves validated prose
// ---------------------------------------------------------------------------

/// Require the terminal report to carry the complete upstream reporting
/// narrative as an unchanged contiguous block. Deterministic system-owned
/// table and provenance blocks are appended independently after task execution,
/// so compare the agent-authored remainders rather than letting injection
/// placement create a false failure. Navigation or dashboard material may wrap
/// the validated block without weakening this guarantee.
fn check_rc_final_report_fidelity(outputs: &Path, report: &mut ReportingInvariantsReport) {
    let reporting_path = outputs.join("reporting").join("report.md");
    let final_path = outputs.join("final_reporting").join("final_report.md");
    let (Ok(upstream), Ok(final_report)) = (
        std::fs::read_to_string(&reporting_path),
        std::fs::read_to_string(&final_path),
    ) else {
        return;
    };
    let upstream = strip_marked_block(
        &strip_provenance_section(&upstream),
        FULL_TABLE_START,
        FULL_TABLE_END,
    );
    let final_report = strip_marked_block(
        &strip_provenance_section(&final_report),
        FULL_TABLE_START,
        FULL_TABLE_END,
    );
    let upstream = upstream.trim_end();
    if upstream.is_empty() {
        return;
    }

    report.checked.push("RC-FINAL-FIDELITY");
    if !final_report.contains(upstream) {
        report.findings.push(ReportingFinding {
            invariant: "RC-FINAL-FIDELITY",
            severity: Severity::Required,
            detail: "the agent-authored portion of final_reporting/final_report.md does not \
                     contain the complete agent-authored reporting/report.md byte-for-byte; \
                     the terminal stage rewrote, omitted, or reordered validated report content"
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

/// Whether a value explicitly records that an assessment was unavailable or
/// never run. Such metadata documents the absence of a computation; it is not
/// itself a retained assessment result.
fn assessment_was_not_run(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(raw) => {
            let token: String = raw
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            matches!(
                token.as_str(),
                "" | "notperformed"
                    | "notassessed"
                    | "notcomputed"
                    | "notrun"
                    | "unavailable"
                    | "notavailable"
                    | "notapplicable"
                    | "noassessment"
                    | "skipped"
                    | "omitted"
                    | "unknown"
            )
        }
        Value::Object(map) => map.iter().any(|(key, value)| {
            let key: String = key
                .chars()
                .filter(|character| character.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            (matches!(
                key.as_str(),
                "status" | "state" | "assessmentstatus" | "availability"
            ) && assessment_was_not_run(value))
                || (matches!(key.as_str(), "performed" | "available")
                    && value.as_bool() == Some(false))
        }),
        // An empty result array is a valid negative assessment, for example
        // `outlier_samples: []`. Booleans and numbers are likewise verdicts.
        Value::Array(_) | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// The first object key containing "outlier" anywhere in a JSON document whose
/// VALUE records a real assessment or verdict. A key such as
/// `sample_outlier_assessment: "not_performed"` is absence metadata, not an
/// outlier artifact. Conversely, `outlier_samples: []` is a retained negative
/// verdict and therefore counts.
fn find_outlier_key(v: &Value) -> Option<String> {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if k.to_lowercase().contains("outlier") && !assessment_was_not_run(val) {
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

/// Depth-first, name-sorted scan for retained sample-QC artifacts.
/// Deterministic: entries are visited in sorted order, files before
/// subdirectories.
fn collect_qc_artifacts(root: &Path, dir: &Path, depth: usize, budget: &mut usize) -> Vec<String> {
    if depth > QC_SCAN_MAX_DEPTH || *budget == 0 {
        return Vec::new();
    }
    let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    names.sort();
    let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
    let mut found = Vec::new();
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
            found.push(rel());
            continue;
        }
        if file_name.to_lowercase().ends_with(".json") {
            if let Some(key) = json_outlier_key(&path) {
                found.push(format!("{} (key `{key}`)", rel()));
            }
        }
    }
    for sub in subdirs {
        found.extend(collect_qc_artifacts(root, &sub, depth + 1, budget));
    }
    found
}

/// Artifact class expressed by a retained QC path, as a prose regex.
fn qc_artifact_class(path: &str) -> Option<(&'static str, &'static str)> {
    let lower = path.to_ascii_lowercase();
    let squashed: String = lower
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if squashed.contains("sampledistance")
        || squashed.contains("sampledist")
        || squashed.contains("distancematrix")
    {
        Some(("sample-distance", r"sample[-\s]+distance"))
    } else if squashed.contains("samplecorrelation") {
        Some(("sample-correlation", r"sample[-\s]+correlation"))
    } else if squashed.contains("cooksdistance") || squashed.contains("cooksd") {
        Some(("Cook's-distance", r"cook'?s[-\s]+distance"))
    } else if squashed.contains("pcacoord") {
        Some(("PCA coordinates", r"\bpca[-\s]+(?:coordinates?|coords?)\b"))
    } else if squashed.contains("pcascores") {
        Some(("PCA scores", r"\bpca[-\s]+scores?\b"))
    } else if squashed.contains("pcaloadings") {
        Some(("PCA loadings", r"\bpca[-\s]+loadings?\b"))
    } else if squashed.contains("pcaplot") || squashed.contains("samplepca") {
        Some(("PCA plot", r"\bpca(?:[-\s]+plot)?\b"))
    } else if squashed.contains("mdsplot")
        || lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|token| token == "mds")
    {
        Some(("MDS", r"\bmds\b"))
    } else if squashed.contains("outlier") {
        Some(("outlier", r"\boutlier"))
    } else {
        None
    }
}

/// Whether the report explicitly says an artifact class is absent. This is
/// narrower than a bag-of-words test: either "no ... <class>" must occur in
/// one clause, or "<class> ... not produced/retained/available" must occur.
fn report_denies_qc_artifact(prose: &str, class_pattern: &str) -> Option<String> {
    let before = Regex::new(&format!(r"(?i)\bno\b[^.;\n]{{0,180}}{class_pattern}"))
        .expect("QC class patterns are static regex fragments");
    let after = Regex::new(&format!(
        r"(?i){class_pattern}[^.;\n]{{0,100}}\b(?:was|were|is|are)?\s*not\s+(?:produced|retained|generated|available|present|included)"
    ))
    .expect("QC class patterns are static regex fragments");
    before
        .find(prose)
        .or_else(|| after.find(prose))
        .map(|m| clause_around(prose, m.start()).to_string())
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

    let mut budget = QC_SCAN_MAX_ENTRIES;
    let artifacts = collect_qc_artifacts(outputs, outputs, 0, &mut budget);
    let mut contradicted = BTreeSet::new();
    for artifact in &artifacts {
        let Some((class, pattern)) = qc_artifact_class(artifact) else {
            continue;
        };
        if let Some(claim) = report_denies_qc_artifact(&prose, pattern) {
            if contradicted.insert(class) {
                report.findings.push(ReportingFinding {
                    invariant: "RP-QC",
                    severity: Severity::Required,
                    detail: format!(
                        "report says the retained {class} artifact is absent (\"{claim}\"), but \
                         `{artifact}` exists under runtime/outputs/. Describe only the assessment \
                         that was not performed; do not deny an artifact the package retains"
                    ),
                });
            }
        }
    }

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
    if !artifacts.is_empty() {
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
        if artifact
            .result_schema
            .as_ref()
            .is_some_and(|embedded| embedded != schema)
        {
            ran = true;
            mismatches.push(format!(
                "{}: embedded result_schema disagrees with the executed WORKFLOW.json contract",
                artifact.stage_id
            ));
        }
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

/// Require the declared `literature_concordance` table to preserve the
/// evidence-row denominator represented by `LiteratureRollup::{concordant,
/// discordant,unverifiable}`. One entity can occur under several PMIDs and
/// even several statuses, so collapsing to one priority verdict per entity
/// destroys retained evidence and makes category totals look mutually
/// exclusive when they are not.
fn check_literature_evidence_table(
    package_root: &Path,
    outputs: &Path,
    literature: &crate::report_contract::LiteratureRollup,
    mismatches: &mut Vec<String>,
) {
    type EvidenceKey = (String, String, String);
    type EvidenceMeasurement = (Option<f64>, Option<f64>);
    let mut expected: BTreeMap<EvidenceKey, usize> = BTreeMap::new();
    let mut expected_measurements: BTreeMap<EvidenceKey, Vec<EvidenceMeasurement>> =
        BTreeMap::new();
    for (status, findings) in [
        ("concordant", literature.concordant.as_slice()),
        ("discordant", literature.discordant.as_slice()),
        ("unverifiable", literature.unverifiable.as_slice()),
    ] {
        for finding in findings {
            let key = (
                normalize_cell(&finding.entity),
                status.to_string(),
                finding.pmid.trim().to_string(),
            );
            *expected.entry(key.clone()).or_default() += 1;
            expected_measurements
                .entry(key)
                .or_default()
                .push((finding.effect, finding.significance));
        }
    }
    if expected.is_empty() {
        return;
    }

    let report_path = outputs.join("reporting").join("report.md");
    let Ok(narrative) = std::fs::read_to_string(&report_path) else {
        mismatches.push(
            "reporting/report.md is absent, so the literature evidence table cannot be checked"
                .into(),
        );
        return;
    };
    let tables = agent_authored_tables(&narrative);
    let has_literature_columns = |table: &&NarrativeTable| {
        let headers: Vec<String> = table
            .header
            .iter()
            .map(|header| normalize_cell(header))
            .collect();
        headers.iter().any(|header| header == "entity")
            && headers
                .iter()
                .any(|header| header.contains("verdict") || header == "status")
            && headers.iter().any(|header| header.contains("pmid"))
    };
    let heading_names_literature = |table: &&NarrativeTable| {
        let heading = normalize_cell(&table.heading);
        heading.contains("literature")
            && (heading.contains("concordance") || heading.contains("evidence"))
    };
    // A short explanatory paragraph commonly sits between a section heading
    // and its table, so the nearest non-blank line is not necessarily the
    // markdown heading. Prefer a heading-labelled structural match, then fall
    // back to the table's modality-neutral Entity + Status + PMID signature.
    let Some(table) = tables
        .iter()
        .find(|table| has_literature_columns(table) && heading_names_literature(table))
        .or_else(|| tables.iter().find(has_literature_columns))
    else {
        mismatches.push(
            "the report has assessed literature evidence but no literature concordance/evidence table"
                .into(),
        );
        return;
    };

    let normalized_headers: Vec<String> = table
        .header
        .iter()
        .map(|header| normalize_cell(header))
        .collect();
    let entity_col = normalized_headers
        .iter()
        .position(|header| header == "entity");
    let status_col = normalized_headers
        .iter()
        .position(|header| header.contains("verdict") || header == "status");
    let pmid_col = normalized_headers
        .iter()
        .position(|header| header.contains("pmid"));
    let (Some(entity_col), Some(status_col), Some(pmid_col)) = (entity_col, status_col, pmid_col)
    else {
        mismatches.push(
            "the literature evidence table must expose Entity, Verdict/Status, and PMID columns"
                .into(),
        );
        return;
    };

    let compact = |text: &str| -> String {
        text.chars()
            .filter(|character| character.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let mut effect_names: BTreeSet<String> =
        ["effect", "effect size"].into_iter().map(compact).collect();
    let mut significance_names: BTreeSet<String> =
        ["significance", "adjusted p value", "false discovery rate"]
            .into_iter()
            .map(compact)
            .collect();
    let policy_names = load_policy_column_synonyms(package_root);
    effect_names.extend(policy_names.effect.iter().map(|name| compact(name)));
    significance_names.extend(policy_names.significance.iter().map(|name| compact(name)));
    if let Some(schemas) = read_report_schemas(package_root) {
        for schema in schemas.values() {
            if let Some(name) = &schema.signed_effect_column {
                effect_names.insert(compact(name));
            }
            effect_names.extend(
                schema
                    .signed_effect_aliases
                    .iter()
                    .map(|name| compact(name)),
            );
            if let Some(significance) = &schema.significance {
                significance_names.insert(compact(&significance.column));
            }
        }
    }
    let header_matches = |header: &str, names: &BTreeSet<String>| {
        let header = compact(header);
        names
            .iter()
            .filter(|name| !name.is_empty())
            .any(|name| header == *name || (name.len() >= 3 && header.contains(name)))
    };
    let effect_col = table
        .header
        .iter()
        .position(|header| header_matches(header, &effect_names));
    let significance_col = table
        .header
        .iter()
        .position(|header| header_matches(header, &significance_names));
    let expects_effect = expected_measurements
        .values()
        .flatten()
        .any(|(effect, _)| effect.is_some());
    let expects_significance = expected_measurements
        .values()
        .flatten()
        .any(|(_, significance)| significance.is_some());
    if expects_effect && effect_col.is_none() {
        mismatches.push(
            "the literature evidence table omits the retained effect measurement column".into(),
        );
    }
    if expects_significance && significance_col.is_none() {
        mismatches.push(
            "the literature evidence table omits the retained significance measurement column"
                .into(),
        );
    }

    let pmid_re = Regex::new(r"\b\d{4,9}\b").expect("static PMID cell regex compiles");
    let mut actual: BTreeMap<EvidenceKey, usize> = BTreeMap::new();
    let mut actual_measurements: BTreeMap<EvidenceKey, Vec<EvidenceMeasurement>> = BTreeMap::new();
    let mut malformed_rows = 0usize;
    let mut aggregated_rows = 0usize;
    for row in &table.rows {
        let entity = row.get(entity_col).map(|cell| normalize_cell(cell));
        let status = row.get(status_col).map(|cell| normalize_cell(cell));
        let pmid_cell = row.get(pmid_col).map(String::as_str).unwrap_or_default();
        let pmids: Vec<&str> = pmid_re
            .find_iter(pmid_cell)
            .map(|matched| matched.as_str())
            .collect();
        let (Some(entity), Some(mut status)) = (entity, status) else {
            malformed_rows += 1;
            continue;
        };
        status = match status.as_str() {
            "same direction" | "same_direction" => "concordant".into(),
            "opposite direction" | "opposite_direction" => "discordant".into(),
            _ => status,
        };
        if entity.is_empty() || pmids.is_empty() {
            malformed_rows += 1;
            continue;
        }
        if pmids.len() != 1 {
            aggregated_rows += 1;
        }
        let measurements = (
            effect_col
                .and_then(|column| row.get(column))
                .and_then(|cell| parse_markdown_number(cell)),
            significance_col
                .and_then(|column| row.get(column))
                .and_then(|cell| parse_markdown_number(cell)),
        );
        for pmid in pmids {
            let key = (entity.clone(), status.clone(), pmid.to_string());
            *actual.entry(key.clone()).or_default() += 1;
            actual_measurements
                .entry(key)
                .or_default()
                .push(measurements);
        }
    }

    if malformed_rows > 0 {
        mismatches.push(format!(
            "literature evidence table has {malformed_rows} row(s) without one resolvable entity, status, and PMID"
        ));
    }
    if aggregated_rows > 0 {
        mismatches.push(format!(
            "literature evidence table aggregates multiple PMIDs in {aggregated_rows} row(s); it must retain one row per evidence record"
        ));
    }
    if table.rows.len() != literature.n_evidence_rows_assessed as usize {
        mismatches.push(format!(
            "literature evidence table has {} row(s), but report-data.json declares {} assessed evidence rows",
            table.rows.len(),
            literature.n_evidence_rows_assessed
        ));
    }
    if actual != expected {
        let missing: usize = expected
            .iter()
            .map(|(key, count)| count.saturating_sub(actual.get(key).copied().unwrap_or(0)))
            .sum();
        let extra: usize = actual
            .iter()
            .map(|(key, count)| count.saturating_sub(expected.get(key).copied().unwrap_or(0)))
            .sum();
        mismatches.push(format!(
            "literature evidence table differs from the report-data evidence-row multiset ({missing} missing, {extra} extra)"
        ));
    }

    if (!expects_effect || effect_col.is_some())
        && (!expects_significance || significance_col.is_some())
    {
        let tolerances = NarrativeTolerances::load(package_root);
        let agrees = |expected: Option<f64>, actual: Option<f64>, significance: bool| match (
            expected, actual,
        ) {
            (None, None) => true,
            (Some(expected), Some(actual)) => tolerances.as_ref().map_or_else(
                || expected == actual,
                |tolerance| {
                    tolerance.agrees(
                        &RoleCell {
                            narrative: 0,
                            source: 0,
                            significance,
                        },
                        actual,
                        expected,
                    )
                },
            ),
            _ => false,
        };
        let mut measurement_mismatches = 0usize;
        for (key, expected_values) in &expected_measurements {
            let mut actual_values = actual_measurements.get(key).cloned().unwrap_or_default();
            if actual_values.len() != expected_values.len() {
                // Identity/multiplicity drift is already reported by the
                // evidence-row multiset check above. Compare measurements only
                // for keys whose rows align one-for-one, so a collapsed PMID or
                // wrong status is not mislabeled as a numeric transcription
                // error as well.
                continue;
            }
            for expected_value in expected_values {
                let matching = actual_values.iter().position(|actual_value| {
                    agrees(expected_value.0, actual_value.0, false)
                        && agrees(expected_value.1, actual_value.1, true)
                });
                if let Some(index) = matching {
                    actual_values.remove(index);
                } else {
                    measurement_mismatches += 1;
                }
            }
            measurement_mismatches += actual_values.len();
        }
        if measurement_mismatches > 0 {
            mismatches.push(format!(
                "literature evidence table has {measurement_mismatches} row(s) whose effect or significance does not match the retained evidence object"
            ));
        }
    }

    let entity_bucket_re = Regex::new(
        r"(?is)summary\s+of\s+assessed\s+entities\b.{0,1200}?\b\d[\d,]*\s+concordant\b.{0,400}?\b\d[\d,]*\s+discordant\b.{0,400}?\b\d[\d,]*\s+unverifiable\b",
    )
    .expect("static literature entity-bucket regex compiles");
    if entity_bucket_re.is_match(&narrative) {
        mismatches.push(
            "the narrative presents concordant, discordant, and unverifiable evidence-row statuses as a partition of assessed entities; entities may occur in more than one status"
                .into(),
        );
    }

    // A status row count and the number of distinct entities represented by
    // those rows are different denominators. Verify both whenever prose names
    // them explicitly in the same statement.
    for (status, findings) in [
        ("concordant", literature.concordant.as_slice()),
        ("discordant", literature.discordant.as_slice()),
        ("unverifiable", literature.unverifiable.as_slice()),
    ] {
        let distinct_entities: BTreeSet<String> = findings
            .iter()
            .map(|finding| normalize_cell(&finding.entity))
            .filter(|entity| !entity.is_empty())
            .collect();
        let pattern = format!(
            r"(?i)\b(\d[\d,]*)\s+{status}\s+(?:evidence\s+)?rows?\s+(?:span|spans|cover|covers|represent|represents|occur\s+across)\s+(\d[\d,]*)\s+distinct\s+entit(?:y|ies)\b"
        );
        let Ok(status_re) = Regex::new(&pattern) else {
            continue;
        };
        for captures in status_re.captures_iter(&narrative) {
            let claimed_rows = captures
                .get(1)
                .and_then(|value| value.as_str().replace(',', "").parse::<usize>().ok());
            let claimed_entities = captures
                .get(2)
                .and_then(|value| value.as_str().replace(',', "").parse::<usize>().ok());
            if claimed_rows != Some(findings.len())
                || claimed_entities != Some(distinct_entities.len())
            {
                mismatches.push(format!(
                    "narrative `{status}` evidence summary reports {} row(s) spanning {} \
                     distinct entity/entities, but report-data retains {} row(s) spanning {} \
                     distinct entity/entities",
                    claimed_rows
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "an unparseable number of".into()),
                    claimed_entities
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "an unparseable number of".into()),
                    findings.len(),
                    distinct_entities.len()
                ));
            }
        }
    }

    // Multiplicity prose has a machine-defined denominator. "One or more"
    // covers every distinct entity represented by an evidence row, whereas
    // "multiple" or "more than one" covers only entities represented by at
    // least two rows. Derive both from retained objects instead of parsing a
    // parenthetical label list, whose identifiers may legitimately contain
    // commas, colons, or other modality-specific delimiters.
    let mut evidence_rows_by_entity: BTreeMap<String, usize> = BTreeMap::new();
    for finding in literature
        .concordant
        .iter()
        .chain(&literature.discordant)
        .chain(&literature.unverifiable)
    {
        let entity = normalize_cell(&finding.entity);
        if !entity.is_empty() {
            *evidence_rows_by_entity.entry(entity).or_default() += 1;
        }
    }
    let multiplicity_re = Regex::new(
        r"(?i)\b(\d[\d,]*)\s+distinct\s+entities?\s+each\s+(?:contributed|produced|yielded|generated|provided)\s+(one\s+or\s+more|more\s+than\s+one|multiple)\s+evidence\s+rows?\b",
    )
    .expect("static literature multiplicity regex compiles");
    for captures in multiplicity_re.captures_iter(&narrative) {
        let Some(claimed) = captures
            .get(1)
            .and_then(|value| value.as_str().replace(',', "").parse::<usize>().ok())
        else {
            continue;
        };
        let Some(quantifier) = captures.get(2).map(|value| value.as_str()) else {
            continue;
        };
        let expected = if quantifier.eq_ignore_ascii_case("one or more") {
            evidence_rows_by_entity.len()
        } else {
            evidence_rows_by_entity
                .values()
                .filter(|rows| **rows > 1)
                .count()
        };
        if claimed != expected {
            mismatches.push(format!(
                "narrative asserts {claimed} distinct entities each contributed \
                 {quantifier} evidence rows, but report-data establishes {expected}"
            ));
        }
    }
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
fn check_rc_literature_counts(
    package_root: &Path,
    outputs: &Path,
    report: &mut ReportingInvariantsReport,
) {
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
            check_literature_evidence_table(package_root, outputs, &literature, &mut mismatches);
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

/// RC-LITERATURE-LINK: verify every entity/source association stated in the
/// agent-authored report against the package's own row-level literature
/// matrix. The claim extractor already binds sources locally in multi-entity
/// sentences, and the literature verifier requires the exact entity/source
/// pair. Reusing those policy-driven components avoids a second,
/// report-specific entity grammar and applies to every analysis modality.
fn check_rc_literature_claim_links(
    package_root: &Path,
    outputs: &Path,
    report: &mut ReportingInvariantsReport,
) {
    let matrix_path = outputs
        .join("contextualize_findings_with_literature")
        .join("claims_evidence_matrix.csv");
    if !matrix_path.is_file() {
        return;
    }
    let Some(narrative) = read_agent_report_prose(outputs) else {
        return;
    };
    let cfg = [package_root.join("policies"), package_root.join("config")]
        .into_iter()
        .find_map(|dir| {
            let path =
                crate::claim_extractor::resolve_policy_file(&dir, "interpretation-policy.json")?;
            let raw = std::fs::read_to_string(path).ok()?;
            let policy = serde_json::from_str::<Value>(&raw).ok()?;
            crate::claim_extractor::ExtractorConfig::from_policy(&policy).ok()
        });
    let Some(cfg) = cfg else {
        return;
    };
    let claims: Vec<_> = crate::claim_extractor::extract_claims(&narrative, &cfg)
        .into_iter()
        .filter(|claim| {
            claim.contract == crate::claim_contract::ClaimContract::LiteratureGrounded
                && claim
                    .literature_evidence
                    .as_ref()
                    .is_some_and(|evidence| !evidence.cited_pmids.is_empty())
        })
        .collect();
    if claims.is_empty() {
        return;
    }
    report.checked.push("RC-LITERATURE-LINK");

    let mut failures: Vec<String> =
        crate::claim_verifier::verify_claims_with_discovery(&claims, outputs, package_root, &cfg)
            .into_iter()
            .filter_map(|verdict| match verdict.status {
                crate::claim_verifier::ClaimStatus::Mismatch { detail }
                    if detail.starts_with("literature:") =>
                {
                    Some(format!(
                        "entity `{}` in `{}`: {detail}",
                        verdict.claim.entity, verdict.claim.excerpt
                    ))
                }
                crate::claim_verifier::ClaimStatus::Unverifiable { reason }
                    if reason.starts_with("no claims_evidence_matrix row for finding") =>
                {
                    Some(format!(
                        "entity `{}` in `{}`: literature: {reason}",
                        verdict.claim.entity, verdict.claim.excerpt
                    ))
                }
                _ => None,
            })
            .collect();
    if failures.is_empty() {
        return;
    }
    failures.sort();
    failures.dedup();
    const MAX_REPORTED_FAILURES: usize = 20;
    let omitted = failures.len().saturating_sub(MAX_REPORTED_FAILURES);
    failures.truncate(MAX_REPORTED_FAILURES);
    let suffix = if omitted == 0 {
        String::new()
    } else {
        format!("; {omitted} additional invalid association(s) omitted")
    };
    report.findings.push(ReportingFinding {
        invariant: "RC-LITERATURE-LINK",
        severity: Severity::Required,
        detail: format!(
            "report prose contains entity/source associations absent from \
             claims_evidence_matrix.csv: {}{suffix}",
            failures.join("; ")
        ),
    });
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
    let compact: String = lc
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect();
    let contains_form = |form: &str| {
        let form_lc = form.to_lowercase();
        let form_compact: String = form_lc
            .chars()
            .filter(|character| character.is_alphanumeric())
            .collect();
        lc.contains(&form_lc)
            || (!form_compact.is_empty() && compact.contains(form_compact.as_str()))
    };
    // Each id-word must be present, either verbatim or via a universal
    // spelled-out alias. Keeping the "ALL words present" requirement preserves
    // the strictness that stops an unrelated heading from anchoring a section;
    // aliases only bridge abbreviation ↔ expansion, never widen the match.
    // Compact comparison also treats punctuation as presentation: a contract
    // token such as `preprocessing` matches "Pre-processing" without adding a
    // domain-specific heading synonym.
    words.iter().all(|word| {
        contains_form(word)
            || section_word_aliases(word)
                .iter()
                .any(|alias| contains_form(alias))
    })
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

/// Whether a result-role cell explicitly states that no value is available.
/// Bounds and approximations are deliberately excluded: they are assertions
/// the point-value comparator abstains on, not missing-value placeholders.
fn is_markdown_missing_value(cell: &str) -> bool {
    let normalized = cell
        .trim()
        .trim_matches(|c| matches!(c, '*' | '`' | '_'))
        .trim()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "" | "-" | "—" | "–" | "na" | "n/a" | "nan" | "null" | "."
    )
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
                        (None, Some(_)) => cells
                            .get(role.narrative)
                            .is_none_or(|cell| !is_markdown_missing_value(cell)),
                        (Some(_), None) => false,
                        (None, None) => true,
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

/// Prefix length asserted by a table caption containing "Top". An explicit
/// number wins; otherwise the number of displayed data rows is the asserted
/// prefix length ("Top enriched pathways" means every displayed row is from
/// the canonical leading prefix).
fn ranked_caption_size(heading: &str, row_count: usize) -> Option<usize> {
    let re = Regex::new(r"(?i)\btop\b(?:[\s\-]+(\d+)\b)?")
        .expect("static RC-ROW ranked-caption regex compiles");
    let captures = re.captures(heading)?;
    Some(
        captures
            .get(1)
            .and_then(|capture| capture.as_str().parse::<usize>().ok())
            .unwrap_or(row_count),
    )
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
            let cell = cells
                .get(role.narrative)
                .map(String::as_str)
                .unwrap_or_default();
            let claimed = parse_markdown_number(cell);
            let observed = source.number(row_index, role.source);
            let column = source.headers.get(role.source).unwrap_or_default();
            match (claimed, observed) {
                (Some(claimed), Some(observed)) if !tol.agrees(role, claimed, observed) => {
                    failures.push(format!(
                        "row `{key}` states {column} = {claimed} but `{artifact}` holds {observed}"
                    ));
                }
                (None, Some(observed)) if is_markdown_missing_value(cell) => {
                    failures.push(format!(
                        "row `{key}` omits {column} as `{cell}` but `{artifact}` holds {observed}"
                    ));
                }
                (Some(claimed), None) => {
                    failures.push(format!(
                        "row `{key}` states {column} = {claimed} but `{artifact}` has no finite value"
                    ));
                }
                _ => {}
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
    let generic_effect_order = [
        "by effect size",
        "by absolute effect",
        "ranked by effect",
        "ordered by effect",
        "sorted by effect",
    ]
    .iter()
    .any(|cue| heading.contains(cue));
    let declared_effect_order = source
        .cols
        .effect
        .and_then(|column| source.headers.get(column))
        .map(normalize_cell)
        .filter(|column| !column.is_empty())
        .is_some_and(|column| {
            ["by", "ranked by", "ordered by", "sorted by"]
                .iter()
                .any(|prefix| heading.contains(&format!("{prefix} {column}")))
        });
    if generic_effect_order || declared_effect_order {
        return RankedTableCheck::Failure(
            "caption says the table is ordered by effect size, but the canonical ranking is \
             declared significance first, then absolute effect, entity, and source row"
                .into(),
        );
    }
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
                    if let Some(claimed_n) = ranked_caption_size(&table.heading, table.rows.len()) {
                        let ranking = report_data
                            .as_ref()
                            .and_then(|data| {
                                data.artifacts
                                    .iter()
                                    .find(|artifact| artifact.stage_id == binding.stage_id)
                            })
                            .and_then(|artifact| artifact.ranking.as_ref());
                        match ranking {
                            Some(ranking) => {
                                match verify_ranked_table(
                                    &table, &binding, source, ranking, claimed_n,
                                ) {
                                    RankedTableCheck::Pass => {}
                                    RankedTableCheck::Failure(detail) => {
                                        offenders.push(format!("{site} — {detail}"));
                                    }
                                    RankedTableCheck::Skipped(detail) => skipped.push(format!(
                                        "{site} ranking could not be re-derived — {detail}"
                                    )),
                                }
                            }
                            None => skipped.push(format!(
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

    #[test]
    fn rc_software_version_rejects_model_recalled_version() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "pathway_enrichment/env.lock",
            "other attached packages:\n [1] fgsea_1.36.2 msigdbr_26.1.0\n",
        );
        write(
            &outputs,
            "reporting/report.md",
            "## Reproducibility\n\n- fgsea: v1.36.2\n- msigdbr: v24.1.1\n",
        );

        let wrong = check_reporting_invariants(tmp.path());
        assert!(wrong.checked.contains(&"RC-SOFTWARE-VERSION"));
        assert!(wrong.findings.iter().any(|finding| {
            finding.invariant == "RC-SOFTWARE-VERSION"
                && finding.severity == Severity::Required
                && finding.detail.contains("msigdbr")
                && finding.detail.contains("24.1.1")
                && finding.detail.contains("26.1.0")
        }));

        write(
            &outputs,
            "reporting/report.md",
            "## Reproducibility\n\n- fgsea: v1.36.2\n- msigdbr: v26.1.0\n",
        );
        let corrected = check_reporting_invariants(tmp.path());
        assert!(corrected
            .findings
            .iter()
            .all(|finding| finding.invariant != "RC-SOFTWARE-VERSION"));
    }

    #[test]
    fn rc_filter_population_rejects_retained_count_as_unqualified_input() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "qc_preprocessing/result.json",
            r#"{
                "gene_filter": {
                    "n_genes_pre_filter": 63677,
                    "n_genes_retained": 22369
                }
            }"#,
        );
        write(
            &outputs,
            "reporting/report.md",
            "The input count matrix contained gene expression values for 22,369 genes across 8 samples.",
        );

        let ambiguous = check_reporting_invariants(tmp.path());
        assert!(ambiguous.checked.contains(&"RC-FILTER-POPULATION"));
        assert!(ambiguous.findings.iter().any(|finding| {
            finding.invariant == "RC-FILTER-POPULATION"
                && finding.severity == Severity::Required
                && finding.detail.contains("22369")
                && finding.detail.contains("63677")
        }));

        write(
            &outputs,
            "reporting/report.md",
            "The source count matrix contained 63,677 genes across 8 samples; the pre-filter \
             retained 22,369 genes for testing.",
        );
        let corrected = check_reporting_invariants(tmp.path());
        assert!(corrected
            .findings
            .iter()
            .all(|finding| finding.invariant != "RC-FILTER-POPULATION"));
    }

    #[test]
    fn rc_method_selection_rejects_invented_preference_and_auto_advance() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "discover_association/decision.json",
            r#"{
                "chosen": "model_x",
                "spec_preference_applied": false,
                "auto_advanced": false
            }"#,
        );
        write(
            &outputs,
            "reporting/report.md",
            "Model X was the SME-preferred method and was confirmed by auto-advance.",
        );
        let wrong = check_reporting_invariants(tmp.path());
        assert!(wrong.checked.contains(&"RC-METHOD-SELECTION"));
        assert!(wrong.findings.iter().any(|finding| {
            finding.invariant == "RC-METHOD-SELECTION"
                && finding.detail.contains("spec_preference_applied=false")
                && finding.detail.contains("auto_advanced=false")
        }));

        write(
            &outputs,
            "reporting/report.md",
            "Model X was selected after SME review of the ranked candidates.",
        );
        let corrected = check_reporting_invariants(tmp.path());
        assert!(corrected
            .findings
            .iter()
            .all(|finding| finding.invariant != "RC-METHOD-SELECTION"));

        write(
            &outputs,
            "discover_association/decision.json",
            r#"{
                "chosen": "model_x",
                "spec_preference_applied": true,
                "auto_advanced": true
            }"#,
        );
        write(
            &outputs,
            "reporting/report.md",
            "Model X was selected without an SME preference and was not auto-advanced.",
        );
        let reversed = check_reporting_invariants(tmp.path());
        let failures = reversed.required_failures().join(" | ");
        assert!(
            failures.contains("spec_preference_applied=true")
                && failures.contains("auto_advanced=true"),
            "{failures}"
        );
    }

    #[test]
    fn rc_filter_population_reads_nested_stats_and_rejects_wrong_filter_attribution() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "preprocess_inputs/result.json",
            r#"{
                "qc_stats": {
                    "n_samples_input": 8,
                    "n_samples_retained": 8,
                    "n_features_input": 63677,
                    "n_features_retained": 22369,
                    "feature_filter": "total_counts_ge_10"
                }
            }"#,
        );
        write(
            &outputs,
            "reporting/report.md",
            "The matrix contained 22,369 features after ModelX's internal low-count pre-filter.",
        );
        let wrong = check_reporting_invariants(tmp.path());
        assert!(wrong.checked.contains(&"RC-FILTER-POPULATION"));
        assert!(wrong.findings.iter().any(|finding| {
            finding.invariant == "RC-FILTER-POPULATION"
                && finding.detail.contains("without the retained criterion")
                && finding.detail.contains("preprocess_inputs")
        }));

        write(
            &outputs,
            "reporting/report.md",
            "Preprocessing retained 22,369 features after requiring total counts >= 10.",
        );
        let corrected = check_reporting_invariants(tmp.path());
        assert!(corrected
            .findings
            .iter()
            .all(|finding| finding.invariant != "RC-FILTER-POPULATION"));
    }

    #[test]
    fn rc_threshold_preserves_strict_lt_and_gt_contracts_across_modalities() {
        for (field, comparator, cutoff, correct, inclusive) in [
            ("error_rate", "lt", 0.1, "<", "≤"),
            ("risk_score", "gt", 2.5, ">", "≥"),
            ("signed_deviation", "lt", -0.5, "<", "≤"),
        ] {
            let tmp = TempDir::new().unwrap();
            let outputs = outputs_dir(&tmp);
            write(
                &outputs,
                "reporting/report-data.json",
                &serde_json::json!({
                    "artifacts": [{
                        "stage_id": "generic_screen",
                        "artifact": "scores.tsv",
                        "result_schema": {
                            "artifact": "scores.tsv",
                            "entity_column": "record_id",
                            "significance": {
                                "column": field,
                                "threshold": cutoff,
                                "comparator": comparator
                            }
                        },
                        "n_total": 4,
                        "n_significant": 2,
                        "direction_split": null,
                        "effect_distribution": null,
                        "significant_entities": [],
                        "significant_table_path":
                            "runtime/outputs/generic_screen/scores.significant.tsv",
                        "full_table_path":
                            "runtime/outputs/generic_screen/scores.full.tsv",
                        "spilled_to_attachment_only": false
                    }],
                    "literature": null
                })
                .to_string(),
            );
            write(
                &outputs,
                "reporting/report.md",
                &format!(
                    "The selection threshold classified records at `{field}` {inclusive} {}.",
                    if field == "error_rate" {
                        ".1".to_string()
                    } else {
                        cutoff.to_string()
                    }
                ),
            );
            let wrong = check_reporting_invariants(tmp.path());
            assert!(
                wrong.required_failures().iter().any(|failure| {
                    failure.starts_with("RC-THRESHOLD:")
                        && failure.contains(inclusive)
                        && failure.contains(correct)
                }),
                "{wrong:#?}"
            );

            write(
                &outputs,
                "reporting/report.md",
                &format!(
                    "The selection threshold classified records at `{field}` {correct} {cutoff}."
                ),
            );
            let corrected = check_reporting_invariants(tmp.path());
            assert!(
                corrected
                    .findings
                    .iter()
                    .all(|finding| finding.invariant != "RC-THRESHOLD"),
                "{corrected:#?}"
            );

            write(
                &outputs,
                "reporting/report.md",
                &format!(
                    "Record ALPHA was significant at `{field}` {correct} {}.\n\n\
                     | Record | Bound |\n\
                     |---|---|\n\
                     | BETA | `{field}` {inclusive} {} |",
                    cutoff / 10.0,
                    cutoff / 5.0
                ),
            );
            let observed_bounds = check_reporting_invariants(tmp.path());
            assert!(
                observed_bounds
                    .findings
                    .iter()
                    .all(|finding| finding.invariant != "RC-THRESHOLD"),
                "{observed_bounds:#?}"
            );

            write(
                &outputs,
                "reporting/report.md",
                &format!(
                    "The declared cutoff classified records at `{field}` {correct} {}.",
                    cutoff / 2.0
                ),
            );
            let wrong_cutoff = check_reporting_invariants(tmp.path());
            assert!(
                wrong_cutoff.required_failures().iter().any(|failure| {
                    failure.starts_with("RC-THRESHOLD:")
                        && failure.contains(&format!("{}", cutoff / 2.0))
                }),
                "{wrong_cutoff:#?}"
            );
        }
    }

    #[test]
    fn rc_threshold_exact_schema_field_disambiguates_shared_policy_aliases() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            tmp.path(),
            "policies/interpretation-policy.json",
            r#"{
                "verifiableEntities": {
                    "pvalueColumns": ["alpha_score", "beta_score", "FDR"]
                }
            }"#,
        );
        let artifact = |stage: &str, field: &str, threshold: f64, comparator: &str| {
            serde_json::json!({
                "stage_id": stage,
                "artifact": "scores.tsv",
                "result_schema": {
                    "artifact": "scores.tsv",
                    "entity_column": "record_id",
                    "significance": {
                        "column": field,
                        "threshold": threshold,
                        "comparator": comparator
                    }
                },
                "n_total": 4,
                "n_significant": 2,
                "direction_split": null,
                "effect_distribution": null,
                "significant_entities": [],
                "significant_table_path": "",
                "full_table_path": "",
                "spilled_to_attachment_only": false
            })
        };
        write(
            &outputs,
            "reporting/report-data.json",
            &serde_json::json!({
                "artifacts": [
                    artifact("alpha_screen", "alpha_score", 0.1, "lt"),
                    artifact("beta_screen", "beta_score", 2.5, "gt")
                ],
                "literature": null
            })
            .to_string(),
        );
        write(
            &outputs,
            "reporting/report.md",
            "The declared threshold classified records at `alpha_score` ≤ 0.1.",
        );
        let exact = check_reporting_invariants(tmp.path());
        assert!(
            exact
                .required_failures()
                .iter()
                .any(|failure| failure.starts_with("RC-THRESHOLD:")),
            "{exact:#?}"
        );

        write(
            &outputs,
            "reporting/report.md",
            "The FDR threshold classified records at FDR ≤ 0.1.",
        );
        let ambiguous_alias = check_reporting_invariants(tmp.path());
        assert!(
            ambiguous_alias
                .findings
                .iter()
                .all(|finding| finding.invariant != "RC-THRESHOLD"),
            "one alias names two incompatible contracts, so the checker must abstain: \
             {ambiguous_alias:#?}"
        );
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
    fn rc_literature_checks_status_entity_denominators_and_multiplicity() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "contextualize_findings_with_literature/claims_evidence_matrix.csv",
            "finding_id,entity,pmid,concordance_flag,searched\n\
             F1,ALPHA,1111,same_direction,true\n\
             F1,ALPHA,2222,same_direction,true\n\
             F2,BETA,3333,opposite_direction,true\n\
             F3,GAMMA,4444,unverifiable,true\n",
        );
        write(
            &outputs,
            "contextualize_findings_with_literature/result.json",
            r#"{
                "n_entities_assessed": 3,
                "n_entities_not_assessed": 0,
                "n_evidence_rows_assessed": 4,
                "n_evidence_rows_total": 4
            }"#,
        );
        let finding = |entity: &str, pmid: &str| {
            serde_json::json!({
                "entity": entity,
                "pmid": pmid,
                "evidence_quote": "retained evidence",
                "effect": null
            })
        };
        write(
            &outputs,
            "reporting/report-data.json",
            &serde_json::json!({
                "artifacts": [],
                "literature": {
                    "concordant": [finding("ALPHA", "1111"), finding("ALPHA", "2222")],
                    "discordant": [finding("BETA", "3333")],
                    "unverifiable": [finding("GAMMA", "4444")],
                    "non_replications": [],
                    "novel_count": 0,
                    "not_assessed_count": 0,
                    "n_entities_assessed": 3,
                    "n_entities_not_assessed": 0,
                    "n_evidence_rows_assessed": 4,
                    "n_evidence_rows_total": 4,
                    "retrieved_sources": ["1111", "2222", "3333", "4444"]
                }
            })
            .to_string(),
        );
        write(
            &outputs,
            "reporting/report.md",
            "## Literature evidence\n\n\
             | Entity | Status | PMID |\n\
             |---|---|---|\n\
             | ALPHA | concordant | 1111 |\n\
             | ALPHA | concordant | 2222 |\n\
             | BETA | discordant | 3333 |\n\
             | GAMMA | unverifiable | 4444 |\n\n\
             2 concordant evidence rows span 2 distinct entities.\n\
             2 distinct entities each contributed multiple evidence rows \
             (ALPHA: 2 rows).\n",
        );

        let report = check_reporting_invariants(tmp.path());
        let literature = report
            .required_failures()
            .into_iter()
            .filter(|failure| failure.starts_with("RC-LITERATURE:"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            literature.contains("2 row(s) spanning 1 distinct"),
            "{literature}"
        );
        assert!(
            literature.contains("asserts 2 distinct entities each contributed multiple")
                && literature.contains("establishes 1"),
            "{literature}"
        );
    }

    #[test]
    fn rc_literature_requires_one_table_row_per_retained_evidence_record() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "contextualize_findings_with_literature/claims_evidence_matrix.csv",
            "finding_id,entity,pmid,concordance_flag,searched\n\
             F1,GENE1,11111111,same_direction,true\n\
             F2,GENE1,22222222,opposite_direction,true\n",
        );
        write(
            &outputs,
            "contextualize_findings_with_literature/result.json",
            r#"{
                "n_entities_assessed": 1,
                "n_entities_not_assessed": 0,
                "n_evidence_rows_assessed": 2,
                "n_evidence_rows_total": 2
            }"#,
        );
        write(
            &outputs,
            "reporting/report-data.json",
            &serde_json::json!({
                "artifacts": [],
                "literature": {
                    "concordant": [{
                        "entity": "GENE1",
                        "pmid": "11111111",
                        "evidence_quote": "up",
                        "effect": 1.0,
                        "significance": 0.01
                    }],
                    "discordant": [{
                        "entity": "GENE1",
                        "pmid": "22222222",
                        "evidence_quote": "down",
                        "effect": 1.0,
                        "significance": 0.01
                    }],
                    "unverifiable": [],
                    "non_replications": [],
                    "novel_count": 0,
                    "not_assessed_count": 0,
                    "n_entities_assessed": 1,
                    "n_entities_not_assessed": 0,
                    "n_evidence_rows_assessed": 2,
                    "n_evidence_rows_total": 2,
                    "retrieved_sources": ["11111111", "22222222"]
                }
            })
            .to_string(),
        );
        write(
            &outputs,
            "reporting/report.md",
            "## Literature Contextualization\n\n\
             Summary of assessed entities (1 total):\n\
             - 1 concordant\n- 1 discordant\n- 0 unverifiable\n\n\
             ### Literature concordance table\n\n\
             | Entity | Verdict | PMIDs |\n\
             |---|---|---|\n\
             | GENE1 | concordant | 11111111; 22222222 |\n",
        );

        let collapsed = check_reporting_invariants(tmp.path());
        let failures = collapsed.required_failures();
        assert!(
            failures.iter().any(|failure| {
                failure.starts_with("RC-LITERATURE:")
                    && failure.contains("one row per evidence record")
                    && failure.contains("partition of assessed entities")
            }),
            "{failures:?}"
        );

        write(
            &outputs,
            "reporting/report.md",
            "## Literature Contextualization\n\n\
             One distinct entity was assessed. Two assessed evidence rows \
             comprised 1 concordant and 1 discordant evidence row.\n\n\
             ### Literature concordance evidence table\n\n\
             | Entity | Verdict | PMID | Effect | Significance |\n\
             |---|---|---|---:|---:|\n\
             | GENE1 | concordant | 11111111 | 1.0 | 0.01 |\n\
             | GENE1 | discordant | 22222222 | 1.0 | 0.01 |\n",
        );
        let corrected = check_reporting_invariants(tmp.path());
        assert!(
            corrected
                .required_failures()
                .iter()
                .all(|failure| !failure.starts_with("RC-LITERATURE:")),
            "{corrected:?}"
        );

        write(
            &outputs,
            "reporting/report.md",
            "## Literature Contextualization\n\n\
             One distinct entity was assessed. Two assessed evidence rows \
             comprised 1 concordant and 1 discordant evidence row.\n\n\
             ### Literature concordance evidence table\n\n\
             | Entity | Verdict | PMID | Effect | Significance |\n\
             |---|---|---|---:|---:|\n\
             | GENE1 | concordant | 11111111 | -9.0 | 0.01 |\n\
             | GENE1 | discordant | 22222222 | 1.0 | 0.01 |\n",
        );
        let wrong_measurement = check_reporting_invariants(tmp.path());
        assert!(
            wrong_measurement.required_failures().iter().any(|failure| {
                failure.starts_with("RC-LITERATURE:") && failure.contains("effect or significance")
            }),
            "{wrong_measurement:?}"
        );
    }

    #[test]
    fn rc_literature_link_rejects_distributing_one_source_across_an_entity_list() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write_root(
            tmp.path(),
            "policies/interpretation-policy.json",
            include_str!("../../../config/downstream-policy/interpretation-policy.json"),
        );
        write(
            &outputs,
            "contextualize_findings_with_literature/claims_evidence_matrix.csv",
            "finding_id,entity,pmid,prior_pmids,concordance_flag,searched,verified\n\
             F1,ALPHA,11111111,11111111,same_direction,true,true\n\
             F2,BETA,22222222,22222222,same_direction,true,true\n",
        );
        write(
            &outputs,
            "reporting/report.md",
            "The evidence context for ALPHA and BETA derives from PMID 11111111.",
        );

        let invalid = check_reporting_invariants(tmp.path());
        assert!(
            invalid.required_failures().iter().any(|failure| {
                failure.starts_with("RC-LITERATURE-LINK:")
                    && failure.contains("BETA")
                    && failure.contains("11111111")
            }),
            "{invalid:#?}"
        );

        write(
            &outputs,
            "reporting/report.md",
            "The evidence context for ALPHA derives from PMID 11111111. \
             The evidence context for BETA derives from PMID 22222222.",
        );
        let corrected = check_reporting_invariants(tmp.path());
        assert!(
            corrected
                .required_failures()
                .iter()
                .all(|failure| !failure.starts_with("RC-LITERATURE-LINK:")),
            "{corrected:#?}"
        );

        write(
            &outputs,
            "reporting/report.md",
            "The evidence context for GAMMA derives from PMID 33333333.",
        );
        let absent_entity = check_reporting_invariants(tmp.path());
        assert!(
            absent_entity.required_failures().iter().any(|failure| {
                failure.starts_with("RC-LITERATURE-LINK:")
                    && failure.contains("GAMMA")
                    && failure.contains("33333333")
            }),
            "{absent_entity:#?}"
        );
    }

    #[test]
    fn rc_attachment_rejects_effect_cutoff_not_used_to_generate_attachment() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write_root(
            tmp.path(),
            "WORKFLOW.json",
            &serde_json::json!({
                "tasks": {
                    "assemble_report_data": {
                        "spec": {
                            "report_schemas": {
                                "differential_expression": {
                                    "artifact": "de_results.tsv",
                                    "entity_column": "gene",
                                    "entity_column_aliases": ["gene_id"],
                                    "significance": {
                                        "column": "padj",
                                        "threshold": 0.05,
                                        "comparator": "lt"
                                    },
                                    "signed_effect_column": "log2FoldChange",
                                    "signed_effect_aliases": ["log2FC"],
                                    "grouping_column": null
                                }
                            }
                        }
                    }
                }
            })
            .to_string(),
        );
        write(
            &outputs,
            "reporting/report-data.json",
            &serde_json::json!({
                "artifacts": [{
                    "stage_id": "differential_expression",
                    "artifact": "de_results.tsv",
                    "n_total": 22369,
                    "n_significant": 4030,
                    "direction_split": null,
                    "effect_distribution": null,
                    "significant_entities": [],
                    "significant_table_path":
                        "runtime/outputs/differential_expression/de_results.significant.tsv",
                    "full_table_path":
                        "runtime/outputs/differential_expression/de_results.full.tsv",
                    "spilled_to_attachment_only": true
                }],
                "literature": null
            })
            .to_string(),
        );
        write(
            &outputs,
            "reporting/report.md",
            "| File | Description |\n\
             | --- | --- |\n\
             | `de_results.significant.tsv` | Significant DE genes \
             (padj <= 0.05, |log2FC| >= 1.0) |\n",
        );

        let report = check_reporting_invariants(tmp.path());
        let failures = report.required_failures().join("\n");
        assert!(
            failures.contains("RC-ATTACHMENT")
                && failures.contains("generated with padj < 0.05 only"),
            "{failures}"
        );

        write(
            &outputs,
            "reporting/report.md",
            "| File | Description |\n\
             | --- | --- |\n\
             | `de_results.significant.tsv` | Significant DE genes \
             (padj < 0.05; columns include log2FC and padj) |\n",
        );
        let corrected = check_reporting_invariants(tmp.path());
        assert!(
            corrected
                .required_failures()
                .iter()
                .all(|failure| !failure.starts_with("RC-ATTACHMENT:")),
            "{corrected:?}"
        );
    }

    #[test]
    fn rc_row_ranked_table_must_match_canonical_prefix() {
        assert_eq!(
            ranked_caption_size("Top enriched pathways by canonical ranking", 6),
            Some(6)
        );
        assert_eq!(ranked_caption_size("Top 2 enriched pathways", 6), Some(2));
        assert_eq!(ranked_caption_size("Selected pathways", 6), None);

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
        let misleading_caption = NarrativeTable {
            heading: "Top 2 enriched entities by effect size".into(),
            ..table(&[("A", "2.0", "0.01"), ("B", "3.0", "0.02")])
        };
        match verify_ranked_table(&misleading_caption, &binding, &source, &ranking, 2) {
            RankedTableCheck::Failure(detail) => {
                assert!(detail.contains("significance first"), "{detail}");
            }
            _ => panic!("an effect-first caption must fail a significance-first ranking"),
        }
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
    fn rc_row_rejects_missing_role_cell_when_source_has_value() {
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
            grouping_column: None,
        };
        let headers = csv::StringRecord::from(vec!["pathway", "NES", "padj"]);
        let rows = vec![csv::StringRecord::from(vec![
            "KEGG_INSULIN_SIGNALING_PATHWAY",
            "1.87248344882034",
            "0.00116661740275901",
        ])];
        let source =
            SourceRowIndex::build(headers, rows, &schema, &PolicyColumnSynonyms::default())
                .expect("source index");
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
            resolved: 1,
            agreeing: 0,
        };
        let table = NarrativeTable {
            line: 1,
            heading: "Selected pathway".into(),
            header: vec!["Pathway".into(), "NES".into(), "padj".into()],
            rows: vec![vec![
                "KEGG_INSULIN_SIGNALING_PATHWAY".into(),
                "—".into(),
                "NA".into(),
            ]],
        };
        let mut failures = Vec::new();
        let mut skipped = Vec::new();
        verify_bound_table(
            "reporting/report.md:1",
            &table,
            &binding,
            &source,
            &NarrativeTolerances {
                effect_absolute: 0.01,
                significance_relative: 0.05,
            },
            &mut failures,
            &mut skipped,
        );

        assert_eq!(failures.len(), 2, "{failures:?}");
        assert!(failures.iter().any(|failure| failure.contains("NES")));
        assert!(failures.iter().any(|failure| failure.contains("padj")));
        assert!(skipped.is_empty(), "{skipped:?}");
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
        write_root(
            root,
            "WORKFLOW.json",
            &serde_json::json!({
                "tasks": {
                    "assemble_report_data": {
                        "spec": {
                            "report_schemas": {
                                "pathway_enrichment": {
                                    "artifact": "pathway_results.tsv",
                                    "entity_column": "pathway",
                                    "entity_column_aliases": ["term"],
                                    "signed_effect_column": "NES",
                                    "significance": {
                                        "column": "padj",
                                        "comparator": "lt",
                                        "threshold": 0.25
                                    }
                                }
                            }
                        }
                    }
                }
            })
            .to_string(),
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
                failure.starts_with("RC-STAGE-NARRATIVE:") && failure.contains("GENE_SET_A")
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
    fn rc_stage_narrative_checks_numeric_row_even_when_sentence_cites_literature() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        seed_pathway_stage_narrative(tmp.path(), &outputs, "6.82e-05");
        write(
            &outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "gene_sets_collections": ["GO_BP"],
                "narrative": "GENE_SET_A was depleted (NES=-1.9024, padj=6.82e-05), consistent with prior work (PMID 12345678)."
            })
            .to_string(),
        );

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

    #[test]
    fn rc_rank_reconciles_mapping_and_duplicate_label_losses() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        seed_ranked_genes(&outputs, 2);
        write(
            &outputs,
            "pathway_enrichment/annotation/symbol_map.tsv",
            "symbol\tensembl_gene_id\n\
             GENE1\tENSG1\n\
             GENE1\tENSG2\n\
             GENE2\tENSG3\n",
        );
        write(
            &outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "n_genes_pre_mapping": 4,
                "n_genes_mapped": 3,
                "n_genes_unmapped": 1,
                "n_genes_ranked": 2,
                "n_duplicate_gene_labels_removed": 1,
                "narrative": "Preranked fgsea included 2 genes."
            })
            .to_string(),
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(
            report
                .required_failures()
                .iter()
                .all(|failure| !failure.starts_with("RC-RANK:")),
            "{report:?}"
        );

        write(
            &outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "n_genes_pre_mapping": 4,
                "n_genes_mapped": 3,
                "n_genes_unmapped": 2,
                "n_genes_ranked": 2,
                "n_duplicate_gene_labels_removed": 0,
                "narrative": "Preranked fgsea included 2 genes."
            })
            .to_string(),
        );
        let wrong = check_reporting_invariants(tmp.path());
        let failures = wrong.required_failures().join("\n");
        assert!(
            failures.contains("n_genes_unmapped=2, recomputed=1")
                && failures.contains("n_duplicate_gene_labels_removed=0, recomputed=1"),
            "{failures}"
        );
    }

    #[test]
    fn rc_rank_rejects_ranked_population_reported_as_duplicate_removals() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        seed_ranked_genes(&outputs, 2);
        write(
            &outputs,
            "pathway_enrichment/annotation/symbol_map.tsv",
            "symbol\tensembl_gene_id\n\
             GENE1\tENSG1\n\
             GENE1\tENSG2\n\
             GENE2\tENSG3\n",
        );
        write(
            &outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "n_genes_pre_mapping": 4,
                "n_genes_mapped": 3,
                "n_genes_unmapped": 1,
                "n_genes_ranked": 2,
                "n_duplicate_gene_labels_removed": 1
            })
            .to_string(),
        );
        write(
            &outputs,
            "reporting/report.md",
            "After removing 2 duplicate gene-symbol labels, 2 genes were ranked for fgsea.",
        );

        let wrong = check_reporting_invariants(tmp.path());
        assert!(wrong.required_failures().iter().any(|failure| {
            failure.starts_with("RC-RANK:")
                && failure.contains("duplicate-label removal count=2")
                && failure.contains("recomputed=1")
        }));

        write(
            &outputs,
            "reporting/report.md",
            "After removing 1 duplicate gene-symbol label, 2 genes were ranked for fgsea.",
        );
        let corrected = check_reporting_invariants(tmp.path());
        assert!(corrected
            .required_failures()
            .iter()
            .all(|failure| !failure.starts_with("RC-RANK:")));
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
                "gene_sets_tested": { "HALLMARK": 2, "GO_BP": 3, "total": 5 },
                "collections": ["HALLMARK", "GO_BP"]
            })
            .to_string(),
        );
        write(
            &outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "gene_sets_collections": ["HALLMARK", "GO_BP"]
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

    // -- RC-METHOD (required) --------------------------------------------

    #[test]
    fn rc_method_rejects_wrong_pathway_implementation_label() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "method": "clusterProfiler::gseGO + gseKEGG"
            })
            .to_string(),
        );
        write(
            &outputs,
            "reporting/result.json",
            &serde_json::json!({
                "summary": "1,450 pathways passed pathway-level (fgsea) FDR < 0.25",
                "pathway_summary": {
                    "threshold": "pathway-level (fgsea) FDR < 0.25"
                }
            })
            .to_string(),
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RC-METHOD"));
        assert!(!report.passed(), "{report:?}");
        assert!(
            report
                .required_failures()
                .iter()
                .any(|failure| failure.contains("RC-METHOD")
                    && failure.contains("fgsea")
                    && failure.contains("clusterprofiler")),
            "{:?}",
            report.required_failures()
        );
    }

    #[test]
    fn rc_method_accepts_method_neutral_pathway_label() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "method": "clusterProfiler::gseGO + gseKEGG"
            })
            .to_string(),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "1,450 pathways passed the pathway-level adjusted p-value (padj) \
             threshold of 0.25. Enrichment used clusterProfiler GSEA.\n",
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RC-METHOD"));
        assert!(
            report
                .required_failures()
                .iter()
                .all(|failure| !failure.contains("RC-METHOD")),
            "{:?}",
            report.required_failures()
        );
    }

    // -- RC-FINAL-FIDELITY (required) -----------------------------------

    #[test]
    fn rc_final_fidelity_rejects_rewritten_upstream_report() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "reporting/report.md",
            "# Primary results\n\n| Gene | log2FC | padj |\n\
             |---|---:|---:|\n| TSC22D3 | 2.68567 | 3.342277e-19 |\n",
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            "# Final report\n\n# Primary results\n\n| Gene | log2FC | padj |\n\
             |---|---:|---:|\n| TSC22D3 | -0.064 | 0.763 |\n",
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RC-FINAL-FIDELITY"));
        assert!(
            report
                .required_failures()
                .iter()
                .any(|failure| failure.contains("RC-FINAL-FIDELITY")),
            "{:?}",
            report.required_failures()
        );
    }

    #[test]
    fn rc_final_fidelity_accepts_verbatim_report_with_dashboard_wrapper() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        let upstream = "# Primary results\n\n| Gene | log2FC | padj |\n\
                        |---|---:|---:|\n| TSC22D3 | 2.68567 | 3.342277e-19 |\n";
        write(&outputs, "reporting/report.md", upstream);
        write(
            &outputs,
            "final_reporting/final_report.md",
            &format!(
                "# Project dashboard\n\nSee `dashboard_index.json`.\n\n{upstream}\n\
                 ## Dashboard files\n\n- `figures/summary_dashboard.png`\n"
            ),
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RC-FINAL-FIDELITY"));
        assert!(
            report
                .required_failures()
                .iter()
                .all(|failure| !failure.contains("RC-FINAL-FIDELITY")),
            "{:?}",
            report.required_failures()
        );
    }

    #[test]
    fn rc_final_fidelity_ignores_independently_appended_system_blocks() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        let upstream = "# Primary results\n\nTSC22D3 had log2FC 2.68567.\n";
        let full_tables = format!("{FULL_TABLE_START}\nsystem table\n{FULL_TABLE_END}\n");
        let provenance = format!(
            "{}\nsystem provenance\n{}\n",
            crate::report_contract::provenance_section::DATA_PROVENANCE_START,
            crate::report_contract::provenance_section::DATA_PROVENANCE_END
        );
        write(
            &outputs,
            "reporting/report.md",
            &format!("{upstream}\n{full_tables}\n{provenance}"),
        );
        write(
            &outputs,
            "final_reporting/final_report.md",
            &format!(
                "# Project dashboard\n\n{upstream}\n## Dashboard files\n\nNavigation only.\n\n\
                 {full_tables}\n{provenance}"
            ),
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RC-FINAL-FIDELITY"));
        assert!(
            report
                .required_failures()
                .iter()
                .all(|failure| !failure.contains("RC-FINAL-FIDELITY")),
            "{:?}",
            report.required_failures()
        );
    }

    // -- RC-METRIC-DEFINITION (required, structure-driven) ---------------

    #[test]
    fn rc_metric_definition_rejects_definition_drift_and_reversed_reference_relation() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "association_scoring/result.json",
            &serde_json::json!({
                "signal_reference_ratio": 0.4,
                "signal_reference_ratio_description":
                    "signal_reference_ratio = 0.4 is the median signal in the selected \
                     entities divided by the median signal in the complete source cohort.",
                "signal_reference_ratio_basis": {
                    "computed": true,
                    "statistic": "ratio_of_medians",
                    "neutral_reference": 0.5,
                    "value": 0.4
                }
            })
            .to_string(),
        );
        write(
            &outputs,
            "reporting/report.md",
            "The `signal_reference_ratio` is 0.4: it is the median signal in the selected \
             entities divided by the median signal in the retained cohort, placing the \
             selected entities above the retained neutral reference 0.5.\n",
        );

        let report = check_reporting_invariants(tmp.path());
        let failures = report.required_failures().join(" | ");
        assert!(
            failures.contains("RC-METRIC-DEFINITION")
                && failures.contains("does not preserve")
                && failures.contains("below"),
            "{failures}"
        );
    }

    #[test]
    fn rc_metric_definition_accepts_retained_definition_and_ratio_relation() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        let description = "signal_reference_ratio = 0.4 is the median signal in the selected \
                           entities divided by the median signal in the complete source cohort";
        write(
            &outputs,
            "association_scoring/result.json",
            &serde_json::json!({
                "summaries": {
                    "primary": {
                        "signal_reference_ratio": 0.4,
                        "signal_reference_ratio_description": description,
                        "signal_reference_ratio_basis": {
                            "computed": true,
                            "statistic": "ratio_of_medians",
                            "neutral_reference": 0.5,
                            "value": 0.4
                        }
                    }
                }
            })
            .to_string(),
        );
        write(
            &outputs,
            "reporting/report.md",
            &format!(
                "{description}; `signal_reference_ratio` is therefore below the retained \
                 neutral reference 0.5.\n"
            ),
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(report.checked.contains(&"RC-METRIC-DEFINITION"));
        assert!(
            report
                .required_failures()
                .iter()
                .all(|failure| !failure.contains("RC-METRIC-DEFINITION")),
            "{report:?}"
        );
    }

    #[test]
    fn rc_metric_definition_rejects_unsupported_reliability_inference_from_pretty_key() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        let description = "top_effect_abundance_ratio = 4.9 is the median information value in \
                           the selected features divided by the median over all tested features";
        write(
            &outputs,
            "association_scoring/result.json",
            &serde_json::json!({
                "top_effect_abundance_ratio": 4.9,
                "top_effect_abundance_ratio_description": description,
                "top_effect_abundance_ratio_basis": {
                    "computed": true,
                    "statistic": "ratio_of_medians",
                    "neutral_reference": 1.0,
                    "value": 4.9
                }
            })
            .to_string(),
        );
        write(
            &outputs,
            "reporting/report.md",
            &format!(
                "{description}. The top-effect-abundance ratio is above 1 and therefore \
                 indicates reliable effect estimation."
            ),
        );
        let report = check_reporting_invariants(tmp.path());
        let failures = report.required_failures().join(" | ");
        assert!(
            failures.contains("RC-METRIC-DEFINITION") && failures.contains("infers `reliable`"),
            "{failures}"
        );
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
                "gene_sets_tested": { "HALLMARK": 2, "GO-BP": 3, "total": 5 },
                "collections": ["hallmark", "go_bp"]
            })
            .to_string(),
        );
        write(
            &outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "gene_sets_collections": ["hallmark", "go_bp"]
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
                "gene_sets_tested": { "HALLMARK": 2, "GO_BP": 3, "MYSTERY": 99, "total": 5 },
                "collections": ["HALLMARK", "GO_BP"]
            })
            .to_string(),
        );
        write(
            &outputs,
            "pathway_enrichment/result.json",
            &serde_json::json!({
                "gene_sets_collections": ["HALLMARK", "GO_BP"]
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
                "gene_sets_tested": { "HALLMARK": 2, "GO_BP": 3, "MYSTERY": 99, "total": 10085 },
                "collections": ["HALLMARK", "GO_BP"]
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
        assert_eq!(
            section_has_content(
                "## QC and Pre-processing\n\nFiltering details are retained here.\n",
                "qc_preprocessing"
            ),
            Some(true),
            "heading punctuation must not change a contract token"
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
    fn rp_qc_accepts_explicit_unperformed_metadata_and_matching_report() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "reporting/report.md",
            "### Sample-outlier assessment\n\nNo sample-outlier assessment was performed \
             in this run. No PCA, sample-distance, or sample-correlation QC artifact was \
             produced.\n",
        );
        write(
            &outputs,
            "quality_control/qc_summary.json",
            &serde_json::json!({
                "sample_outlier_assessment": "not_performed",
                "sample_count": 12
            })
            .to_string(),
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(
            report
                .required_failures()
                .iter()
                .all(|failure| !failure.starts_with("RP-QC:")),
            "absence metadata is not a retained outlier computation: {report:?}"
        );
    }

    #[test]
    fn rp_qc_rejects_denial_of_retained_sample_distance_matrix() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "reporting/report.md",
            "### QC\n\nNo sample-outlier assessment was performed; no outlier table, \
             Cook's-distance output, PCA outlier score, or sample-distance matrix was \
             produced and retained as a package artifact.\n",
        );
        write(
            &outputs,
            "normalisation/intermediates/sample_distances.tsv",
            "sample\tS1\nS1\t0\n",
        );

        let report = check_reporting_invariants(tmp.path());
        let failures = report.required_failures().join("\n");
        assert!(
            failures.contains("RP-QC")
                && failures.contains("sample-distance")
                && failures.contains("sample_distances.tsv"),
            "{failures}"
        );
    }

    #[test]
    fn rp_qc_does_not_equate_pca_coordinates_with_outlier_scores() {
        let tmp = TempDir::new().unwrap();
        let outputs = outputs_dir(&tmp);
        write(
            &outputs,
            "reporting/report.md",
            "### QC\n\nNo PCA outlier score was produced.\n",
        );
        write(
            &outputs,
            "normalisation/intermediates/pca_coords.tsv",
            "sample\tPC1\tPC2\nS1\t0\t0\n",
        );

        let report = check_reporting_invariants(tmp.path());
        assert!(
            report
                .required_failures()
                .iter()
                .all(|failure| !failure.starts_with("RP-QC:")),
            "{report:?}"
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
