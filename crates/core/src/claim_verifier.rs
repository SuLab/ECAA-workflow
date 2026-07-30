//! Verify extracted [`Claim`]s against cited result tables.
//!
//! Consumes the output of [`claim_extractor::extract_claims`] plus the
//! package's `results/tables/` directory and produces a
//! [`ClaimVerificationReport`] that classifies each claim as:
//!
//! * **Verified** — the table row exists and all mentioned numeric
//!   slots agree within the policy's configured tolerances.
//! * **Mismatch** — the table row exists but a claimed value contradicts
//!   the observed one (wrong sign on the effect size, p-value off by
//!   more than the relative tolerance, etc.). The `detail` field spells
//!   out which slot disagreed.
//! * **Unverifiable** — the claim did not cite a table, or the cited
//!   table doesn't exist, or the entity name isn't present in any
//!   configured `entityColumns`.
//!
//! The verifier is deterministic Rust — no LLM, no network. Table lookup
//! uses `csv` crate with `BufReader` so very large tables stay bounded
//! in memory.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use ts_rs::TS;
use unicode_normalization::UnicodeNormalization;

use crate::claim_contract::ClaimContract;
use crate::claim_extractor::{Claim, Direction, ExtractorConfig};

/// Static regex for `verify_rank_top_n`'s "top-N" parser. Hoisted to
/// module scope so the pattern is compiled once at first use instead of
/// recompiled per claim — the original in-function `Regex::new` showed
/// up as a hot spot under high-volume verification batches.
static RANK_TOP_N_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\btop[\s-](\d+)\b").expect("static regex"));

/// An anaphoric rank scope whose candidate set is outside the extracted
/// sentence ("within this top-by-significance tier"). The verifier receives a
/// claim excerpt and a source table, not the preceding rendered table, so
/// treating this as a global argmax/argmin manufactures a false mismatch.
static ANAPHORIC_RANK_SCOPE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:within\s+(?:this|the)\s+(?:displayed\s+)?(?:top(?:[\s-]+by[\s-]+[a-z0-9_-]+)?(?:\s+tier)?|tier|subset|table)|among\s+(?:this\s+(?:displayed\s+)?(?:top(?:[\s-]+by[\s-]+[a-z0-9_-]+)?(?:\s+tier)?|tier|subset|table)|the\s+displayed\s+(?:top(?:[\s-]+by[\s-]+[a-z0-9_-]+)?(?:\s+tier)?|tier|subset|table)))\b",
    )
    .expect("static regex")
});

/// Floor for the SOFT top-N threshold (a vague "one of the top" claim with no
/// explicit number). A soft claim never resolves to fewer than this many rows,
/// so a small table still admits a reasonable "one of the top" set.
const DEFAULT_SOFT_FLOOR: usize = 10;

/// Fraction of the ranked-row count a SOFT top-N claim is allowed to span (top
/// 1%). For a large table this dominates the floor — e.g. 22,369 ranked genes →
/// `ceil(0.01 × 22_369) = 224`, so a gene at rank 31 ("the top 0.14%") honestly
/// counts as "one of the top DE genes" while a rank-5,000 gene does not.
const SOFT_TOP_PERCENTILE: f64 = 0.01;

/// Canonical normalization for string-equality / substring tests
/// between narrative text and table cells: Unicode NFC composition
/// followed by ASCII-strict casefold. The combination keeps composed
/// vs decomposed accents from producing spurious mismatches while
/// avoiding the Unicode-casefold table (which would inflate the
/// binary and obscure the audit trail for ASCII-only cells, which
/// is the overwhelmingly common case).
fn normalize(s: &str) -> String {
    s.nfc().collect::<String>().to_ascii_lowercase()
}

/// Coarse id-namespace class for the VF-0 (Suspicious) absent-entity guard.
/// Distinguishes three classes:
///   * `ensembl` — Ensembl-family stable ids (`ENSG…`, `ENSMUSG…`, `ENST…`);
///   * `set` — a multi-WORD value (contains internal ASCII whitespace after
///     trimming), the signature of a gene-SET / pathway / GO-term NAME
///     ("TNF-alpha Signaling via NF-kB", "Oxidative Phosphorylation");
///   * `symbol` — everything else: a single bare token (gene symbol, etc.).
///
/// A1 FIX: a pathway/term row keyed on a multi-word SET name is class `set`,
/// NOT `symbol`. Previously a bare token claim ("TNF") looked up against a
/// pathway table whose entity column held set NAMES classed both as `symbol`,
/// so `namespace_matches_table` falsely matched and VF-0 wrongly fired
/// Suspicious. A single-token symbol/ensembl claim must NOT be treated as the
/// same namespace as a multi-word set name.
fn id_namespace(token: &str) -> &'static str {
    let t = token.trim();
    // A set NAME has internal whitespace (e.g. "TNF-alpha Signaling via NF-kB").
    // A bare gene symbol or Ensembl id never does. Classify multi-word values
    // as `set` first so they can never collide with a single-token symbol.
    if t.split_whitespace().nth(1).is_some() {
        return "set";
    }
    let upper = t.to_ascii_uppercase();
    if upper.starts_with("ENS") {
        // ENS + optional species (up to 4 letters) + G/T/P + ≥6 digits.
        let rest = &upper[3..];
        let letters: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        let digits: String = rest[letters.len()..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if letters.len() <= 5 && digits.len() >= 6 {
            return "ensembl";
        }
    }
    "symbol"
}

/// Number of rows sampled to classify a table's entity-column namespace.
/// A pathway/enrichment table interleaves single-word term names
/// ("Adipogenesis", "Apoptosis") with multi-word ones, so the FIRST row is
/// not a reliable witness — we must scan a window.
const NAMESPACE_SAMPLE_ROWS: usize = 32;

/// Lowercased phrases by which a narrative POSITIVELY asserts agreement with
/// prior literature. A literature-grounded claim only contradicts the matrix
/// when it asserts concordance against a flag that says otherwise; a neutral
/// or faithful-discordance mention carries none of these cues and is not a
/// fabricated-concordance contradiction. Shared by the `opposite_direction`
/// and `no_prior_finding` branches of `verify_literature_grounded_at` so the
/// concordance test is applied symmetrically.
const AGREEMENT_CUES: &[&str] = &[
    "concordant",
    "consistent with prior",
    "consistent with previous",
    "in agreement with",
    "agrees with prior",
    "as previously reported",
    "as previously shown",
    "confirms prior",
    "confirms previous",
    "replicates prior",
    "in line with prior",
    "matches prior",
];

/// The id-namespace of a TABLE's entity column, classified from a sampled
/// window of rows rather than the first row alone.
///
/// A2 FIX: a pathway/term table can have a SINGLE-WORD term name in its first
/// (top-ranked) row — e.g. "Adipogenesis" — which `id_namespace` classes as
/// `symbol`, indistinguishable from a gene symbol. Sampling only the first row
/// therefore mis-typed such a table as `symbol`-keyed, so an absent bare-token
/// claim ("TNF", extracted from the prose "TNF-alpha Signaling via NF-kB")
/// falsely matched the namespace and VF-0 fired Suspicious on a finding that is
/// actually PRESENT in the table under its full set name. We now class the
/// table as `set` if ANY sampled row carries a multi-word set name — a single
/// multi-word term cannot occur in a true symbol/Ensembl column, so this is a
/// conservative, false-positive-only-reducing witness. Falls back to the first
/// row's class when every sampled row is single-token.
fn table_namespace(cached: &CachedTable) -> Option<&'static str> {
    let first = cached.rows.first()?;
    let saw_set = cached
        .rows
        .iter()
        .take(NAMESPACE_SAMPLE_ROWS)
        .any(|r| id_namespace(&r.entity) == "set");
    if saw_set {
        Some("set")
    } else {
        Some(id_namespace(&first.entity))
    }
}

/// True when the claim entity's id-namespace matches the cited table's
/// entity-column namespace. Used by VF-0 so an absent entity is only flagged
/// Suspicious when its absence is a real negative in the SAME namespace, not a
/// symbol-vs-Ensembl (or symbol-vs-pathway-set) lookup artifact.
///
/// A1 FIX: a single-token symbol/ensembl claim against a `set`-keyed table
/// (pathway/term row holding a multi-word set NAME) returns FALSE, so VF-0 and
/// sibling-discovery fall through to Unverifiable rather than Suspicious — a
/// bare "TNF" cited from a pathway table is a benign cross-namespace miss,
/// never a fabricated finding. A2 FIX: the table's namespace is now sampled
/// across a row window (see `table_namespace`) so a pathway table whose FIRST
/// row is a single-word term name is still recognised as `set`-keyed.
fn namespace_matches_table(claim_entity: &str, cached: &CachedTable) -> bool {
    match table_namespace(cached) {
        Some(table_ns) => id_namespace(claim_entity) == table_ns,
        // Empty table: no namespace to compare — treat as non-matching so we
        // stay Unverifiable rather than guess.
        None => false,
    }
}

/// SME-safe table reference: the file's base name only (or `?` when
/// the path has none). Used inside human-readable `Mismatch`/
/// `Unverifiable` `detail`/`reason` strings so an absolute path like
/// `/tmp/scripps-e2e-packages/...session.../results/tables/de.tsv`
/// is never surfaced verbatim to the SME — they see `de.tsv`. The
/// UI's `sanitizeForSme` is a separate defense layer (the
/// `runtime|results` path pattern only catches paths anchored at
/// those prefixes); this trims at the source.
fn table_label(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .map(String::from)
        .unwrap_or_else(|| "?".into())
}

/// Package-root-relative path with forward slashes (e.g.
/// `runtime/outputs/data_acquisition/cohort_manifest.tsv`), used as a
/// discovered claim's `source_table` so the projected `supported_by`
/// evidence reference points at the directory the table actually lives in.
/// Falls back to the bare file name if `path` is not under `package_root`.
fn package_relative_label(path: &Path, package_root: &Path) -> String {
    path.strip_prefix(package_root)
        .ok()
        .and_then(|p| p.to_str())
        .map(|s| s.replace('\\', "/"))
        .unwrap_or_else(|| table_label(path))
}

/// Per-claim verdict.
#[derive(
    Debug, Clone, Serialize, Deserialize, PartialEq, TS, strum::EnumCount, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ClaimStatus {
    /// The cited table row was found and every mentioned slot matches
    /// within the configured tolerance.
    Verified,
    /// A specific slot disagreed. `detail` describes which and how.
    Mismatch { detail: String },
    /// The claim could not be cross-checked (no table cited, table
    /// missing, entity not in any configured entity column, etc.).
    Unverifiable { reason: String },
    /// The claim was NEVER ADJUDICATED — there was no adjudication site to
    /// run at all (no evidence file present, an in-result self-reference, no
    /// countable/per-entity quantity to check, or a resolution gap). Distinct
    /// from [`Self::Unverifiable`], which means a table WAS loaded and checked
    /// but yielded nothing determinable (no effect/p column). Splitting the
    /// two keeps coverage honest: relabeling a checked-but-undeterminable
    /// claim as `Pending` (or vice versa) cannot inflate any verified/coverage
    /// floor, because both are non-Verified and counted in their own bucket.
    Pending { reason: String },
    /// A confident quantitative claim was attributed to an entity that is
    /// ABSENT from a successfully-loaded cited table whose id-namespace
    /// matches the claim token — the signature of a fabricated or untested
    /// finding. SOFT/review-required: it is surfaced and counted separately
    /// (`n_suspicious`) but, unlike `Mismatch`, never hard-blocks the run —
    /// so it raises catch-recall on the unverifiable-as-evasion gap without
    /// risking a false block on a faithful narrative. A pure interpretation
    /// sentence, a bare mention, or a namespace-mismatched symbol stays
    /// `Unverifiable`, never `Suspicious`.
    Suspicious { reason: String },
}

/// Per-claim verdict plus the source claim itself (so callers can
/// render the excerpt alongside the status without re-zipping).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ClaimVerdict {
    /// Claim.
    pub claim: Claim,
    /// Status.
    pub status: ClaimStatus,
    /// Confirmatory-mode classification of the claim's
    /// analytical discipline. `Prespecified` when the claim's supporting
    /// stage has no `PostHocDeviation` record; `PostHoc` when at least
    /// one deviation record covers the stage lineage; `Exploratory` when
    /// the session was never confirmatory. The UI surfaces a red flag
    /// when a `Prespecified` claim's lineage turns out to contain
    /// deviations.
    #[serde(default)]
    pub strength: ClaimStrength,
    /// The reconstructable audit tuple for this verdict (source table, entity /
    /// measurement columns, claimed vs observed values, comparator, tolerances,
    /// class tag, verifier version, rationale). `None` for verdicts produced
    /// before the numeric gate ran, or when no audit context was captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub audit: Option<VerdictAudit>,
}

/// Claim-strength classification for confirmatory-mode demotion.
/// Exploratory sessions emit `Exploratory` for every claim and demotion
/// is a no-op; confirmatory sessions walk the `PostHocDeviation` log to
/// pick `Prespecified` vs `PostHoc`.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ClaimStrength {
    /// Prespecified variant.
    Prespecified,
    /// PostHoc variant.
    PostHoc,
    #[default]
    /// Exploratory variant.
    Exploratory,
}

/// Verifier-implementation version stamped onto every [`VerdictAudit`] so a
/// persisted verdict tuple records which comparator logic produced it. Bump
/// when the numeric-gate comparison semantics change (order-of-magnitude band,
/// tolerance defaults) so an older recorded verdict is not silently re-read
/// under new semantics.
pub const CLAIM_VERIFIER_VERSION: &str = "2.0";

/// Coarse verification-method class tag reported per verdict and tallied into
/// the persisted header's per-class counts. Total over every verdict: every
/// verdict maps to exactly one class, so the per-class counts always sum to
/// `n_checked`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VerdictClass {
    /// A numeric value in the claim was compared against a table cell
    /// (log2FC / p-value / count / ratio).
    NumericTable,
    /// Only an up/down direction was checked (no numeric assertion present).
    Directional,
    /// The check concerned whether a named entity is present / ranked in a
    /// table (categorical, rank/top-N, argmax/argmin membership).
    EntityPresence,
    /// A literature-grounded support claim adjudicated against the
    /// PMID-anchored `claims_evidence_matrix.csv`.
    LiteratureQuotation,
}

/// The reconstructable audit tuple for a single verdict. When the numeric gate
/// compared a claimed quantity against a table cell the numeric slots are
/// populated; for a non-numeric verdict only `class`/`verifier_version`/
/// `entity_value` are set. Persisted verbatim into
/// `runtime/claim-verification.json` and the signed sink so a reviewer can
/// reconstruct *what was compared against what, and how*.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct VerdictAudit {
    /// The verification-method class for this verdict.
    pub class: VerdictClass,
    /// Package-relative (or basename) reference of the table the observed value
    /// was read from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_table: Option<String>,
    /// The table's entity column the row was resolved by (e.g. `gene`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub entity_column: Option<String>,
    /// The claim's entity value used for the row lookup (e.g. `CRISPLD2`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub entity_value: Option<String>,
    /// The table column the observed value was read from (e.g. `padj`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub measurement_column: Option<String>,
    /// The numeric value asserted in the claim text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub claimed_value: Option<f64>,
    /// The numeric value observed in the table cell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub observed_value: Option<f64>,
    /// The comparison performed (`order_of_magnitude` for p-values, `absolute`
    /// for log2FC / effect sizes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub comparison_operator: Option<String>,
    /// Absolute tolerance used (log2FC / effect size).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub absolute_tolerance: Option<f64>,
    /// Relative / order-of-magnitude tolerance used (p-values, in orders of
    /// magnitude).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub relative_tolerance: Option<f64>,
    /// Any unit conversion applied before comparison (e.g. `linear_fold->log2`);
    /// `None` when the claim and table share units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub unit_conversion: Option<String>,
    /// The verifier-implementation version that produced this verdict.
    pub verifier_version: String,
    /// Human-readable rationale for the verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rationale: Option<String>,
    /// Fraction of numeric tokens found in the claim text that were actually
    /// compared against a table cell (parse-coverage metric). `1.0` when there
    /// were no numeric tokens or all were compared; `< 1.0` when a token was
    /// unparseable or its entity/column could not be resolved.
    pub parse_coverage: f64,
}

impl VerdictAudit {
    /// A class-only audit skeleton with no numeric comparison recorded
    /// (`parse_coverage` defaults to `1.0` — nothing numeric to compare).
    fn class_only(class: VerdictClass, entity_value: Option<String>) -> Self {
        Self {
            class,
            source_table: None,
            entity_column: None,
            entity_value,
            measurement_column: None,
            claimed_value: None,
            observed_value: None,
            comparison_operator: None,
            absolute_tolerance: None,
            relative_tolerance: None,
            unit_conversion: None,
            verifier_version: CLAIM_VERIFIER_VERSION.to_string(),
            rationale: None,
            parse_coverage: 1.0,
        }
    }
}

/// Rollup of every claim in one narrative artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ClaimVerificationReport {
    /// N checked.
    pub n_checked: usize,
    /// N verified.
    pub n_verified: usize,
    /// N mismatch.
    pub n_mismatch: usize,
    /// N unverifiable (table loaded + checked but undeterminable).
    pub n_unverifiable: usize,
    /// N pending (never adjudicated — no adjudication site ran at all).
    /// Defaults to 0 so older serialized reports without the field still
    /// deserialize.
    #[serde(default)]
    pub n_pending: usize,
    /// N suspicious (soft / review-required; never blocks). Defaults to 0 so
    /// older serialized reports without the field still deserialize.
    #[serde(default)]
    pub n_suspicious: usize,
    /// Verdicts.
    pub verdicts: Vec<ClaimVerdict>,
    /// Dual-channel audit cross-reference.
    /// Path (relative to the emitted package root) of the task's
    /// agent-runtime decision log, when present. The UI links it from
    /// the verification badge so reviewers can cross-check
    /// SME-visible `decisions.jsonl` deviations against the runtime
    /// decisions the agent recorded while executing the stage. `None`
    /// when the agent did not write a runtime log (older packages /
    /// non-instrumented agents).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub runtime_decision_log_path: Option<String>,
}

impl ClaimVerificationReport {
    /// Empty.
    pub fn empty() -> Self {
        Self {
            n_checked: 0,
            n_verified: 0,
            n_mismatch: 0,
            n_unverifiable: 0,
            n_pending: 0,
            n_suspicious: 0,
            verdicts: Vec::new(),
            runtime_decision_log_path: None,
        }
    }

    /// Push.
    pub fn push(&mut self, verdict: ClaimVerdict) {
        self.n_checked += 1;
        match &verdict.status {
            ClaimStatus::Verified => self.n_verified += 1,
            ClaimStatus::Mismatch { .. } => self.n_mismatch += 1,
            ClaimStatus::Unverifiable { .. } => self.n_unverifiable += 1,
            ClaimStatus::Pending { .. } => self.n_pending += 1,
            ClaimStatus::Suspicious { .. } => self.n_suspicious += 1,
        }
        self.verdicts.push(verdict);
    }

    /// True iff at least one claim was classified as `Mismatch`. Used by
    /// the session-state hook to decide whether to transition to
    /// `Blocked { ValidationFailed }`. `Suspicious` deliberately does NOT
    /// trip this — it is soft/review-required, never a hard block.
    pub fn has_mismatch(&self) -> bool {
        self.n_mismatch > 0
    }

    /// True iff at least one claim was flagged `Suspicious` (a confident
    /// quantitative claim about an entity absent from its cited table).
    /// Surfaced for review; never blocks the run.
    pub fn has_suspicious(&self) -> bool {
        self.n_suspicious > 0
    }
}

/// Verify every `claim` against the tables under `tables_root`.
///
/// `tables_root` is typically `<package>/results/tables/`; the verifier
/// resolves each claim's `source_table` by scanning that directory for
/// a matching file name. If no `source_table` was extracted, the claim
/// is unverifiable by construction.
pub fn verify_claims(
    claims: &[Claim],
    tables_root: &Path,
    cfg: &ExtractorConfig,
) -> ClaimVerificationReport {
    let mut report = ClaimVerificationReport::empty();
    let index = TableIndex::scan(tables_root);
    // Per-call table cache: keyed by resolved table `PathBuf`. Lazily
    // populated on first claim referencing each table so the second and
    // subsequent claims against the same source_table reuse one CSV
    // parse + one entity-index map.
    let mut cache: BTreeMap<PathBuf, CachedTable> = BTreeMap::new();

    let mut verdicts = Vec::with_capacity(claims.len());
    for claim in claims {
        let (status, audit) = verify_for_contract_audited(claim, &index, cfg, &mut cache);
        verdicts.push(ClaimVerdict {
            claim: claim.clone(),
            status,
            strength: ClaimStrength::Exploratory,
            audit,
        });
    }
    demote_contradicted_missing_row_mismatches(&mut verdicts);
    for verdict in verdicts {
        report.push(verdict);
    }
    report
}

/// Report-level self-consistency guard. A literature claim can land BOTH a
/// `Verified` verdict and a "no supporting row" `Mismatch` for the same
/// `(entity, cited PMID)` pair when two excerpts cite the same finding and one
/// resolves through an annotation-map alias the other did not — a contradiction
/// the verifier should never surface. When a `(normalize(entity), PMID)` pair is
/// Verified anywhere in the report, demote any "missing supporting row" Mismatch
/// keyed on that same pair to `Unverifiable` (it is not load-bearing evidence of
/// fabrication, since the pair WAS verified elsewhere).
///
/// Scope is deliberately narrow: only the missing-supporting-row Mismatch class
/// is touched. `opposite_direction` and fabricated-concordance Mismatches key on
/// concordance FLAGS, not a missing row, and are never demoted — a genuinely
/// absent `(gene, PMID)` Mismatch with no Verified twin is also preserved.
fn demote_contradicted_missing_row_mismatches(verdicts: &mut [ClaimVerdict]) {
    const MISSING_ROW_CUE: &str = "has no such supporting row";

    // (normalize(entity), PMID) pairs that are Verified somewhere in the report.
    let mut verified_pairs: std::collections::BTreeSet<(String, u64)> =
        std::collections::BTreeSet::new();
    for v in verdicts.iter() {
        if !matches!(v.status, ClaimStatus::Verified) {
            continue;
        }
        let Some(ev) = v.claim.literature_evidence.as_ref() else {
            continue;
        };
        let ent = normalize(&v.claim.entity);
        for pmid in &ev.cited_pmids {
            verified_pairs.insert((ent.clone(), *pmid));
        }
    }
    if verified_pairs.is_empty() {
        return;
    }

    for v in verdicts.iter_mut() {
        let ClaimStatus::Mismatch { detail } = &v.status else {
            continue;
        };
        if !detail.contains(MISSING_ROW_CUE) {
            continue;
        }
        let Some(ev) = v.claim.literature_evidence.as_ref() else {
            continue;
        };
        let ent = normalize(&v.claim.entity);
        let contradicted = ev
            .cited_pmids
            .iter()
            .any(|pmid| verified_pairs.contains(&(ent.clone(), *pmid)));
        if contradicted {
            v.status = ClaimStatus::Unverifiable {
                reason: format!(
                    "literature: missing-supporting-row mismatch for `{}` is contradicted by a verified citation of the same finding elsewhere in the report",
                    v.claim.entity
                ),
            };
        }
    }
}

/// Walk `decisions` and mark every claim whose supporting stage is
/// referenced by a `PostHocDeviation` record as `PostHoc`; other claims
/// stay `Prespecified`. Exploratory sessions skip this — the caller
/// should pass `is_confirmatory = false` and the strength stays
/// `Exploratory`.
///
/// The stage lookup is by substring: a claim's `claim.table` value of
/// the form `primary_endpoint_table.tsv` is considered to derive from
/// a stage named `primary_endpoint` if the deviation's `target_stage`
/// appears as a token in the table filename. This is intentionally
/// conservative — precise stage-lineage tracking is a future concern.
pub fn demote_claims_from_deviations(
    report: &mut ClaimVerificationReport,
    decisions: &[crate::decision_log::DecisionRecord],
    is_confirmatory: bool,
) {
    if !is_confirmatory {
        return;
    }
    let deviated_stages: Vec<&str> = decisions
        .iter()
        .filter_map(|d| match &d.decision {
            crate::decision_log::DecisionType::PostHocDeviation { target_stage, .. } => {
                Some(target_stage.as_str())
            }
            _ => None,
        })
        .collect();
    for verdict in &mut report.verdicts {
        let claim_table = verdict
            .claim
            .source_table
            .as_deref()
            .unwrap_or("")
            .to_lowercase();
        let excerpt = verdict.claim.excerpt.to_lowercase();
        let deviated = deviated_stages.iter().any(|s| {
            let needle = s.to_lowercase();
            claim_table.contains(&needle) || excerpt.contains(&needle)
        });
        verdict.strength = if deviated {
            ClaimStrength::PostHoc
        } else {
            ClaimStrength::Prespecified
        };
    }
}

/// Resolve the on-disk root for a task's outputs. The canonical layout
/// the harness writes is `<package>/runtime/outputs/<task_id>/`;
/// older packages (and any non-harness-emitted ones) keep their files
/// at `<package>/runtime/<task_id>/`. Try the canonical path first,
/// fall back to the legacy one. Returns `None` if neither exists.
pub(crate) fn resolve_task_runtime_dir(package_root: &Path, task_id: &str) -> Option<PathBuf> {
    let canonical = package_root.join("runtime").join("outputs").join(task_id);
    if canonical.is_dir() {
        return Some(canonical);
    }
    let legacy = package_root.join("runtime").join(task_id);
    if legacy.is_dir() {
        return Some(legacy);
    }
    None
}

/// Locate a narrative artifact (`.md`/`.txt`) in the task's runtime
/// directory (`runtime/outputs/<task_id>/`, falling back to
/// `runtime/<task_id>/` for legacy packages), preferring file names
/// containing `report`, then `interpretation`, then `summary`.
///
/// Returns `None` when the directory is missing or contains no narrative
/// candidates — the caller treats this as "nothing to verify" rather than
/// an error so the emit-time and GET-time entry points stay cheap.
fn find_narrative_artifact(package_root: &Path, task_id: &str) -> Option<PathBuf> {
    let runtime_dir = resolve_task_runtime_dir(package_root, task_id)?;
    let rd = std::fs::read_dir(&runtime_dir).ok()?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let ext_lower = ext.to_ascii_lowercase();
        if ext_lower == "md" || ext_lower == "txt" {
            candidates.push(path);
        }
    }
    candidates.sort_by_key(|p| {
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.contains("report") {
            0
        } else if name.contains("interpretation") {
            1
        } else if name.contains("summary") {
            2
        } else {
            3
        }
    });
    candidates.into_iter().next()
}

/// Load `interpretation-policy.json` from `config_dir`, trying
/// `config_dir/downstream-policy/` first (the repo `config/` layout) then a
/// flat `config_dir/` (an emitted package's own `policies/`) via
/// [`crate::claim_extractor::resolve_policy_file`]. The emit-time entry-point
/// reuses this when the package-side `policies/interpretation-policy.json` gate
/// is enabled — the extractor needs the full policy (entity name patterns,
/// direction vocab, tolerances).
fn load_interpretation_policy(config_dir: &Path) -> Option<serde_json::Value> {
    let path =
        crate::claim_extractor::resolve_policy_file(config_dir, "interpretation-policy.json")?;
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Emit-time, in-core mirror of the server's `verify_task_with_context`.
///
/// Runs the full claim-extractor → claim-verifier → post-hoc-demotion
/// pipeline for a single task and returns the resulting
/// `ClaimVerificationReport`.
///
/// NOTE: this has no in-tree caller. The former emit-time sidecar writer
/// (which persisted `runtime/verification-reports/<task_id>.json`) was
/// removed because the `GET /task/:task_id/result` handler always
/// recomputes verification from source — the package tree is rw-mounted
/// into the executing agent's container, so a pre-written sidecar could be
/// overwritten with an all-clean report and defeat the anti-hallucination
/// contract. This function is retained as public API for a future
/// HMAC-signed cache (see `server::chat_routes::tasks::result`).
///
/// Returns `None` when:
/// - the task has no narrative artifact under `runtime/<task_id>/`, or
/// - the configured `interpretation-policy.json` lacks a
///   `verifiableEntities` block.
///
/// Both cases are treated as "nothing to verify", matching the
/// behavior of the server-side wrapper.
pub fn verify_task_with_context_emit_time(
    package_root: &Path,
    task_id: &str,
    config_dir: &Path,
    project_class: crate::project_class::ProjectClass,
    decisions: &[crate::decision_log::DecisionRecord],
    is_confirmatory: bool,
) -> Option<ClaimVerificationReport> {
    let narrative_path = find_narrative_artifact(package_root, task_id)?;
    let policy = load_interpretation_policy(config_dir)?;
    let cfg = ExtractorConfig::from_policy_for_class(&policy, config_dir, project_class).ok()?;
    let narrative = std::fs::read_to_string(&narrative_path).ok()?;

    let tables_root = package_root.join("results").join("tables");
    let effective_root = if tables_root.is_dir() {
        tables_root
    } else {
        resolve_task_runtime_dir(package_root, task_id)
            .unwrap_or_else(|| package_root.join("runtime").join(task_id))
    };

    let claims = crate::claim_extractor::extract_claims(&narrative, &cfg);
    let mut report = verify_claims(&claims, &effective_root, &cfg);
    demote_claims_from_deviations(&mut report, decisions, is_confirmatory);

    for candidate in [
        package_root
            .join("runtime")
            .join(task_id)
            .join("runtime-decisions.jsonl"),
        package_root
            .join("runtime")
            .join("RUNTIME_DECISION_LOG.jsonl"),
    ] {
        if candidate.is_file() {
            if let Ok(rel) = candidate.strip_prefix(package_root) {
                report.runtime_decision_log_path = Some(rel.to_string_lossy().into_owned());
                break;
            }
        }
    }

    Some(report)
}

/// Dispatch verification to the sub-function that matches `claim.contract`.
///
/// Each contract class has a dedicated verifier that interprets the row
/// columns differently. `NumericTableLookup` preserves the pre-existing
/// implementation; the five new classes add targeted checks layered on top
/// of the common row-lookup path.
/// A "TOKEN ( TOKEN )" apposition — for the VF-13 symbol↔Ensembl pairing check.
/// Both tokens are alnum/dot/dash with NO internal whitespace, so a descriptive
/// parenthetical ("IL6 (interleukin-6 receptor)") is not matched; the in-code
/// namespace classification then rejects same-namespace and non-id pairs.
static ID_APPOSITION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"([A-Za-z0-9][A-Za-z0-9.\-]*)\s*\(\s*([A-Za-z0-9][A-Za-z0-9.\-]*)\s*\)")
        .expect("static regex")
});

/// Lowercase and strip a trailing Ensembl version suffix (`.12`) for comparison.
fn strip_id_version(id: &str) -> String {
    let lower = id.to_ascii_lowercase();
    match lower.split_once('.') {
        Some((stem, _)) => stem.to_string(),
        None => lower,
    }
}

/// VF-13 — detect a gene SYMBOL paired with the WRONG Ensembl id per an
/// INDEPENDENT symbol→ensembl `map`. Scans `text` for "SYMBOL (ENSG…)" or
/// "ENSG… (SYMBOL)" appositions and, for one whose symbol the map covers and
/// whose asserted Ensembl id (version-stripped) differs from the map's truth,
/// returns `(symbol, asserted_ensembl, detail)`. Abstains (None) when there is
/// no apposition, both tokens share a namespace, the symbol is absent from the
/// map (which may be incomplete), or the pairing is correct — so an honest
/// pairing and an uncovered gene are never flagged.
fn detect_wrong_id_pairing(
    text: &str,
    map: &BTreeMap<String, String>,
) -> Option<(String, String, String)> {
    for caps in ID_APPOSITION_RE.captures_iter(text) {
        let (Some(a), Some(b)) = (caps.get(1), caps.get(2)) else {
            continue;
        };
        let (a, b) = (a.as_str(), b.as_str());
        let (symbol, ensembl) = match (id_namespace(a) == "ensembl", id_namespace(b) == "ensembl") {
            (false, true) => (a, b), // SYMBOL (ENSG…)
            (true, false) => (b, a), // ENSG… (SYMBOL)
            _ => continue,           // both same namespace → not a cross pairing
        };
        if let Some(truth) = map.get(&symbol.to_ascii_lowercase()) {
            if strip_id_version(truth) != strip_id_version(ensembl) {
                return Some((
                    symbol.to_string(),
                    ensembl.to_string(),
                    format!(
                        "gene identity: narrative pairs {symbol} with {ensembl}, but the independent annotation maps {symbol} to {truth} (wrong-gene citation)"
                    ),
                ));
            }
        }
    }
    None
}

fn verify_for_contract(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    verify_for_contract_audited(claim, index, cfg, cache).0
}

/// As [`verify_for_contract`], but also returns the reconstructable
/// [`VerdictAudit`] tuple. The per-contract dispatch computes the base status,
/// then the CLASS-AGNOSTIC numeric gate ([`run_numeric_gate`]) cross-checks any
/// numeric quantity the claim text asserts against the cited/discovered table
/// row — so a literature-grounded (or any other) claim carrying a wrong number
/// can no longer be `Verified` on its non-numeric predicate alone (CV-1/CV-2).
fn verify_for_contract_audited(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> (ClaimStatus, Option<VerdictAudit>) {
    // VF-13 — wrong gene-identity pairing. Strictly inert unless an INDEPENDENT
    // symbol↔Ensembl reference map is configured (no shipped policy sets one).
    // When the narrative asserts a "SYMBOL (ENSG…)" apposition the map
    // contradicts, that is a wrong-gene citation (the CRISPLD2→wrong-ENSG
    // hallucination class); flag Mismatch on the SYMBOL-anchored claim only, so
    // the redundant Ensembl-entity claim is not double-counted.
    if let Some(map) = &cfg.gene_annotation_map {
        if let Some((symbol, _ensembl, detail)) = detect_wrong_id_pairing(&claim.excerpt, map) {
            if claim.entity.eq_ignore_ascii_case(&symbol) {
                let mut audit =
                    VerdictAudit::class_only(verdict_class_of(claim), entity_opt(claim));
                audit.rationale = Some(detail.clone());
                return (ClaimStatus::Mismatch { detail }, Some(audit));
            }
        }
    }
    let base = match claim.contract {
        ClaimContract::NumericTableLookup => verify_numeric_lookup(claim, index, cfg, cache),
        ClaimContract::ThresholdedDeOrEnrichment => verify_thresholded(claim, index, cfg, cache),
        ClaimContract::RankTopN => verify_rank_top_n(claim, index, cfg, cache),
        ClaimContract::GroupComparison => verify_group_comparison(claim, index, cfg, cache),
        ClaimContract::Categorical => verify_categorical(claim, index, cfg, cache),
        ClaimContract::TimeSeriesSummary => verify_time_series(claim, index, cfg, cache),
        ClaimContract::LiteratureGrounded => verify_literature_grounded(claim, index, cfg, cache),
        ClaimContract::ExtremeValue => verify_extreme_value(claim, index, cfg, cache),
        ClaimContract::KeyedTableCell => verify_keyed_cell(claim, index, cfg, cache),
        ClaimContract::QuantileOfColumn => verify_quantile(claim, index, cfg, cache),
    };
    run_numeric_gate(base, claim, index, cfg, cache)
}

/// `Some(entity)` when the claim has a non-empty entity, for the audit's
/// `entity_value` slot.
fn entity_opt(claim: &Claim) -> Option<String> {
    let e = claim.entity.trim();
    if e.is_empty() {
        None
    } else {
        Some(e.to_string())
    }
}

/// Coarse verification-method class for a claim, ignoring whether a numeric
/// comparison actually ran (the gate promotes a compared verdict to
/// [`VerdictClass::NumericTable`]). Total over every [`ClaimContract`], so the
/// persisted per-class counts always sum to `n_checked`.
pub fn verdict_class_of(claim: &Claim) -> VerdictClass {
    match claim.contract {
        ClaimContract::LiteratureGrounded => VerdictClass::LiteratureQuotation,
        ClaimContract::Categorical | ClaimContract::RankTopN | ClaimContract::ExtremeValue => {
            VerdictClass::EntityPresence
        }
        ClaimContract::NumericTableLookup
        | ClaimContract::ThresholdedDeOrEnrichment
        | ClaimContract::KeyedTableCell
        | ClaimContract::QuantileOfColumn
        | ClaimContract::GroupComparison
        | ClaimContract::TimeSeriesSummary => {
            let has_numeric = claim.effect_size.is_some()
                || claim.pvalue.is_some()
                || claim.linear_fold.is_some()
                || claim.keyed_value.is_some()
                || claim.aggregate_value.is_some();
            if !has_numeric && claim.direction.is_some() {
                VerdictClass::Directional
            } else {
                VerdictClass::NumericTable
            }
        }
    }
}

/// Contract classes whose dedicated verifier already performs a full numeric
/// cell comparison (`verify_one`/`verify_thresholded`/…). For these the gate
/// only *strengthens* — it can promote to `Mismatch` on an additional
/// disagreeing token but never downgrades a `Verified` to `Unverifiable`
/// (that would regress a legitimately-adjudicated numeric verdict). The
/// `Unverifiable`-on-unconfirmable-token rule applies only to the classes that
/// otherwise never check the number (`LiteratureGrounded`, `Categorical`,
/// `RankTopN`, `ExtremeValue`).
fn numeric_adjudicating_class(contract: ClaimContract) -> bool {
    matches!(
        contract,
        ClaimContract::NumericTableLookup
            | ClaimContract::ThresholdedDeOrEnrichment
            | ClaimContract::KeyedTableCell
            | ClaimContract::QuantileOfColumn
            | ClaimContract::GroupComparison
            | ClaimContract::TimeSeriesSummary
    )
}

/// Which table column family a numeric token in a claim asserts against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumericColumnKind {
    /// A log2 fold-change / effect-size token (absolute tolerance).
    Log2Fc,
    /// An ADJUSTED p-value token (`padj`/`fdr`/`q-value`/`adj p`).
    PvalueAdjusted,
    /// A RAW p-value token (`p`/`p-value`).
    PvalueRaw,
}

/// One numeric assertion parsed out of a claim's narrative text.
#[derive(Debug)]
struct NumericToken {
    kind: NumericColumnKind,
    /// Parsed value, `None` when the value string could not be parsed at all
    /// (malformed / overflow) — the token is then *unparseable* for coverage.
    claimed: Option<f64>,
    /// Byte span in the canonicalized excerpt. Used to bind a value to the
    /// nearest entity in a sentence that reports several entities.
    start: usize,
    end: usize,
}

/// Order-of-magnitude p-value tolerance (in decades): a claimed p and a table p
/// agree when their base-10 logs differ by no more than this. Deliberately a
/// GENUINE order-of-magnitude band, NOT the relative `pvalue_within_tolerance`,
/// so an honestly-rounded p (claim `~1e-45` vs table `7.5e-46`) is not a false
/// `Mismatch` while a value wrong by dozens of orders (the CV-1 padj) is caught.
const PVALUE_ORDERS_TOLERANCE: f64 = 1.0;

/// Reporting floor below which a claimed p-value of *exactly* 0.0 is read as a
/// display-rounded / below-reporting-precision value rather than a literal zero.
/// Narrative result tables routinely render a tiny positive adjusted p as
/// "padj 0.000" (fixed-decimal rounding), which parses to 0.0; the entity is in
/// fact highly significant, so such a claim must AGREE with an observed p that is
/// itself below this floor. A claimed 0.0 against a clearly non-significant
/// observed p (e.g. 0.3) is NOT display rounding and stays a Mismatch. Applies
/// to p-value columns only — no effect-size / non-p comparison is loosened.
const PVALUE_ZERO_DISPLAY_FLOOR: f64 = 1e-3;

/// Match a numeric-assertion keyword (`log2FC`/`padj`/`fdr`/`q-value`/`p-value`/
/// `p`), the SEPARATOR run (group 2), then a value token (group 3). Longest/
/// most-specific keywords come first so `padj` is preferred over bare `p` at the
/// same position (leftmost-first semantics). The value run is
/// `[+-]?<digit><digit/./e/E/+/-…>` so scientific forms (`7.5e-46`), the
/// collapsed base-10 form (`10-80`, i.e. `1e-80`) and a signed effect (`+2.61`)
/// are all captured; a malformed run (`1e`, `1e999`) is captured too and
/// rejected downstream so it counts as *unparseable*. The separator is captured
/// so a THRESHOLD assertion (`FDR < 0.05`, `log2FC > 1`) — where the value is a
/// cutoff, not an observed cell value — can be distinguished from a POINT
/// assertion (`padj = 1e-5`, `padj ≈ 1e-80`) and skipped by the point-value gate.
static NUMERIC_TOKEN_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(log2\s*fold\s*change|log\s*fold\s*change|log2\s*fc|logfc|padj|adj\.?\s*p(?:[\s._-]*value)?|fdr|q[\s._-]*value|p[\s._-]*value|p)\b([\s:=≈~<>≤≥]*)([+-]?[0-9][0-9.eE+^\-]*)",
    )
    .expect("static regex")
});

/// Scan a claim's narrative text for POINT numeric assertions, mapping each to
/// its table column family. The text is Unicode-canonicalized first
/// (superscripts / subscripts / minus / `×10ⁿ` → ASCII), so `log₂FC` and
/// `10⁻⁸⁰` are handled. A token whose separator carries an inequality
/// (`<`/`>`/`≤`/`≥`) is a THRESHOLD (a cutoff, adjudicated by the thresholded
/// verifier), not an observed-value assertion, and is skipped here.
fn scan_numeric_tokens(text: &str) -> Vec<NumericToken> {
    let canon = crate::claim_extractor::canonicalize_scientific(text);
    let mut out = Vec::new();
    for caps in NUMERIC_TOKEN_RE.captures_iter(&canon) {
        let (Some(kw), Some(sep), Some(val)) = (caps.get(1), caps.get(2), caps.get(3)) else {
            continue;
        };
        // Threshold assertion (cutoff), not a point value — skip.
        if sep.as_str().contains(['<', '>', '≤', '≥']) {
            continue;
        }
        let kw_l = kw.as_str().to_ascii_lowercase();
        let kind = if kw_l.contains("log") {
            NumericColumnKind::Log2Fc
        } else if is_adjusted_pvalue_keyword(&kw_l) {
            NumericColumnKind::PvalueAdjusted
        } else {
            NumericColumnKind::PvalueRaw
        };
        out.push(NumericToken {
            kind,
            claimed: parse_numeric_token(val.as_str()),
            start: caps.get(0).map_or(val.start(), |matched| matched.start()),
            end: caps.get(0).map_or(val.end(), |matched| matched.end()),
        });
    }
    out
}

/// Bind one numeric token to the nearest policy-recognized entity in the
/// canonicalized excerpt. Returns `None` when no entity is detectable or the
/// nearest-distance tie is ambiguous. A definite `false` marks a value owned by
/// another entity and lets the numeric gate exclude it from this claim's
/// coverage without suppressing contradictory values in a single-entity claim.
fn numeric_token_owned_by_claim(
    token: &NumericToken,
    canonical_excerpt: &str,
    claim: &Claim,
    cfg: &ExtractorConfig,
    table_entities: &BTreeSet<String>,
) -> Option<bool> {
    let mut spans: BTreeSet<(usize, usize, String)> = BTreeSet::new();
    let claim_namespace = id_namespace(&claim.entity);
    let claim_entity = normalize(&claim.entity);
    for pattern in &cfg.entity_patterns {
        for matched in pattern.find_iter(canonical_excerpt) {
            let entity = matched.as_str();
            let normalized = normalize(entity);
            if id_namespace(entity) != claim_namespace
                || (normalized != claim_entity && !table_entities.contains(&normalized))
                || cfg
                    .entity_exclude_patterns
                    .iter()
                    .any(|excluded| excluded.is_match(entity))
            {
                continue;
            }
            spans.insert((matched.start(), matched.end(), normalized));
        }
    }
    // The extractor may obtain an entity from a structured Markdown cell
    // rather than the policy's broad prose regex (for example,
    // HALLMARK_ADIPOGENESIS). Add the claim's literal occurrence explicitly,
    // allowing CommonMark's presentation-only backslash before punctuation.
    let mut literal_pattern = String::from("(?i)");
    for ch in claim.entity.chars() {
        if ch.is_ascii_punctuation() {
            literal_pattern.push_str(r"\\?");
        }
        literal_pattern.push_str(&regex::escape(&ch.to_string()));
    }
    let claim_spans: Vec<(usize, usize)> = regex::Regex::new(&literal_pattern)
        .ok()
        .map(|pattern| {
            pattern
                .find_iter(canonical_excerpt)
                .map(|matched| (matched.start(), matched.end()))
                .collect()
        })
        .unwrap_or_default();
    for (start, end) in &claim_spans {
        spans.insert((*start, *end, claim_entity.clone()));
    }
    // Drop broad-regex fragments contained in the exact claim occurrence,
    // fragments inside the measurement expression itself, and fragments
    // embedded in a larger identifier token. Broad symbol patterns can
    // otherwise misread the `FC` in either `log2FC=4.8` or a preceding bare
    // `log2FC` label as a gene and assign an adjacent value to that synthetic
    // entity.
    spans.retain(|(start, end, entity)| {
        let overlaps_measurement = *start < token.end && token.start < *end;
        let identifier_char = |value: char| value.is_alphanumeric() || value == '_';
        let embedded_fragment = canonical_excerpt[..*start]
            .chars()
            .next_back()
            .is_some_and(identifier_char)
            || canonical_excerpt[*end..]
                .chars()
                .next()
                .is_some_and(identifier_char);
        !overlaps_measurement
            && (entity == &claim_entity
                || (!embedded_fragment
                    && !claim_spans
                        .iter()
                        .any(|(claim_start, claim_end)| claim_start <= start && end <= claim_end)))
    });
    if spans.is_empty() {
        return None;
    }

    let distance = |start: usize, end: usize| {
        if end <= token.start {
            token.start - end
        } else if token.end <= start {
            start - token.end
        } else {
            0
        }
    };
    let min_distance = spans
        .iter()
        .map(|(start, end, _)| distance(*start, *end))
        .min()?;
    let nearest: BTreeSet<&str> = spans
        .iter()
        .filter(|(start, end, _)| distance(*start, *end) == min_distance)
        .map(|(_, _, entity)| entity.as_str())
        .collect();
    if nearest.len() != 1 {
        return None;
    }
    Some(nearest.contains(claim_entity.as_str()))
}

/// Parse a numeric value token, tolerating the collapsed base-10 exponent form
/// `10-80` (meaning `1e-80`) and `10^-80`. Returns `None` for a token that
/// cannot be resolved to a finite number (`1e`, `1e999`, `10-`) — the *cannot
/// be parsed at all* case that yields `Unverifiable`.
fn parse_numeric_token(raw: &str) -> Option<f64> {
    let s = raw.trim().trim_end_matches([',', ';', ')', '.']);
    if s.is_empty() {
        return None;
    }
    if let Ok(v) = s.parse::<f64>() {
        return if v.is_finite() { Some(v) } else { None };
    }
    for pref in ["10^-", "10-"] {
        if let Some(exp) = s.strip_prefix(pref) {
            if !exp.is_empty() && exp.bytes().all(|b| b.is_ascii_digit()) {
                return format!("1e-{exp}")
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite());
            }
        }
    }
    None
}

/// True when a claimed p and observed p agree within an order-of-magnitude
/// band. Exact equality (incl. both-zero) always agrees; a claimed exactly-zero
/// p is treated as display rounding and agrees when `observed` is below
/// `PVALUE_ZERO_DISPLAY_FLOOR`; other comparisons require both values positive.
fn pvalue_orders_within(claimed: f64, observed: f64, max_orders: f64) -> bool {
    if claimed == observed {
        return true;
    }
    // A claimed p of exactly 0.0 is a display-rounded value ("padj 0.000" in a
    // narrative table); it agrees iff the observed p is itself below the small
    // reporting floor (the entity IS that significant), and Mismatches only when
    // the observed p is NOT small. See `PVALUE_ZERO_DISPLAY_FLOOR`.
    if claimed == 0.0 {
        return observed > 0.0 && observed <= PVALUE_ZERO_DISPLAY_FLOOR;
    }
    if claimed <= 0.0 || observed <= 0.0 {
        return false;
    }
    (claimed.log10() - observed.log10()).abs() <= max_orders
}

/// A resolved table row for the gate: the table label, the entity column the
/// row was keyed by, and the row's (normalized-key) cell values.
struct ResolvedRow {
    table_label: String,
    entity_column: Option<String>,
    values: BTreeMap<String, String>,
}

/// Resolve the claim's entity to a row: prefer the cited `source_table`, else
/// scan every distinct table in the index for a row whose entity matches.
/// Returns the row's values (cloned) so the caller is free of the `&mut cache`
/// borrow.
fn resolve_entity_row(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> Option<ResolvedRow> {
    let needle = normalize(&claim.entity);
    if needle.is_empty() {
        return None;
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Some(src) = claim.source_table.as_deref() {
        if let Some(p) = index.resolve(src) {
            paths.push(p.to_path_buf());
        }
    }
    for p in index.distinct_paths() {
        if !paths.contains(&p) {
            paths.push(p);
        }
    }
    for path in paths {
        if !cache.contains_key(&path) {
            match load_table_rows(&path, &cfg.entity_columns) {
                Ok(t) => {
                    cache.insert(path.clone(), t);
                }
                Err(_) => continue,
            }
        }
        let Some(cached) = cache.get(&path) else {
            continue;
        };
        if let Some(row) = cached.get_by_normalized(&needle) {
            let entity_column = cfg
                .entity_columns
                .iter()
                .find(|c| {
                    row.values
                        .get(&normalize(c))
                        .is_some_and(|value| normalize(value) == needle)
                })
                .or_else(|| {
                    cfg.entity_columns
                        .iter()
                        .find(|c| row.values.contains_key(&normalize(c)))
                })
                .cloned();
            return Some(ResolvedRow {
                table_label: table_label(&path),
                entity_column,
                values: row.values.clone(),
            });
        }
    }
    None
}

/// The candidate `(column_name, value)` pairs a numeric token may agree with,
/// mirroring `verify_one`'s "match ANY present column of the family" leniency
/// so a table carrying both a raw `pvalue` and an adjusted `padj` does not
/// false-mismatch an honest adjusted-p claim.
fn observed_candidates(
    values: &BTreeMap<String, String>,
    kind: NumericColumnKind,
    cfg: &ExtractorConfig,
) -> Vec<(String, f64)> {
    let collect = |cols: &[String]| -> Vec<(String, f64)> {
        cols.iter()
            .filter_map(|c| {
                values
                    .get(&normalize(c))
                    .and_then(|raw| raw.parse::<f64>().ok())
                    .filter(|v| v.is_finite())
                    .map(|v| (c.clone(), v))
            })
            .collect()
    };
    match kind {
        NumericColumnKind::Log2Fc => collect(&cfg.effect_size_columns),
        NumericColumnKind::PvalueAdjusted => {
            let adj: Vec<String> = cfg
                .pvalue_columns
                .iter()
                .filter(|c| is_adjusted_pvalue_keyword(c))
                .cloned()
                .collect();
            let out = collect(&adj);
            if out.is_empty() {
                collect(&cfg.pvalue_columns)
            } else {
                out
            }
        }
        NumericColumnKind::PvalueRaw => {
            let raw: Vec<String> = cfg
                .pvalue_columns
                .iter()
                .filter(|c| !is_adjusted_pvalue_keyword(c))
                .cloned()
                .collect();
            let out = collect(&raw);
            if out.is_empty() {
                collect(&cfg.pvalue_columns)
            } else {
                out
            }
        }
    }
}

/// The class-agnostic numeric gate: cross-checks every numeric token in the
/// claim text against the resolved table row. A parseable value disagreeing
/// beyond tolerance ⇒ `Mismatch`; a value that cannot be parsed, or an entity
/// that cannot be resolved to a row / column, is *uncompared* and (for classes
/// that do not otherwise adjudicate the number) demotes a would-be `Verified`
/// to `Unverifiable`. `Verified` requires the base predicate AND every numeric
/// token compared and agreeing.
fn run_numeric_gate(
    base: ClaimStatus,
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> (ClaimStatus, Option<VerdictAudit>) {
    let base_is_verified = matches!(base, ClaimStatus::Verified);
    let base_is_mismatch = matches!(base, ClaimStatus::Mismatch { .. });

    let canonical_excerpt = crate::claim_extractor::canonicalize_scientific(&claim.excerpt);
    let tokens = scan_numeric_tokens(&claim.excerpt);
    if tokens.is_empty() {
        // No numeric assertion in the narrative text → the gate is inert; record
        // a class-only audit. When the base verifier already rejected the claim,
        // carry its detail as the audit rationale so a Mismatch never surfaces
        // with an empty audit (status/rationale coherence — mirrors the
        // base-Mismatch realignment in the keep-base-verdict branch below).
        let mut audit = VerdictAudit::class_only(verdict_class_of(claim), entity_opt(claim));
        if let ClaimStatus::Mismatch { detail } = &base {
            audit.rationale = Some(detail.clone());
        }
        return (base, Some(audit));
    }

    let resolved = resolve_entity_row(claim, index, cfg, cache);
    let mut table_entities = BTreeSet::new();
    for table in cache.values() {
        for row in &table.rows {
            for column in &cfg.entity_columns {
                if let Some(value) = row.values.get(&normalize(column)) {
                    let value = normalize(value);
                    if !value.is_empty() {
                        table_entities.insert(value);
                    }
                }
            }
        }
    }
    table_entities.insert(normalize(&claim.entity));
    let n_tokens = tokens.len();
    let mut n_compared = 0usize;
    let mut any_unparseable = false;
    let mut mismatch: Option<VerdictAudit> = None;
    let mut pass: Option<VerdictAudit> = None;
    let mut uncompared: Option<VerdictAudit> = None;

    let mk_numeric_audit = |claimed: Option<f64>,
                            observed: Option<f64>,
                            column: Option<String>,
                            kind: NumericColumnKind,
                            agrees: Option<bool>|
     -> VerdictAudit {
        let (op, abs_tol, rel_tol, unit) = match kind {
            NumericColumnKind::Log2Fc => {
                ("absolute", Some(cfg.log2fc_tolerance), None, None::<String>)
            }
            _ => (
                "order_of_magnitude",
                None,
                Some(PVALUE_ORDERS_TOLERANCE),
                None,
            ),
        };
        let rationale = match (agrees, observed) {
            (Some(false), Some(obs)) => Some(format!(
                "numeric: claim asserts {} = {} but the table cell is {} (compared by {})",
                column.clone().unwrap_or_else(|| "value".into()),
                claimed.map(fmt_num).unwrap_or_else(|| "?".into()),
                fmt_num(obs),
                op,
            )),
            (Some(true), Some(obs)) => Some(format!(
                "numeric: claim {} = {} agrees with table {} (within {})",
                column.clone().unwrap_or_else(|| "value".into()),
                claimed.map(fmt_num).unwrap_or_else(|| "?".into()),
                fmt_num(obs),
                op,
            )),
            _ => None,
        };
        VerdictAudit {
            class: VerdictClass::NumericTable,
            source_table: resolved.as_ref().map(|r| r.table_label.clone()),
            entity_column: resolved.as_ref().and_then(|r| r.entity_column.clone()),
            entity_value: entity_opt(claim),
            measurement_column: column,
            claimed_value: claimed,
            observed_value: observed,
            comparison_operator: Some(op.to_string()),
            absolute_tolerance: abs_tol,
            relative_tolerance: rel_tol,
            unit_conversion: unit,
            verifier_version: CLAIM_VERIFIER_VERSION.to_string(),
            rationale,
            parse_coverage: 1.0,
        }
    };

    // Bind each point value to its nearest entity before comparison. This
    // removes another entity's value from a shared sentence without using the
    // unsafe shortcut "one value agreed, therefore every disagreement in the
    // same numeric family is ambient", which could verify a contradictory
    // second value in a single-entity claim.
    let mut n_ambient = 0usize;
    for tok in &tokens {
        if numeric_token_owned_by_claim(tok, &canonical_excerpt, claim, cfg, &table_entities)
            == Some(false)
        {
            n_ambient += 1;
            continue;
        }
        let Some(claimed) = tok.claimed else {
            any_unparseable = true;
            uncompared.get_or_insert_with(|| {
                let mut a = mk_numeric_audit(None, None, None, tok.kind, None);
                a.rationale =
                    Some("numeric: a value in the narrative could not be parsed".to_string());
                a
            });
            continue;
        };
        let Some(row) = resolved.as_ref() else {
            uncompared.get_or_insert_with(|| {
                let mut a = mk_numeric_audit(Some(claimed), None, None, tok.kind, None);
                a.rationale = Some(format!(
                    "numeric: entity `{}` could not be resolved to a table row",
                    claim.entity
                ));
                a
            });
            continue;
        };
        let candidates = observed_candidates(&row.values, tok.kind, cfg);
        if candidates.is_empty() {
            uncompared.get_or_insert_with(|| {
                let mut a = mk_numeric_audit(Some(claimed), None, None, tok.kind, None);
                a.rationale =
                    Some("numeric: table row has no matching measurement column".to_string());
                a
            });
            continue;
        }
        // Compare against every candidate; agree if ANY is within tolerance.
        // Report the CLOSEST candidate as the observed value.
        let within = |obs: f64| match tok.kind {
            NumericColumnKind::Log2Fc => (claimed - obs).abs() <= cfg.log2fc_tolerance,
            _ => pvalue_orders_within(claimed, obs, PVALUE_ORDERS_TOLERANCE),
        };
        let dist = |obs: f64| match tok.kind {
            NumericColumnKind::Log2Fc => (claimed - obs).abs(),
            _ => {
                if claimed > 0.0 && obs > 0.0 {
                    (claimed.log10() - obs.log10()).abs()
                } else {
                    (claimed - obs).abs()
                }
            }
        };
        let agrees = candidates.iter().any(|(_, v)| within(*v));
        let (col, obs) = candidates
            .iter()
            .min_by(|a, b| dist(a.1).total_cmp(&dist(b.1)))
            .cloned()
            .expect("candidates non-empty");
        let audit = mk_numeric_audit(Some(claimed), Some(obs), Some(col), tok.kind, Some(agrees));
        if agrees {
            n_compared += 1;
            pass.get_or_insert(audit);
        } else {
            n_compared += 1;
            mismatch.get_or_insert(audit);
        }
    }

    let relevant_tokens = n_tokens.saturating_sub(n_ambient);
    let parse_coverage = if relevant_tokens == 0 {
        1.0
    } else {
        n_compared as f64 / relevant_tokens as f64
    };

    // A genuine numeric disagreement is a Mismatch regardless of base status.
    if let Some(mut a) = mismatch {
        a.parse_coverage = parse_coverage;
        let status = if base_is_mismatch {
            base // preserve the base verifier's (more specific) detail
        } else {
            ClaimStatus::Mismatch {
                detail: a
                    .rationale
                    .clone()
                    .unwrap_or_else(|| "numeric value disagrees with the cited table".into()),
            }
        };
        return (status, Some(a));
    }

    // No disagreement. Demote a would-be Verified to Unverifiable when a numeric
    // assertion was present but not fully confirmable — but only for classes
    // that do not otherwise adjudicate the number (avoids regressing a
    // legitimately-verified numeric-class verdict).
    let fully_confirmed = n_compared == relevant_tokens && !any_unparseable;
    if base_is_verified && !fully_confirmed && !numeric_adjudicating_class(claim.contract) {
        let reason = "numeric assertion in the narrative could not be confirmed against the cited table (unparseable value or unresolved entity/column)".to_string();
        let mut a = uncompared.or(pass).unwrap_or_else(|| {
            VerdictAudit::class_only(verdict_class_of(claim), entity_opt(claim))
        });
        a.parse_coverage = parse_coverage;
        a.rationale = Some(reason.clone());
        return (ClaimStatus::Unverifiable { reason }, Some(a));
    }

    // Keep the base verdict; attach the most informative audit we captured.
    let mut audit = pass
        .or(uncompared)
        .unwrap_or_else(|| VerdictAudit::class_only(verdict_class_of(claim), entity_opt(claim)));
    audit.parse_coverage = parse_coverage;
    // A recorded Mismatch must never carry an "agrees" numeric rationale. When
    // the base verifier rejected the claim on non-numeric grounds (a hedged
    // rank/extreme claim whose cited number happens to be correct while the RANK
    // assertion fails), the audit we attached is the AGREEING numeric pass — its
    // "agrees" text contradicts the Mismatch status. Realign the rationale with
    // the base Mismatch detail so status and rationale stay coherent (mirrors the
    // disagreement branch above).
    if let ClaimStatus::Mismatch { detail } = &base {
        audit.rationale = Some(detail.clone());
    }
    (base, Some(audit))
}

/// Compact numeric formatter for audit rationales: scientific for very small /
/// large magnitudes, plain otherwise.
fn fmt_num(v: f64) -> String {
    if v != 0.0 && (v.abs() < 1e-3 || v.abs() >= 1e6) {
        format!("{v:.4e}")
    } else {
        format!("{v:.4}")
    }
}

/// Verify a direct numeric table-cell lookup claim.
/// This is the original implementation used before per-contract dispatch.
fn verify_numeric_lookup(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    verify_one(claim, index, cfg, cache)
}

/// Ensure `path`'s rows are cached, returning a reference to the loaded table or
/// `None` when the file has no configured entity column / is unreadable. Shared
/// by the Phase-C keyed-cell and quantile verifiers, which scan whole tables
/// rather than a single by-entity row.
fn ensure_cached<'c>(
    cache: &'c mut BTreeMap<PathBuf, CachedTable>,
    path: &Path,
    cfg: &ExtractorConfig,
) -> Option<&'c CachedTable> {
    if !cache.contains_key(path) {
        match load_table_rows(path, &cfg.entity_columns) {
            Ok(t) => {
                cache.insert(path.to_path_buf(), t);
            }
            Err(_) => return None,
        }
    }
    cache.get(path)
}

/// The set of tables a Phase-C aggregate / keyed-cell claim may live in: the
/// cited table when it resolves, else EVERY distinct table in the index (these
/// claims rarely carry a "Table S1" cite — the statistic is a whole-column /
/// composite-key derivation, not a single addressed cell). Deduped by path.
fn candidate_tables_for_derived(claim: &Claim, index: &TableIndex) -> Vec<PathBuf> {
    if let Some(src) = claim.source_table.as_deref() {
        if let Some(p) = index.resolve(src) {
            return vec![p.to_path_buf()];
        }
    }
    index.distinct_paths()
}

/// Read the value of one of a row's columns, trying each candidate header alias
/// (already-normalized lookup) and returning the first that parses as a finite
/// f64.
fn row_value_for_aliases(row: &TableRow, aliases: &[&str]) -> Option<f64> {
    for a in aliases {
        if let Some(raw) = row.values.get(&normalize(a)) {
            if let Ok(v) = raw.parse::<f64>() {
                if v.is_finite() {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// Header aliases the keyed statistic column maps onto in a real enrichment
/// table. `nes` and the adjusted-p family each list the common spellings.
fn keyed_column_aliases(keyed_column: &str) -> &'static [&'static str] {
    if keyed_column.eq_ignore_ascii_case("nes") {
        &[
            "nes",
            "normalized_enrichment_score",
            "normalised_enrichment_score",
        ]
    } else {
        &[
            "adj_p_value",
            "adj_pvalue",
            "padj",
            "p_adj",
            "adjusted_p_value",
            "qvalue",
            "q_value",
            "fdr",
        ]
    }
}

/// Verify a composite-key enrichment cell ("KEGG Autophagy GSEA padj 2.98e-04").
///
/// A LINEAR SCAN over the cited table's rows matching BOTH the collection key
/// (e.g. `KEGG`) AND the term key (e.g. `Autophagy`) in ANY of the row's cells —
/// deliberately NOT a `by_entity` lookup, which collapses every "Autophagy" row
/// (KEGG vs Reactome) to the first one and so cannot tell the two apart. The
/// named statistic column (`NES` / `adj_p_value`) of the matched row is then
/// compared within tolerance: `pvalue_relative_tolerance` for an adjusted-p
/// column, `log2fc_tolerance` for NES. Abstains (`Unverifiable`) when no table
/// carries the keyed row or the named statistic column is absent.
fn verify_keyed_cell(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    let (Some(collection), Some(term), Some(keyed_column), Some(claimed)) = (
        claim.collection.as_deref(),
        claim.term.as_deref(),
        claim.keyed_column.as_deref(),
        claim.keyed_value,
    ) else {
        return ClaimStatus::Unverifiable {
            reason: "keyed-cell claim is missing a collection/term/column/value slot".into(),
        };
    };
    let collection_norm = normalize(collection);
    let term_norm = normalize(term);
    let aliases = keyed_column_aliases(keyed_column);
    let is_pvalue_column = !keyed_column.eq_ignore_ascii_case("nes");

    let mut saw_keyed_row = false;
    let mut saw_row_without_column = false;
    for path in candidate_tables_for_derived(claim, index) {
        let Some(cached) = ensure_cached(cache, &path, cfg) else {
            continue;
        };
        // LINEAR SCAN: a row matches when one cell equals the collection AND
        // another equals the term. Never `by_entity` — that collapses the
        // duplicated "Autophagy" rows to one.
        for row in &cached.rows {
            let has_collection = row.values.values().any(|v| normalize(v) == collection_norm);
            let has_term = row.values.values().any(|v| normalize(v) == term_norm);
            if !(has_collection && has_term) {
                continue;
            }
            saw_keyed_row = true;
            let Some(observed) = row_value_for_aliases(row, aliases) else {
                saw_row_without_column = true;
                continue;
            };
            let agrees = if is_pvalue_column {
                pvalue_within_tolerance(claimed, observed, cfg.pvalue_relative_tolerance)
            } else {
                (observed - claimed).abs() <= cfg.log2fc_tolerance
            };
            if agrees {
                return ClaimStatus::Verified;
            }
            return ClaimStatus::Mismatch {
                detail: if is_pvalue_column {
                    format!(
                        "{collection} {term} {keyed_column}: narrative {claimed:.4e} vs table {observed:.4e} (relative tolerance {}%)",
                        (cfg.pvalue_relative_tolerance * 100.0) as u32
                    )
                } else {
                    format!(
                        "{collection} {term} {keyed_column}: narrative {claimed:.4} vs table {observed:.4} (tolerance ±{:.4})",
                        cfg.log2fc_tolerance
                    )
                },
            };
        }
    }

    ClaimStatus::Unverifiable {
        reason: if saw_keyed_row && saw_row_without_column {
            format!(
                "keyed row `{collection} / {term}` found but cited statistic column `{keyed_column}` absent"
            )
        } else {
            format!("no table carries an enrichment row keyed on `{collection} / {term}`")
        },
    }
}

/// Verify a quantile-of-column claim ("median baseMean of tested genes = 263.14").
///
/// RECOMPUTES the named statistic (median/mean) from the cited table's column
/// over the CORRECT row set — for "tested genes" only rows whose adjusted
/// p-value is present (non-NA), else every row — and compares it within the
/// p-value relative tolerance (a multiplicative band suited to magnitudes that
/// span orders). Abstains (`Unverifiable`) when no table carries the named
/// column. This is the recall-closing twin to the median-baseMean mislabel: the
/// all-rows median (100.94) and the tested-genes median (263.14) differ, so the
/// row set the claim names is load-bearing.
fn verify_quantile(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    let (Some(kind), Some(column), Some(rowset), Some(claimed)) = (
        claim.aggregate_kind,
        claim.aggregate_column.as_deref(),
        claim.aggregate_rowset,
        claim.aggregate_value,
    ) else {
        return ClaimStatus::Unverifiable {
            reason: "quantile claim is missing a kind/column/rowset/value slot".into(),
        };
    };
    let column_norm = normalize(column);

    let mut saw_column = false;
    for path in candidate_tables_for_derived(claim, index) {
        let Some(cached) = ensure_cached(cache, &path, cfg) else {
            continue;
        };
        // Does this table carry the named column at all?
        if !cached
            .rows
            .iter()
            .any(|r| r.values.contains_key(&column_norm))
        {
            continue;
        }
        saw_column = true;

        let mut sample: Vec<f64> = Vec::new();
        for row in &cached.rows {
            // Row-set filter: "tested genes" keeps only rows with a non-NA
            // adjusted p-value, the DE convention for genes that survived
            // independent filtering.
            if rowset == crate::claim_extractor::QuantileRowSet::TestedGenes
                && !row_has_non_na_adjusted_pvalue(row, cfg)
            {
                continue;
            }
            if let Some(raw) = row.values.get(&column_norm) {
                if let Ok(v) = raw.parse::<f64>() {
                    if v.is_finite() {
                        sample.push(v);
                    }
                }
            }
        }
        if sample.is_empty() {
            continue;
        }
        let observed = match kind {
            crate::claim_extractor::QuantileKind::Median => median(&mut sample),
            crate::claim_extractor::QuantileKind::Mean => {
                sample.iter().sum::<f64>() / sample.len() as f64
            }
        };
        if pvalue_within_tolerance(claimed, observed, cfg.pvalue_relative_tolerance) {
            return ClaimStatus::Verified;
        }
        return ClaimStatus::Mismatch {
            detail: format!(
                "{kind:?} of `{column}` over {rowset:?}: narrative {claimed:.4} vs recomputed {observed:.4} (relative tolerance {}%)",
                (cfg.pvalue_relative_tolerance * 100.0) as u32
            ),
        };
    }

    ClaimStatus::Unverifiable {
        reason: if saw_column {
            format!("column `{column}` present but no rows survived the claimed row set")
        } else {
            format!("no cited table carries a `{column}` column to take the quantile of")
        },
    }
}

/// True when `row` has a parseable, finite adjusted p-value in any configured
/// ADJUSTED p-value column — the "tested genes" membership test (a non-NA padj
/// marks a gene that survived independent filtering).
fn row_has_non_na_adjusted_pvalue(row: &TableRow, cfg: &ExtractorConfig) -> bool {
    cfg.pvalue_columns
        .iter()
        .filter(|c| is_adjusted_pvalue_keyword(c))
        .any(|c| {
            row.values
                .get(&normalize(c))
                .and_then(|raw| raw.parse::<f64>().ok())
                .is_some_and(f64::is_finite)
        })
}

/// Median of `sample` (sorted in place). Empty input is the caller's
/// responsibility (it is filtered out before this is called).
fn median(sample: &mut [f64]) -> f64 {
    sample.sort_by(f64::total_cmp);
    let n = sample.len();
    if n % 2 == 1 {
        sample[n / 2]
    } else {
        (sample[n / 2 - 1] + sample[n / 2]) / 2.0
    }
}

/// Non-significance assertion cues for a thresholded claim: prose stating the
/// entity did NOT pass significance, or a p-value-family comparator pointing the
/// wrong way (`padj > 0.05`). When present, a bare-significance threshold check
/// must verify the OPPOSITE polarity (observed p >= threshold confirms the
/// non-significance claim) rather than flag it. Kept narrow so an honest
/// significance assertion ("padj < 0.05", "significant") never matches.
static NONSIGNIFICANCE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(not\s+(?:statistically\s+)?significant|non[\s-]?significant|did\s+not\s+reach\s+significance|failed\s+to\s+reach\s+significance|not\s+reach\s+significance|(?:padj|fdr|q[\s-]?value|adjusted\s*p|p[\s-]?value|\bp|\bq)\s*[>≥])",
    )
    .expect("static regex")
});

/// Verify a thresholded DE or enrichment claim.
///
/// In addition to the base numeric checks, confirms that the observed
/// p-value in the table falls below the threshold implied by the claim.
/// When no explicit threshold is present in the claim, falls back to the
/// standard numeric check.
fn verify_thresholded(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    // Run the base numeric check first — it already validates effect size,
    // direction, and p-value relative tolerance.
    let base = verify_one(claim, index, cfg, cache);

    // If the base check already failed or the claim carries a pvalue, the
    // existing comparison was sufficient. For a thresholded claim whose
    // pvalue slot was not parsed (the narrative only said "FDR < 0.05"
    // without quoting a specific number), add an extra check that the
    // observed p-value is indeed < 0.05 — the canonical DE reporting threshold.
    if matches!(
        base,
        ClaimStatus::Mismatch { .. } | ClaimStatus::Unverifiable { .. }
    ) {
        return base;
    }
    if claim.pvalue.is_none() {
        // Reuse the cache populated by `verify_one` above so the
        // post-success threshold check is a hashmap probe, not a
        // second `File::open`.
        if let Some(source_ref) = claim.source_table.as_deref() {
            if let Ok((_path, cached)) = cached_table_for(cache, index, source_ref, cfg) {
                if let Some(row) = cached.get_by_normalized(&normalize(&claim.entity)) {
                    // VF-7 — judge a bare-significance claim on the p-value
                    // column CLASS the claim NAMES. A claim naming an adjusted
                    // threshold ("significant at FDR/padj < 0.05") is judged on
                    // the adjusted column: a gene with raw p=0.042 but padj=0.16
                    // is NOT FDR-significant, and the old raw-first probe
                    // silently passed that overclaim. A claim naming a RAW
                    // threshold ("raw p < 0.05", "nominal p < 0.05") is judged
                    // on the raw column, so a correct nominal-significance
                    // statement is NOT falsely flagged against the stricter
                    // adjusted column. A bare "significant" with no explicit
                    // keyword defaults to the adjusted column (the DE reporting
                    // convention). The other class is consulted only as a
                    // fallback when the named class is absent from the table.
                    let want_adjusted = THRESH_RE
                        .captures(&claim.excerpt)
                        .and_then(|c| c.get(1))
                        .map(|m| is_adjusted_pvalue_keyword(m.as_str()))
                        .unwrap_or(true);
                    let adjusted: Vec<String> = cfg
                        .pvalue_columns
                        .iter()
                        .filter(|c| is_adjusted_pvalue_keyword(c))
                        .cloned()
                        .collect();
                    let raw: Vec<String> = cfg
                        .pvalue_columns
                        .iter()
                        .filter(|c| !is_adjusted_pvalue_keyword(c))
                        .cloned()
                        .collect();
                    let (primary, fallback) = if want_adjusted {
                        (&adjusted, &raw)
                    } else {
                        (&raw, &adjusted)
                    };
                    let obs_p = lookup_numeric(&row.values, primary)
                        .or_else(|| lookup_numeric(&row.values, fallback));
                    if let Some(obs_p) = obs_p {
                        if NONSIGNIFICANCE_RE.is_match(&claim.excerpt) {
                            // The narrative asserts the entity is NOT significant
                            // ("not significant", "padj > 0.05"). This is confirmed
                            // when the observed p is indeed >= threshold; it is a
                            // Mismatch ONLY if the gene is actually significant
                            // (p < threshold) despite the non-significance claim.
                            // Without this the bare-significance branch flipped the
                            // polarity and flagged an honest "CXCL1 … padj = 0.16 —
                            // not significant" as a fabrication.
                            if obs_p < 0.05 {
                                return ClaimStatus::Mismatch {
                                    detail: format!(
                                        "thresholded claim: narrative reports non-significance but observed {} p-value {:.4e} is significant (< 0.05)",
                                        if want_adjusted { "adjusted" } else { "raw" },
                                        obs_p
                                    ),
                                };
                            }
                        } else if obs_p >= 0.05 {
                            return ClaimStatus::Mismatch {
                                detail: format!(
                                    "thresholded claim: observed {} p-value {:.4e} does not meet the claimed significance (< 0.05)",
                                    if want_adjusted { "adjusted" } else { "raw" },
                                    obs_p
                                ),
                            };
                        }
                    }
                }
            }
        }
    }
    base
}

/// Verify a rank / top-N membership claim.
///
/// Checks whether the entity appears in the top-N rows of the source table,
/// recomputed here rather than trusting the table's physical row order (which
/// may be sorted by p-value, gene name, or anything else). The ranking axis
/// follows the superlative: a SIGNIFICANCE superlative ("one of the most
/// significant", "among the top by padj") ranks by p-value (argmin — smallest p
/// is the best rank), every other superlative ("one of the largest
/// fold-changes") ranks by absolute effect size descending. Without this a
/// hedged "one of the most significantly upregulated genes" was ranked by
/// |log2FC| and false-Mismatched the genuinely most-significant gene when it sat
/// outside the top-N by fold-change magnitude.
///
/// Two flavours of "top-N" claim are distinguished by whether the excerpt names
/// an explicit number:
/// * EXPLICIT ("in the top 5", "one of the top 20") → that exact N is used.
/// * SOFT (vague superlative, no number: "one of the top DE genes", "among the
///   strongest") → a GENEROUS threshold that scales with table size,
///   `max(DEFAULT_SOFT_FLOOR, ceil(SOFT_TOP_PERCENTILE × n_ranked_rows))`. A
///   strict N=10 over-flags a correct vague claim on a large table: in the
///   Himes package CRISPLD2 ranks 31 of 22,369 tested genes (the top 0.14%), so
///   "one of the top DE genes" is accurate, yet a top-10 cutoff called it a
///   Mismatch. The percentile floor (224 rows there) verifies it while a
///   genuinely low-ranked gene called "one of the top" still Mismatches.
fn verify_rank_top_n(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    if ANAPHORIC_RANK_SCOPE_RE.is_match(&claim.excerpt) {
        return ClaimStatus::Unverifiable {
            reason: "rank scope refers to a displayed subset that is not encoded in the claim; global ranking was not substituted".into(),
        };
    }
    let Some(source_ref) = claim.source_table.as_deref() else {
        return ClaimStatus::Unverifiable {
            reason: "no source table cited — cannot check rank membership".into(),
        };
    };
    // VF-11: unresolved cited table → sibling-membership discovery fallback.
    if index.resolve(source_ref).is_none() {
        return verify_via_sibling_discovery(claim, index, cfg, cache);
    }
    let (path, cached) = match cached_table_for(cache, index, source_ref, cfg) {
        Ok(t) => t,
        Err(status) => return status,
    };

    // Parse an explicit N from the excerpt ("top-10", "top 5", etc.). `Some(n)`
    // marks an EXPLICIT "top N" claim (the narrative named the number); `None`
    // marks a SOFT claim (vague "one of the top" with no number). The two get
    // different thresholds below — explicit uses N verbatim, soft scales with
    // table size — so the presence of a captured digit IS the soft/explicit
    // marker, recovered losslessly from the excerpt the claim already carries.
    let explicit_n: Option<usize> = RANK_TOP_N_RE
        .captures(&claim.excerpt.to_lowercase())
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse().ok());

    // Locate the claimed entity's row. If it isn't in the table at all, the
    // top-N question is unverifiable rather than a fabrication mismatch.
    let Some(claimed_row) = cached.get_by_normalized(&normalize(&claim.entity)) else {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "entity `{}` not found in table `{}` — cannot check rank membership",
                claim.entity,
                table_label(&path)
            ),
        };
    };

    // Significance-ranked vs magnitude-ranked membership. Mirror
    // `verify_extreme_value`'s `over_pvalue`: an explicit significance
    // superlative ("one of the most significant", "among the top by padj") — or
    // a bare p-value-column token with no competing signed-effect direction cue
    // — ranks membership by p-value (argmin, smallest p = best rank) rather than
    // by |effect size|. A HEDGED "one of the most significantly upregulated
    // genes" thus tests SIGNIFICANCE rank, so the most-significant gene verifies
    // even when it sits outside the top-N by fold-change magnitude. Every other
    // superlative ("one of the largest fold-changes") keeps the |effect size|
    // ranking.
    let excerpt = &claim.excerpt;
    let has_effect_direction_cue =
        EXTREME_DOWN_RE.is_match(excerpt) || EXTREME_UP_RE.is_match(excerpt);
    let over_pvalue = EXTREME_PVAL_EXPLICIT_RE.is_match(excerpt)
        || (EXTREME_PVAL_COL_RE.is_match(excerpt) && !has_effect_direction_cue);
    let rank_columns: &[String] = if over_pvalue {
        &cfg.pvalue_columns
    } else {
        &cfg.effect_size_columns
    };

    // The claimed entity must itself carry a finite value in the ranking column
    // (effect size, or p-value when significance-ranked); without one it cannot
    // be ranked, so its top-N membership is unverifiable.
    if lookup_numeric(&claimed_row.values, rank_columns)
        .filter(|v| v.is_finite())
        .is_none()
    {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "entity `{}` has no numeric {} in `{}` — cannot rank",
                claim.entity,
                if over_pvalue {
                    "significance value"
                } else {
                    "effect size"
                },
                table_label(&path)
            ),
        };
    }

    // VF-6 — a ranked claim that ALSO asserts a direction ("the top-3 most
    // UP-regulated genes include X") must have X's own sign agree: a
    // large-NEGATIVE gene can legitimately sit in the top-N by |effect size|
    // while flatly contradicting an "upregulated" claim; that signed-vs-magnitude
    // confusion is a fabrication, not a pass. Direction has no meaning on a
    // p-value column, so this always reads the SIGNED effect column and is simply
    // skipped when the entity carries no effect size (a significance-ranked claim
    // can still verify its membership). Only fires when the sign positively
    // contradicts (obs nonzero, opposite sign), so a faithful "top-N upregulated"
    // naming a positive gene still Verifies.
    if let Some(direction) = claim.direction {
        if let Some((claimed_eff_col, claimed_eff)) =
            matched_numeric_column(&claimed_row.values, &cfg.effect_size_columns)
        {
            // VF-19: derive the sign on the matched column's scale (ratio columns
            // pivot at 1.0) so a rank claim on an HR/OR table is not misread.
            let observed_direction =
                observed_effect_direction(claimed_eff, effect_column_scale(claimed_eff_col));
            if observed_direction.is_some() && observed_direction != Some(direction) {
                return ClaimStatus::Mismatch {
                    detail: format!(
                        "rank claim direction: narrative says {:?} but `{}` has effect size {:+.4} in `{}`",
                        direction,
                        claim.entity,
                        claimed_eff,
                        table_label(&path)
                    ),
                };
            }
        }
    }

    // Rank the table rows and take the best N. The sort below is descending by
    // key, so a magnitude ranking uses |effect size| (largest first) and a
    // significance ranking uses the NEGATED p-value (smallest p = strongest
    // significance sorts first). Rows lacking a finite value in the ranking
    // column are dropped (NaN/±inf from "NA"/blank cells cannot be ranked and
    // would poison the sort comparator); the entity-name tie-break keeps the
    // ordering stable + deterministic.
    let mut ranked: Vec<(&str, f64)> = cached
        .rows
        .iter()
        .filter_map(|r| {
            lookup_numeric(&r.values, rank_columns)
                .filter(|v| v.is_finite())
                .map(|v| (r.entity.as_str(), if over_pvalue { -v } else { v.abs() }))
        })
        .collect();

    // If no row in the table carries a rankable value, there is nothing to rank.
    if ranked.is_empty() {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "table `{}` has no configured {} column — cannot rank",
                table_label(&path),
                if over_pvalue {
                    "p-value"
                } else {
                    "effect-size"
                },
            ),
        };
    }

    // Resolve the membership cutoff. An EXPLICIT "top N" claim uses that exact N
    // verbatim — the narrative committed to a number, so we hold it to it. A
    // SOFT claim (no number) uses a generous threshold that scales with the
    // ranked-row count: `max(DEFAULT_SOFT_FLOOR, ceil(SOFT_TOP_PERCENTILE × n))`.
    // `n_ranked_rows` is exactly the set being ranked here (rows with a usable
    // numeric value in the ranking column), so the percentile tracks the real
    // table size.
    let n_ranked_rows = ranked.len();
    let n = match explicit_n {
        Some(explicit) => explicit,
        None => {
            let percentile_rows = (SOFT_TOP_PERCENTILE * n_ranked_rows as f64).ceil() as usize;
            DEFAULT_SOFT_FLOOR.max(percentile_rows)
        }
    };

    // total_cmp is a genuine total order over all f64 (NaN included), so the
    // sort never panics even if a non-finite value slips through; the entity
    // tie-break keeps the ordering stable + deterministic.
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    ranked.truncate(n);

    let in_top_n = ranked
        .iter()
        .any(|(e, _)| e.eq_ignore_ascii_case(&claim.entity));
    if in_top_n {
        ClaimStatus::Verified
    } else {
        ClaimStatus::Mismatch {
            detail: format!(
                "entity `{}` is not in the top-{} rows of `{}` ranked by {}",
                claim.entity,
                n,
                table_label(&path),
                if over_pvalue {
                    "significance (smallest p)"
                } else {
                    "|effect size|"
                },
            ),
        }
    }
}

/// Resolve a claim's entity to its row in `cached`, bridging the
/// symbol↔Ensembl gap with the configured annotation map so a symbol-only
/// claim ("CRISPLD2 is the highest log2FC gene") and an accession-keyed table
/// row (`ENSG00000103196`) — or the reverse — verify identically.
///
/// Tries, in order: the entity exactly as written (normalized); the entity's
/// Ensembl id via the symbol→Ensembl map (both the versioned id and its
/// version-stripped stem, since tables key on either); and, when the entity is
/// itself an Ensembl id, every symbol the map points at that accession. Returns
/// `None` only when no form is present in the table, leaving the caller's
/// existing "entity not found → Unverifiable" abstention intact (never a false
/// Mismatch). Inert when no map is configured: it reduces to the plain lookup.
fn resolve_row_with_annotation_map<'a>(
    entity: &str,
    cached: &'a CachedTable,
    cfg: &ExtractorConfig,
) -> Option<&'a TableRow> {
    // 1. The entity exactly as written.
    if let Some(row) = cached.get_by_normalized(&normalize(entity)) {
        return Some(row);
    }
    let Some(map) = &cfg.gene_annotation_map else {
        return None;
    };
    // 2. symbol → Ensembl (try the full id and its version-stripped stem).
    if let Some(ens) = map.get(&entity.to_ascii_lowercase()) {
        if let Some(row) = cached.get_by_normalized(&normalize(ens)) {
            return Some(row);
        }
        if let Some(row) = cached.get_by_normalized(&normalize(&strip_id_version(ens))) {
            return Some(row);
        }
    }
    // 3. Ensembl → symbol(s): the entity may be an accession whose symbol keys
    //    the table. Match on the version-stripped accession so `ENSG….3`
    //    resolves the same symbol as the bare id.
    let entity_stem = strip_id_version(entity);
    for (sym, ens) in map.iter() {
        if strip_id_version(ens) == entity_stem {
            if let Some(row) = cached.get_by_normalized(&normalize(sym)) {
                return Some(row);
            }
        }
    }
    None
}

/// Which extreme a superlative selects on a given column.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ExtremeKind {
    /// Argmax: the row with the greatest column value (highest log2FC, largest NES).
    Max,
    /// Argmin: the row with the least column value (lowest padj, smallest p-value).
    Min,
}

/// Words that pick the MAXIMUM of the named column.
static EXTREME_MAX_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(highest|largest|strongest|maximal|greatest|top[\s-]?(?:ranked|most|scoring)?)\b",
    )
    .expect("static regex")
});
/// Words that pick the MINIMUM of the named column.
static EXTREME_MIN_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(lowest|smallest|weakest|minimal|least|bottom[\s-]?(?:ranked|most)?)\b",
    )
    .expect("static regex")
});
/// True when the excerpt names a P-VALUE-family column (padj/fdr/p-value/…),
/// so the extreme is taken over the p-value columns rather than effect size.
static EXTREME_PVAL_COL_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(padj|p[\s_-]?adj|fdr|q[\s_-]?value|qvalue|p[\s_-]?value|pvalue)\b")
        .expect("static regex")
});
/// Direction-of-regulation cues that mark the argMIN of a SIGNED effect column
/// (a down-regulated / most-negative extreme).
static EXTREME_DOWN_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(down[\s-]?(?:regulat|associat)|negative[\s-]+(?:effect|association)|most[\s-]?negative|largest[\s-]?negative)",
    )
        .expect("static regex")
});
/// Direction-of-regulation cues that mark the argMAX of a SIGNED effect column
/// (an up-regulated / most-positive extreme).
static EXTREME_UP_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(up[\s-]?(?:regulat|associat)|positive[\s-]+(?:effect|association)|most[\s-]?positive|largest[\s-]?positive)",
    )
        .expect("static regex")
});

/// Contrastive / non-replication phrasing that frames a direction word as a
/// reference to a PRIOR or contrasted finding rather than a claim about THIS
/// result — e.g. "in the opposite direction from the prior repression", "in
/// contrast to the earlier report", "unlike the previous study". Used only to
/// waive the direction hard-mismatch when the claim's OWN stated effect size
/// already confirms the observed direction (see the direction cross-check in
/// [`verify_one`]); it never turns a confirm into a flag.
static CONTRASTIVE_DIRECTION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(opposite[\s-]+(?:direction|to|of|from)|contrary\s+to|in\s+contrast|as\s+opposed\s+to|unlike\s+the|rather\s+than)",
    )
    .expect("static regex")
});

/// EXPLICIT p-value / significance EXTREME phrasing — the extreme IS taken over
/// the p-value column regardless of any signed-effect direction aside. "Most
/// significant gene (by padj)" ranks by padj (argmin, smallest p); a trailing
/// "strongly upregulated" merely describes that gene, it does not move the
/// extreme onto log2FC. This is DISTINCT from an INCIDENTAL padj annotation on
/// an effect-extreme claim ("most negative log2FC …, padj ≈ 5e-81"), which the
/// `EXTREME_PVAL_COL_RE` + direction-cue guard still routes to the effect
/// column. When this matches, the reported extreme on a p-value column is the
/// STRONGEST significance = smallest p (argmin).
static EXTREME_PVAL_EXPLICIT_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(most\s+(?:highly\s+)?significant|highly\s+significant|(?:greatest|highest|strongest)\s+significance|by\s+(?:padj|p[\s_-]?adj|fdr|q[\s_-]?value|p[\s_-]?value|pvalue)|(?:lowest|smallest)\s+(?:padj|p[\s_-]?adj|fdr|q[\s_-]?value|p[\s_-]?value|pvalue))",
    )
    .expect("static regex")
});

/// A3 — verify an ordinal/superlative EXTREME claim ("the strongest enrichment
/// by NES", "the most-downregulated gene by log2FC", "TP53 had the lowest
/// padj"). No explicit rank digit is present (those route to `RankTopN`). The
/// named entity must be the actual argmax/argmin of the cited column for the
/// stated extreme; otherwise it is a `Mismatch`.
///
/// Column selection: a p-value-family token in the excerpt picks the table's
/// p-value columns, else the effect-size columns. Extreme direction: a
/// max-word (highest/largest/strongest) → argmax; a min-word (lowest/smallest/
/// least) → argmin; for a p-value column the natural extreme inverts (a
/// "strongest"/"most-significant" p is the SMALLEST), so on a p-value column a
/// max-word is reinterpreted as argmin. Abstains to `Unverifiable` (never a
/// false Mismatch) when the extreme kind is ambiguous, the column is absent,
/// or the named entity itself is not in the table.
fn verify_extreme_value(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    if ANAPHORIC_RANK_SCOPE_RE.is_match(&claim.excerpt) {
        return ClaimStatus::Unverifiable {
            reason: "extreme-value scope refers to a displayed subset that is not encoded in the claim; global argmax/argmin was not substituted".into(),
        };
    }
    let Some(source_ref) = claim.source_table.as_deref() else {
        return ClaimStatus::Unverifiable {
            reason: "no source table cited — cannot verify extreme-value claim".into(),
        };
    };
    if index.resolve(source_ref).is_none() {
        return verify_via_sibling_discovery(claim, index, cfg, cache);
    }
    let (path, cached) = match cached_table_for(cache, index, source_ref, cfg) {
        Ok(t) => t,
        Err(status) => return status,
    };

    let excerpt = &claim.excerpt;
    // A p-value-family token in the excerpt normally selects the p-value columns
    // for the extreme — UNLESS the excerpt also carries a signed-effect direction
    // cue ("top down-regulated" / "most negative log2FC"). In that case the
    // extreme is over the EFFECT column and the p-value token is only the gene's
    // OWN padj annotation ("…most negative log2FC…, padj ≈ 5.1e-81"). Without this
    // guard the incidental padj flipped the extreme onto the p-value column and
    // false-Mismatched the true argmin gene whose padj is not the table minimum
    // (himes rerun 2026-07-22).
    let has_effect_direction_cue = claim.direction.is_some()
        || EXTREME_DOWN_RE.is_match(excerpt)
        || EXTREME_UP_RE.is_match(excerpt);
    // An EXPLICIT p-value/significance extreme ("most significant", "by padj",
    // "lowest padj") takes the extreme over the p-value column regardless of a
    // signed-effect direction ASIDE ("Most significant gene (by padj): SPARCL1
    // … strongly upregulated" ranks by padj — the "upregulated" describes that
    // gene, it does not move the extreme onto log2FC). Otherwise a bare p-value
    // token selects the p-value column only when no direction cue is present
    // (an incidental padj annotation on an effect-extreme claim stays on the
    // effect column — the 2026-07-22 guard).
    let explicit_pvalue_extreme = EXTREME_PVAL_EXPLICIT_RE.is_match(excerpt);
    let over_pvalue = explicit_pvalue_extreme
        || (EXTREME_PVAL_COL_RE.is_match(excerpt) && !has_effect_direction_cue);
    let columns: &[String] = if over_pvalue {
        &cfg.pvalue_columns
    } else {
        &cfg.effect_size_columns
    };

    // Resolve the extreme kind from the superlative word, inverting for a
    // p-value column (smaller p = stronger). Ambiguous (both or neither word
    // present) → abstain.
    let has_max = EXTREME_MAX_RE.is_match(excerpt);
    let has_min = EXTREME_MIN_RE.is_match(excerpt);
    // Direction-of-regulation cues over a SIGNED effect column disambiguate the
    // extreme even when a generic superlative ("top") is also present: "top
    // down-regulated / most negative" is an argMIN, "top up-regulated / most
    // positive" an argMAX. Without this the word "top" (a max-word) forced an
    // argmax reading of a down-regulation claim and false-Mismatched the true
    // argmin gene (himes rerun audit 2026-07-21). The cue only governs a signed
    // effect column, never a p-value column (whose extreme has no up/down sense).
    let (down_dir, up_dir) = if over_pvalue {
        (false, false)
    } else {
        match claim.direction {
            // Extraction binds direction words to their nearest entity. Prefer
            // that entity-local binding over sentence-wide regexes so a sentence
            // that names both the strongest positive and strongest negative
            // association verifies each entity against the correct extreme.
            Some(Direction::Down) => (true, false),
            Some(Direction::Up) => (false, true),
            None => (
                EXTREME_DOWN_RE.is_match(excerpt),
                EXTREME_UP_RE.is_match(excerpt),
            ),
        }
    };
    let kind = if down_dir && !up_dir {
        ExtremeKind::Min
    } else if up_dir && !down_dir {
        ExtremeKind::Max
    } else if over_pvalue {
        // On a p-value column the reported extreme is the STRONGEST significance
        // = smallest p (argmin): "most significant", "lowest padj", "smallest
        // p-value", and a generic "top … by padj" all select the minimum
        // (a max-word like "top"/"strongest" means strongest significance, i.e.
        // the smallest p). A "least significant" / "largest padj" argmax is not
        // asserted in practice, so p-value extremes resolve to argmin.
        ExtremeKind::Min
    } else {
        match (has_max, has_min) {
            (true, false) => ExtremeKind::Max,
            (false, true) => ExtremeKind::Min,
            _ => {
                return ClaimStatus::Unverifiable {
                    reason: "extreme-value claim names no unambiguous superlative — cannot determine argmax/argmin".into(),
                };
            }
        }
    };

    // The named entity must be present and carry a finite value in the column.
    // Resolve symbol↔Ensembl through the annotation map so a symbol-only claim
    // and an accession-keyed table row verify identically.
    let Some(claimed_row) = resolve_row_with_annotation_map(&claim.entity, cached, cfg) else {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "entity `{}` not found in table `{}` — cannot verify extreme",
                claim.entity,
                table_label(&path)
            ),
        };
    };
    let Some(claimed_val) = lookup_numeric(&claimed_row.values, columns).filter(|v| v.is_finite())
    else {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "entity `{}` has no finite value in the cited column of `{}` — cannot rank",
                claim.entity,
                table_label(&path)
            ),
        };
    };

    // Compute the true argmax/argmin over all rows carrying a finite value.
    let mut extreme: Option<(f64, &str)> = None;
    for r in &cached.rows {
        let Some(v) = lookup_numeric(&r.values, columns).filter(|v| v.is_finite()) else {
            continue;
        };
        let better = match (&extreme, kind) {
            (None, _) => true,
            (Some((best, _)), ExtremeKind::Max) => v > *best,
            (Some((best, _)), ExtremeKind::Min) => v < *best,
        };
        if better {
            extreme = Some((v, r.entity.as_str()));
        }
    }
    let Some((extreme_val, _extreme_entity)) = extreme else {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "table `{}` has no finite values in the cited column — cannot rank",
                table_label(&path)
            ),
        };
    };

    // The claim verifies when the named entity's value IS the extreme value
    // (ties are tolerated: any entity holding the extreme value passes).
    if claimed_val == extreme_val {
        ClaimStatus::Verified
    } else {
        ClaimStatus::Mismatch {
            detail: format!(
                "extreme: narrative names `{}` ({:.4}) as the {} value in `{}`, but the {} is {:.4}",
                claim.entity,
                claimed_val,
                match kind { ExtremeKind::Max => "maximum", ExtremeKind::Min => "minimum" },
                table_label(&path),
                match kind { ExtremeKind::Max => "maximum", ExtremeKind::Min => "minimum" },
                extreme_val,
            ),
        }
    }
}

/// Verify a group-comparison summary claim.
///
/// Confirms the direction of the effect-size column agrees with the claimed
/// direction word. Uses the same sign-check as the numeric-lookup path but
/// treats the absence of an explicit effect-size value as still verifiable
/// via the direction field alone.
fn verify_group_comparison(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    verify_one(claim, index, cfg, cache)
}

/// Verify a categorical-label claim.
///
/// Looks for a column whose name contains "label", "type", "cluster", or
/// "category" (case-insensitive) and checks whether its value for the
/// matched entity row contains the entity name itself or a token from the
/// claim excerpt.
fn verify_categorical(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    let Some(source_ref) = claim.source_table.as_deref() else {
        return ClaimStatus::Unverifiable {
            reason: "no source table cited — cannot verify categorical label".into(),
        };
    };
    // VF-11: unresolved cited table → sibling-membership discovery fallback.
    if index.resolve(source_ref).is_none() {
        return verify_via_sibling_discovery(claim, index, cfg, cache);
    }
    let (_path, cached) = match cached_table_for(cache, index, source_ref, cfg) {
        Ok(t) => t,
        Err(status) => return status,
    };
    let Some(row) = cached.get_by_normalized(&normalize(&claim.entity)) else {
        return ClaimStatus::Unverifiable {
            reason: format!("entity `{}` not found in table", claim.entity),
        };
    };

    // Find a label-like column.
    let label_col = row.values.keys().find(|k| {
        let k = k.as_str();
        k.contains("label") || k.contains("type") || k.contains("cluster") || k.contains("category")
    });
    if let Some(col) = label_col {
        let observed = row.values[col].to_lowercase();
        let excerpt_lower = claim.excerpt.to_lowercase();
        // Accept if the observed label appears in the excerpt (the narrative
        // typically quotes the label text directly).
        if !observed.is_empty() && excerpt_lower.contains(&observed) {
            return ClaimStatus::Verified;
        }
        return ClaimStatus::Mismatch {
            detail: format!(
                "categorical label `{}` not found in claim excerpt",
                row.values[col]
            ),
        };
    }

    // No label/type/cluster column to check a category against — but the
    // sentence was only routed here because it contained a categorical cue
    // word (e.g. "marker"). It may STILL carry a checkable direction/effect/
    // p-value about a real result-table gene ("ACAN was upregulated as a
    // marker of NP phenotype" — ACAN is -2.8/down in the DE table). The old
    // blanket-`Verified` existence check let that planted sign-flip pass
    // silently. Fall back to the numeric/direction verifier: a contradicting
    // sign now Mismatches, while a faithful direction (or a pure label mention
    // with no quantitative slot) still Verifies. No false positive — verify_one
    // only flags a slot the table positively refutes.
    verify_one(claim, index, cfg, cache)
}

/// Verify a time-series or clinical-trial summary claim.
///
/// Checks for a time-coordinate column ("day", "week", "timepoint", etc.)
/// and validates that the entity row's time value is mentioned in the
/// excerpt. When the table lacks a recognizable time column, falls back to
/// the existence check from `verify_one`.
fn verify_time_series(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    let Some(source_ref) = claim.source_table.as_deref() else {
        return ClaimStatus::Unverifiable {
            reason: "no source table cited — cannot verify time-series claim".into(),
        };
    };
    // VF-11: unresolved cited table → sibling-membership discovery fallback.
    if index.resolve(source_ref).is_none() {
        return verify_via_sibling_discovery(claim, index, cfg, cache);
    }
    // Scope the immutable cache borrow so the trailing `verify_one`
    // can reacquire it mutably without aliasing.
    let early = {
        let (_path, cached) = match cached_table_for(cache, index, source_ref, cfg) {
            Ok(t) => t,
            Err(status) => return status,
        };
        let Some(row) = cached.get_by_normalized(&normalize(&claim.entity)) else {
            return ClaimStatus::Unverifiable {
                reason: format!("entity `{}` not found in table", claim.entity),
            };
        };

        // Find a time-coordinate column.
        let time_col = row.values.keys().find(|k| {
            let k = k.as_str();
            k.contains("day")
                || k.contains("week")
                || k.contains("time")
                || k.contains("visit")
                || k.contains("period")
                || k.contains("cycle")
        });
        if let Some(col) = time_col {
            let observed = row.values[col].to_lowercase();
            let excerpt_lower = claim.excerpt.to_lowercase();
            if !observed.is_empty() && !excerpt_lower.contains(&observed) {
                Some(ClaimStatus::Mismatch {
                    detail: format!(
                        "time coordinate `{}` not mentioned in claim excerpt",
                        row.values[col]
                    ),
                })
            } else {
                None
            }
        } else {
            None
        }
    };
    if let Some(status) = early {
        return status;
    }

    // Fall through to numeric checks when the base check succeeds.
    verify_one(claim, index, cfg, cache)
}

fn verify_one(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    let Some(source_ref) = claim.source_table.as_deref() else {
        return ClaimStatus::Unverifiable {
            reason: "no source table cited in narrative".into(),
        };
    };
    // VF-11: a cited table that does not resolve (phantom or ambiguous) falls
    // back to sibling-membership discovery rather than a blanket Unverifiable.
    if index.resolve(source_ref).is_none() {
        return verify_via_sibling_discovery(claim, index, cfg, cache);
    }
    let (path, cached) = match cached_table_for(cache, index, source_ref, cfg) {
        Ok(t) => t,
        Err(status) => return status,
    };

    let claim_entity_norm = normalize(&claim.entity);
    let Some(row) = cached.get_by_normalized(&claim_entity_norm) else {
        // VF-0 — unverifiable-as-evasion catch. A CONFIDENT QUANTITATIVE claim
        // (a specific effect size or p-value) attributed to an entity ABSENT
        // from a successfully-loaded cited table is the fabricated- or
        // untested-gene signature — currently a silent Unverifiable pass. Flag
        // it SUSPICIOUS (soft / review-required; never a hard block). Two
        // guards keep this false-positive-free: (1) the claim must carry a
        // quantitative slot — a bare mention or pure interpretation sentence
        // has nothing fabricated to flag and stays Unverifiable; (2) the claim
        // entity's id-namespace must MATCH the table's, so a symbol looked up
        // in an Ensembl-keyed table (a benign cross-namespace miss, handled by
        // the gene_symbol↔Ensembl validator instead) stays Unverifiable.
        let has_quant = claim.effect_size.is_some() || claim.pvalue.is_some();
        if has_quant && namespace_matches_table(&claim.entity, cached) {
            return ClaimStatus::Suspicious {
                reason: format!(
                    "entity `{}` is absent from cited table `{}` ({} rows) yet a specific {} is asserted — a fabricated or untested finding, flagged for review",
                    claim.entity,
                    table_label(&path),
                    cached.rows.len(),
                    if claim.effect_size.is_some() { "effect size" } else { "p-value" },
                ),
            };
        }
        return ClaimStatus::Unverifiable {
            reason: format!(
                "entity `{}` not found in table `{}` (checked {} rows)",
                claim.entity,
                table_label(&path),
                cached.rows.len()
            ),
        };
    };

    // Effect size: compare sign + magnitude within tolerance.
    if let Some(claimed) = claim.effect_size {
        let observed = lookup_numeric(&row.values, &cfg.effect_size_columns);
        match observed {
            Some(obs) => {
                if (obs - claimed).abs() > cfg.log2fc_tolerance {
                    return ClaimStatus::Mismatch {
                        detail: format!(
                            "effect size: narrative says {:.4}, table has {:.4} (tolerance ±{:.4})",
                            claimed, obs, cfg.log2fc_tolerance
                        ),
                    };
                }
                if obs.signum() != claimed.signum() && claimed != 0.0 && obs != 0.0 {
                    return ClaimStatus::Mismatch {
                        detail: format!(
                            "effect size sign: narrative {:+.4} vs table {:+.4}",
                            claimed, obs
                        ),
                    };
                }
            }
            None => {
                return ClaimStatus::Unverifiable {
                    reason: "table has no configured effect-size column".into(),
                }
            }
        }
    }

    // VF-4 (linear-fold magnitude): prose often states magnitude as a LINEAR
    // fold ("induced 10-fold", "8-fold higher") rather than a log2 value, an
    // assertion that otherwise escapes verification entirely (the effect-size
    // slot only parses "log2FC=…"). Compare the claimed fold's MAGNITUDE
    // (log2 of the ratio) against the table's |log2FC| for the entity, and
    // flag ONLY a gross magnitude contradiction — off by more than one full
    // log2 unit (>2× in linear terms). Direction-agnostic by design (a sign
    // error is the direction check's / VF-6's job, not this one) and gated on
    // a meaningful fold (>1×), so an honest "~10-fold" against a 5–20× table
    // value is never flagged.
    if let Some(lf) = claim.linear_fold {
        if lf > 1.0 && lf.is_finite() {
            if let Some(obs) = lookup_numeric(&row.values, &cfg.effect_size_columns) {
                const LINEAR_FOLD_LOG2_BAND: f64 = 1.0; // one full doubling
                let claimed_log2_mag = lf.log2();
                let observed_log2_mag = obs.abs();
                if (claimed_log2_mag - observed_log2_mag).abs() > LINEAR_FOLD_LOG2_BAND {
                    return ClaimStatus::Mismatch {
                        detail: format!(
                            "fold-change magnitude: narrative claims {lf:.1}x (log2 {claimed_log2_mag:.2}) but table |log2FC| is {observed_log2_mag:.2} ({:.1}x)",
                            observed_log2_mag.exp2()
                        ),
                    };
                }
            }
        }
    }

    // Direction word cross-check: if narrative says "upregulated" but the
    // observed effect size is negative (or vice versa), flag it. This is
    // the highest-signal check and catches the lotz v1-style fabrication
    // pattern even when the numeric effect size was omitted.
    if let Some(direction) = claim.direction {
        // VF-19: resolve WHICH effect column matched so its scale (log2 pivots
        // at 0, ratio pivots at 1) drives both the near-no-change band and the
        // observed direction. `matched_numeric_column` returns the same value
        // `lookup_numeric` would, plus the column name.
        let observed = matched_numeric_column(&row.values, &cfg.effect_size_columns);
        if let Some((eff_col, obs)) = observed {
            let scale = effect_column_scale(eff_col);
            // Near-no-change / non-significance policy: a *bare* direction claim
            // (no stated effect-size value) on an entity that is both near the
            // scale's no-change point AND non-significant has no mechanically
            // determinable direction — neither confirmable nor refutable — so
            // it is `Unverifiable` rather than verified or flagged. The
            // no-change band is |log2FC| < 0.5 on a log scale, or a ratio
            // within ~1.5× of 1.0 (|ln(ratio)| < 0.405) on a ratio scale.
            // Significance is judged on the *adjusted* p (the largest reported
            // p-value-family value, so e.g. padj=0.16 reads non-significant
            // even when raw p<0.05); a claim that itself states an effect size
            // is exempt because it makes a checkable quantitative assertion.
            const NEAR_ZERO_LOG2FC: f64 = 0.5;
            const NEAR_ONE_LN_RATIO: f64 = 0.405; // ratio in ~[0.667, 1.5]
            let near_no_change = match scale {
                EffectScale::Log => obs.abs() < NEAR_ZERO_LOG2FC,
                EffectScale::Ratio => obs > 0.0 && obs.ln().abs() < NEAR_ONE_LN_RATIO,
            };
            if claim.effect_size.is_none() && near_no_change {
                let max_p = cfg
                    .pvalue_columns
                    .iter()
                    .filter_map(|c| {
                        row.values
                            .get(&normalize(c))
                            .and_then(|raw| raw.parse::<f64>().ok())
                    })
                    .filter(|v| v.is_finite())
                    .fold(None, |acc: Option<f64>, v| {
                        Some(acc.map_or(v, |a: f64| a.max(v)))
                    });
                let non_significant = max_p.map_or(true, |p| p >= 0.05);
                if non_significant {
                    return ClaimStatus::Unverifiable {
                        reason: format!(
                            "direction claim on a non-significant near-zero effect (log2FC {:+.4}, adjusted p >= 0.05); direction not mechanically determinable",
                            obs
                        ),
                    };
                }
            }
            // A no-change observed effect (0.0 on a log scale, 1.0 on a ratio
            // scale) agrees with NEITHER direction — an "upregulated"/
            // "downregulated" claim on a no-change row is a fabrication, not a
            // confirm. VF-19: a ratio column (HR=0.72) is "down" because it is
            // < 1.0; the old pivot-at-0 logic read 0.72 > 0 as "up" and falsely
            // Mismatched a faithful "the hazard was reduced (HR=0.72)".
            let observed_direction = observed_effect_direction(obs, scale);
            if observed_direction != Some(direction) {
                // The direction WORD contradicts the observed sign. This is a
                // Mismatch UNLESS the sentence is contrastive / non-replication
                // phrasing whose OWN stated effect size already confirms the
                // observed direction — then the direction word refers to a PRIOR
                // or contrasted finding, not this result (real himes: "IL11 …
                // log2FC = +0.098 … in the opposite direction from [the prior]
                // repression"). BOTH conditions are required: a bare claim, or
                // one whose number contradicts its own word ("increased hazard
                // (HR=0.72)"), still Mismatches — the number is what confirms the
                // word is merely contextual.
                let stated_confirms_observed = claim
                    .effect_size
                    .map(|e| observed_effect_direction(e, scale) == observed_direction)
                    .unwrap_or(false);
                let contrastive = CONTRASTIVE_DIRECTION_RE.is_match(&claim.excerpt);
                if !(stated_confirms_observed && contrastive) {
                    return ClaimStatus::Mismatch {
                        detail: format!(
                            "direction: narrative says {:?}, table effect value is {:+.4} (pivot {})",
                            direction,
                            obs,
                            match scale {
                                EffectScale::Log => "0",
                                EffectScale::Ratio => "1",
                            }
                        ),
                    };
                }
            }
        }
    }

    // P-value: allow relative tolerance; narrative rounding is common so
    // this is a softer check than effect size. DE / enrichment tables
    // typically carry BOTH a raw `pvalue` and an adjusted `padj`/`FDR`
    // column, and narratives usually quote the adjusted value — so accept
    // the claim if it agrees with ANY present p-value column within
    // tolerance rather than only the first one `lookup_numeric` finds
    // (which is the raw column and differs from `padj` by orders of
    // magnitude, producing false mismatches).
    if let Some(claimed_p) = claim.pvalue {
        if !claimed_p.is_finite() {
            return ClaimStatus::Unverifiable {
                reason: "p-value is not finite in narrative".into(),
            };
        }
        let observed: Vec<f64> = cfg
            .pvalue_columns
            .iter()
            .filter_map(|c| {
                row.values
                    .get(&normalize(c))
                    .and_then(|raw| raw.parse::<f64>().ok())
            })
            .filter(|v| v.is_finite())
            .collect();
        if observed.is_empty() {
            return ClaimStatus::Unverifiable {
                reason: "table has no configured p-value column/value for claimed p-value".into(),
            };
        }
        // VF-8 (p-laundering): the narrative attributed the value to an
        // ADJUSTED p-value ("padj"/"FDR"/"q…"), but the table's adjusted
        // column(s) disagree while a RAW `pvalue` column matches it — i.e. the
        // author quoted the (smaller, more impressive) raw p-value under an
        // adjusted label. Flag ONLY on this positive refutation: claim keyword
        // is adjusted-class AND the row HAS an adjusted column value AND no
        // adjusted value matches within tolerance AND some raw value DOES. If
        // the adjusted column matches (honest rounding), or the row carries no
        // adjusted column to adjudicate, this is inert — preserving the lenient
        // "match ANY p-column" acceptance below for honest claims. Asymmetric
        // by design: a raw value quoted under a raw label ("p = …") is never
        // flagged, only the laundering direction that inflates significance.
        if claim
            .matched_pvalue_keyword
            .as_deref()
            .is_some_and(is_adjusted_pvalue_keyword)
        {
            let in_tol =
                |obs: f64| pvalue_within_tolerance(claimed_p, obs, cfg.pvalue_relative_tolerance);
            let class_observed = |adjusted: bool| -> Vec<f64> {
                cfg.pvalue_columns
                    .iter()
                    .filter(|c| is_adjusted_pvalue_keyword(c) == adjusted)
                    .filter_map(|c| {
                        row.values
                            .get(&normalize(c))
                            .and_then(|raw| raw.parse::<f64>().ok())
                    })
                    .filter(|v| v.is_finite())
                    .collect()
            };
            let adjusted_observed = class_observed(true);
            let raw_observed = class_observed(false);
            if !adjusted_observed.is_empty()
                && !adjusted_observed.iter().copied().any(in_tol)
                && raw_observed.iter().copied().any(in_tol)
            {
                let adj_closest = adjusted_observed
                    .iter()
                    .cloned()
                    .min_by(|a, b| (claimed_p - a).abs().total_cmp(&(claimed_p - b).abs()))
                    .unwrap_or(adjusted_observed[0]);
                return ClaimStatus::Mismatch {
                    detail: format!(
                        "p-value laundering: narrative quotes adjusted p {claimed_p:.4e} but the table's adjusted column is {adj_closest:.4e}; the quoted value matches only the raw p-value column (raw value mis-labelled as adjusted)"
                    ),
                };
            }
        }
        let matches_any = observed
            .iter()
            .any(|&obs_p| pvalue_within_tolerance(claimed_p, obs_p, cfg.pvalue_relative_tolerance));
        if !matches_any {
            // Report against the numerically closest column for a readable
            // mismatch detail.
            let closest = observed
                .iter()
                .cloned()
                .min_by(|a, b| (claimed_p - a).abs().total_cmp(&(claimed_p - b).abs()))
                .unwrap_or(observed[0]);
            return ClaimStatus::Mismatch {
                detail: format!(
                    "p-value: narrative {:.4e} vs table {:.4e} (relative tolerance {}%)",
                    claimed_p,
                    closest,
                    (cfg.pvalue_relative_tolerance * 100.0) as u32
                ),
            };
        }
    }

    ClaimStatus::Verified
}

/// True when `claimed` agrees with `obs` within a relative tolerance.
/// Exact equality (incl. both-zero underflow) and the log-ratio band are both
/// accepted; a claimed exactly-zero p is treated as display rounding and agrees
/// when `obs` is below `PVALUE_ZERO_DISPLAY_FLOOR`; other non-positive values
/// match only on exact equality.
fn pvalue_within_tolerance(claimed: f64, obs: f64, rel_tol: f64) -> bool {
    if claimed == obs {
        return true;
    }
    // A claimed p of exactly 0.0 is a display-rounded value ("padj 0.000" in a
    // narrative table rounds a true tiny positive p to zero); it agrees iff the
    // observed p is itself below the small reporting floor, and Mismatches only
    // when the observed p is clearly non-significant. See
    // `PVALUE_ZERO_DISPLAY_FLOOR`.
    if claimed == 0.0 {
        return obs > 0.0 && obs <= PVALUE_ZERO_DISPLAY_FLOOR;
    }
    if claimed <= 0.0 || obs <= 0.0 {
        return false;
    }
    (claimed / obs).ln().abs() <= (1.0 + rel_tol).ln()
}

/// True when `tok` occurs in `key` at a run boundary that is not embedded
/// inside a longer ASCII-digit run — i.e., the character immediately before
/// and after the match are both non-digits (or the string edge). This keeps
/// the fuzzy token match's substring semantics for letter-flanked identifiers
/// (`s1` inside `x_s1_de`) while refusing a numeric cite from matching a
/// wider number: a "Table 5" cite (token `5`) matches the `5` segment of
/// `de_table_5` but NOT the `5` inside the `15` of `scrna_table_15`. Without
/// this guard the numeric token matched both, collapsing resolution to None
/// (≥2 candidates), which sent the numeric gate to scan every table and read
/// the wrong entity row for a legitimately-cited table.
fn token_matches_key(key: &str, tok: &str) -> bool {
    if tok.is_empty() {
        return false;
    }
    let kb = key.as_bytes();
    let tlen = tok.len();
    let mut start = 0usize;
    while let Some(rel) = key[start..].find(tok) {
        let at = start + rel;
        let before_is_digit = at > 0 && kb[at - 1].is_ascii_digit();
        let after = at + tlen;
        let after_is_digit = after < kb.len() && kb[after].is_ascii_digit();
        if !before_is_digit && !after_is_digit {
            return true;
        }
        start = at + 1;
    }
    false
}

/// In-memory index of `results/tables/*.{tsv,csv}` by file stem + full
/// name, case-insensitive. Cheap to construct; the narrative-size
/// expected input means a full scan is well under a millisecond.
struct TableIndex {
    by_name: BTreeMap<String, PathBuf>,
}

impl TableIndex {
    fn scan(root: &Path) -> Self {
        let mut by_name: BTreeMap<String, PathBuf> = BTreeMap::new();
        if let Ok(rd) = std::fs::read_dir(root) {
            for entry in rd.flatten() {
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                by_name.insert(normalize(name), path.clone());
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    by_name
                        .entry(normalize(stem))
                        .or_insert_with(|| path.clone());
                }
            }
        }
        Self { by_name }
    }

    /// The distinct table files in this index, deduped by path (each file is
    /// stored under both its full-name and its stem key). VF-11 enumerates
    /// these as the sibling set when a cited table fails to resolve.
    fn distinct_paths(&self) -> Vec<PathBuf> {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for p in self.by_name.values() {
            if seen.insert(p.clone()) {
                out.push(p.clone());
            }
        }
        out
    }

    /// Build an index containing a single known table file (its full
    /// name + stem keys). Used by the structured-claim path, where the
    /// evidence path already resolved to one concrete file and there is
    /// no directory to scan.
    fn single(path: &Path) -> Self {
        let mut by_name: BTreeMap<String, PathBuf> = BTreeMap::new();
        if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
            by_name.insert(normalize(name), path.to_path_buf());
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            by_name
                .entry(normalize(stem))
                .or_insert_with(|| path.to_path_buf());
        }
        Self { by_name }
    }

    /// Build an index over an explicit set of table paths (their full-name +
    /// stem keys). Used by the numeric gate on the discovery path, where the
    /// candidate tables are already enumerated rather than scanned from a root.
    fn from_paths(paths: &[PathBuf]) -> Self {
        let mut by_name: BTreeMap<String, PathBuf> = BTreeMap::new();
        for path in paths {
            if let Some(name) = path.file_name().and_then(|s| s.to_str()) {
                by_name.insert(normalize(name), path.clone());
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                by_name
                    .entry(normalize(stem))
                    .or_insert_with(|| path.clone());
            }
        }
        Self { by_name }
    }

    fn resolve(&self, source_ref: &str) -> Option<&Path> {
        // Strategy, in order — each step is a cheap map lookup or linear
        // scan over the (small) index:
        // 1. Exact file-name match: cite "de_summary.tsv".
        // 2. Exact stem: cite "de_summary".
        // 2b. Cited-path basename/stem EXACT match: cite
        // "results/tables/de_summary.tsv" → reduce to the bare basename
        // ("de_summary.tsv") and stem ("de_summary") and try each as an
        // exact lookup. This sits between the exact steps and the fuzzy
        // fallback so a cited *path* never collapses to None merely because
        // a twin table shares a fuzzy token (F4 laundering guard).
        // 3. Token match: cite "Table S1"; peel "table" off the front
        // and match any file whose stem contains the remaining
        // identifier ("s1"). This is the common case — narratives
        // use the RO-Crate-style reference, table files use a
        // descriptive slug.
        // 4. Whole-needle substring either direction.
        //
        // Steps 3 and 4 (fuzzy fallback) return `None` when ≥2
        // candidates match the needle — choosing the first one
        // silently hides ambiguity from the caller and risks
        // cross-table fabrication going unverified. Exact-match
        // steps (1, 2, 2b) remain deterministic and unique by
        // construction.
        let needle = normalize(source_ref.trim());
        if let Some(p) = self.by_name.get(&needle) {
            return Some(p);
        }
        let collapsed: String = needle.split_whitespace().collect();
        if let Some(p) = self.by_name.get(&collapsed) {
            return Some(p);
        }
        // Cited-path preference (F4): when the reference is a path, try its
        // bare basename (and stem) as an EXACT lookup before any fuzzy
        // fallback. A cited path must never collapse to None just because a
        // twin table shares a fuzzy token.
        if let Some(base) = std::path::Path::new(source_ref.trim())
            .file_name()
            .and_then(|s| s.to_str())
        {
            let base_norm = normalize(base);
            if let Some(p) = self.by_name.get(&base_norm) {
                return Some(p);
            }
            if let Some(stem) = std::path::Path::new(base)
                .file_stem()
                .and_then(|s| s.to_str())
            {
                if let Some(p) = self.by_name.get(&normalize(stem)) {
                    return Some(p);
                }
            }
        }
        let tokens: Vec<&str> = needle
            .split_whitespace()
            .filter(|t| *t != "table" && *t != "tables")
            .collect();
        for tok in &tokens {
            // Deduplicate by path value: the index stores both the full
            // filename key and the stem key for every file, so a token
            // contained in the stem will appear in both keys and produce
            // two references to the same path — which must not be treated
            // as ambiguity.
            let mut seen: std::collections::BTreeSet<&std::path::Path> =
                std::collections::BTreeSet::new();
            for (key, path) in &self.by_name {
                if token_matches_key(key, tok) {
                    seen.insert(path.as_path());
                }
            }
            match seen.len() {
                1 => return seen.into_iter().next(),
                0 => continue,
                _ => return None,
            }
        }
        // Step 4: whole-needle substring either direction, deduplicated.
        let mut seen: std::collections::BTreeSet<&std::path::Path> =
            std::collections::BTreeSet::new();
        for (key, path) in &self.by_name {
            if key.contains(&needle) || needle.contains(key.as_str()) {
                seen.insert(path.as_path());
            }
        }
        match seen.len() {
            1 => seen.into_iter().next(),
            _ => None,
        }
    }
}

#[derive(Debug)]
/// TableRow data.
pub struct TableRow {
    /// Entity.
    pub entity: String,
    /// Values keyed by already-lowercased column names. Lowercasing
    /// once at load time avoids the 20×3×20 = 1200 string clones per
    /// verification that a per-call lowercase map would incur.
    pub values: BTreeMap<String, String>,
}

/// Cached table rows + entity-index map. Avoids re-loading the same
/// CSV from disk per-claim (was: N file opens for N claims against
/// the same source_table). Entity normalization is precomputed so
/// `verify_one` does O(log N) lookup instead of an O(rows) linear scan.
struct CachedTable {
    rows: Vec<TableRow>,
    by_entity: BTreeMap<String, usize>,
}

impl CachedTable {
    /// Build from a freshly-parsed `Vec<TableRow>`, precomputing the
    /// normalized identifier-to-row map. Compatible identifier columns in the
    /// same family are indexed as aliases, so a `symbol` can resolve the row
    /// whose primary key is `gene`. On duplicate keys the first occurrence
    /// wins, matching the prior linear-search behavior.
    fn from_rows(rows: Vec<TableRow>, entity_columns: &[String]) -> Self {
        let mut by_entity: BTreeMap<String, usize> = BTreeMap::new();
        let primary_column = rows.first().and_then(|row| {
            entity_columns
                .iter()
                .find(|column| row.values.contains_key(&normalize(column)))
        });
        let primary_family = primary_column.and_then(|column| entity_alias_family(column));
        for (i, row) in rows.iter().enumerate() {
            by_entity.entry(normalize(&row.entity)).or_insert(i);
            if let Some(family) = primary_family {
                for column in entity_columns {
                    if entity_alias_family(column) != Some(family) {
                        continue;
                    }
                    let Some(alias) = row.values.get(&normalize(column)) else {
                        continue;
                    };
                    if !alias.trim().is_empty() {
                        by_entity.entry(normalize(alias)).or_insert(i);
                    }
                }
            }
        }
        Self { rows, by_entity }
    }

    /// Look a row up by already-normalized entity name. Returns `None`
    /// if the entity is absent.
    fn get_by_normalized(&self, needle: &str) -> Option<&TableRow> {
        self.by_entity
            .get(needle)
            .and_then(|idx| self.rows.get(*idx))
    }
}

/// Identifier columns in one family can safely serve as aliases for a row.
/// Unrelated dimensions such as `sample` and `gene` never alias each other.
fn entity_alias_family(column: &str) -> Option<&'static str> {
    let normalized = normalize(column).replace([' ', '-', '.'], "_");
    match normalized.as_str() {
        "gene" | "gene_id" | "gene_name" | "symbol" | "ensembl_id" | "ensembl_gene_id" => {
            Some("gene")
        }
        "pathway" | "pathway_id" | "term" | "gene_set_id" => Some("set"),
        "sample" | "sample_id" => Some("sample"),
        "cell" | "cell_id" | "barcode" => Some("cell"),
        "taxon" | "taxon_id" | "taxon_name" | "otu" | "otu_id" | "asv" => Some("taxon"),
        "region" | "peak" | "peak_id" | "cpg" | "cpg_id" | "site" | "probe" | "probe_id" => {
            Some("region")
        }
        "variant" | "snp" | "rsid" | "locus" => Some("variant"),
        "protein" | "protein_id" | "uniprot" => Some("protein"),
        "transcript" | "transcript_id" => Some("transcript"),
        _ => None,
    }
}

/// Get-or-load helper. Resolves `source_ref` against `index`, then
/// returns the cached `CachedTable` for the resolved path, loading
/// it from disk on first miss. Returns `Err(ClaimStatus::Unverifiable)`
/// when the table cannot be located or read so callers can short-circuit
/// without duplicating the diagnostic strings.
/// VF-11 — recover from a citation that does NOT resolve (phantom: the file
/// exists nowhere; or ambiguous: ≥2 fuzzy candidates collapse `resolve` to
/// None). Fires ONLY from the resolution-failure arm of the per-contract
/// verifiers, so a faithful resolvable citation is byte-for-byte unchanged.
///
/// It searches the index's sibling tables for ones CONTAINING the claim entity
/// and re-runs the contract against each, returning the best status
/// (Verified > Mismatch > Unverifiable/Suspicious) and short-circuiting on
/// Verified — so a correct value in a real table verifies even when the human's
/// table label was garbled. PROMOTES to Mismatch only when a real containing
/// sibling POSITIVELY contradicts and none verify. When NO sibling contains the
/// entity: Suspicious if the claim carries a quantitative slot whose namespace
/// matches some sibling's entity column (a fabricated/garbled-cite finding
/// flagged for review, mirroring VF-0), else Unverifiable (a genuine resolution
/// gap — honest claims about untested entities stay unverifiable, never
/// Mismatch).
fn verify_via_sibling_discovery(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    let needle = normalize(&claim.entity);
    let siblings = index.distinct_paths();
    let mut containing: Vec<PathBuf> = Vec::new();
    let mut any_loaded = false;
    for path in &siblings {
        if !cache.contains_key(path) {
            match load_table_rows(path, &cfg.entity_columns) {
                Ok(t) => {
                    cache.insert(path.clone(), t);
                }
                Err(e) => {
                    tracing::warn!(
                        target: "ecaa::claim_verifier",
                        table = %path.display(),
                        error = %e,
                        "sibling table not usable during VF-11 fallback (e.g. a non-result table with no configured entity column, or a genuine parse/IO error — see `error`); excluding it"
                    );
                    continue;
                }
            }
        }
        any_loaded = true;
        if cache
            .get(path)
            .map(|t| t.get_by_normalized(&needle).is_some())
            .unwrap_or(false)
        {
            containing.push(path.clone());
        }
    }

    if !containing.is_empty() {
        // Best-status reducer over the containing siblings (same promote policy
        // as verify_claims_with_discovery): Verified wins, else Mismatch, else
        // Unverifiable/Suspicious. Mismatch only on positive contradiction.
        let mut best: Option<ClaimStatus> = None;
        for path in &containing {
            let mut c = claim.clone();
            c.source_table = Some(table_label(path));
            let idx = TableIndex::single(path);
            let status = verify_for_contract(&c, &idx, cfg, cache);
            let verified = matches!(status, ClaimStatus::Verified);
            let prefer = match &best {
                None => true,
                Some(ClaimStatus::Verified) => false,
                Some(ClaimStatus::Mismatch { .. }) => verified,
                Some(_) => verified || matches!(status, ClaimStatus::Mismatch { .. }),
            };
            if prefer {
                best = Some(status);
            }
            if matches!(best, Some(ClaimStatus::Verified)) {
                break;
            }
        }
        return best.expect("non-empty containing set");
    }

    // No sibling contains the entity.
    let has_quant = claim.effect_size.is_some() || claim.pvalue.is_some();
    let namespace_match = any_loaded
        && siblings.iter().any(|p| {
            cache
                .get(p)
                .map(|t| namespace_matches_table(&claim.entity, t))
                .unwrap_or(false)
        });
    let cited = claim.source_table.as_deref().unwrap_or("?");
    if has_quant && namespace_match {
        ClaimStatus::Suspicious {
            reason: format!(
                "cited table `{cited}` does not resolve and `{}` is absent from all {} sibling result tables, yet a specific quantitative value is asserted — fabricated/garbled citation flagged for review",
                claim.entity,
                siblings.len()
            ),
        }
    } else {
        ClaimStatus::Unverifiable {
            reason: format!(
                "cited table `{cited}` not found and entity `{}` not present in any sibling result table",
                claim.entity
            ),
        }
    }
}

fn cached_table_for<'c>(
    cache: &'c mut BTreeMap<PathBuf, CachedTable>,
    index: &TableIndex,
    source_ref: &str,
    cfg: &ExtractorConfig,
) -> Result<(PathBuf, &'c CachedTable), ClaimStatus> {
    let Some(path) = index.resolve(source_ref) else {
        return Err(ClaimStatus::Unverifiable {
            reason: format!(
                "cited table `{}` not found under results tables",
                source_ref
            ),
        });
    };
    let owned: PathBuf = path.to_path_buf();
    if !cache.contains_key(&owned) {
        match load_table_rows(&owned, &cfg.entity_columns) {
            Ok(t) => {
                cache.insert(owned.clone(), t);
            }
            Err(e) => {
                return Err(ClaimStatus::Unverifiable {
                    reason: format!("table `{}` unreadable: {:#}", owned.display(), e),
                });
            }
        }
    }
    let cached = cache
        .get(&owned)
        .expect("just inserted or pre-existing entry");
    Ok((owned, cached))
}

/// Path-based loader. Resolves the CSV/TSV delimiter from the file
/// extension and dispatches to the pure
/// [`parse_table_rows_from_reader`]. Retained as the convenience entry
/// for the in-tree `verify_one` caller, which already has a `&Path`
/// from `TableIndex`.
///
/// C22 / R-7: the file `open()` is the one unavoidable fs call site
/// remaining inside `claim_verifier`. The actual CSV→`TableRow` parse
/// is pure and lives in `parse_table_rows_from_reader` so external
/// callers (or future migrations) can pre-load the bytes and skip the
/// fs altogether.
fn load_table_rows(path: &Path, entity_columns: &[String]) -> Result<CachedTable> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let delimiter = if ext == "csv" { b',' } else { b'\t' };
    let rows = parse_table_rows_from_reader(file, delimiter, entity_columns)
        .with_context(|| format!("parsing {}", path.display()))?;
    Ok(CachedTable::from_rows(rows, entity_columns))
}

/// Pure CSV/TSV → `TableRow` parser. No fs access; the caller chose
/// the reader and the delimiter (defaults: `b','` for CSV, `b'\t'` for
/// TSV). Surfaced so future C22 work can migrate `verify_claims`
/// callers to pre-loaded readers without rewriting the parse loop.
pub fn parse_table_rows_from_reader<R: Read>(
    reader: R,
    delimiter: u8,
    entity_columns: &[String],
) -> Result<Vec<TableRow>> {
    let mut csv_reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(true)
        .from_reader(reader);
    let headers = csv_reader.headers()?.clone();

    // Pick the first configured entity column that actually exists,
    // matching after NFC + ASCII-lowercase normalization so canonically-
    // equivalent Unicode forms (e.g. NFD-encoded headers) still bind.
    let header_norm: Vec<String> = headers.iter().map(normalize).collect();
    // Require a CONFIGURED entity column. We deliberately do NOT fall back to
    // "first column as entity": result tables expose their entity via the
    // policy `entityColumns` (e.g. de_results.tsv's `gene_id`, added in
    // Workstream A1), so a table with no configured entity column is a
    // NON-result table (method_landscape.csv keyed on `axis`,
    // mean_variance.tsv on `feature`, validation tables on `check_id`). A
    // first-column fallback let those load and then spuriously matched a
    // correct claim value against an unrelated number (e.g. a +1.50 log2FC vs
    // a mean of 10.79), producing false Mismatch verdicts. Erroring here makes
    // `verify_claims_with_discovery` warn-and-exclude such tables instead.
    let entity_idx = entity_columns
        .iter()
        .find_map(|col| {
            let needle = normalize(col);
            header_norm.iter().position(|h| h == &needle)
        })
        .ok_or_else(|| anyhow!("no configured entity column in headers {:?}", headers))?;

    let mut rows: Vec<TableRow> = Vec::new();
    for record in csv_reader.records() {
        let record = record?;
        let entity = record.get(entity_idx).unwrap_or("").to_string();
        let mut values: BTreeMap<String, String> = BTreeMap::new();
        // Build the map with already-normalized keys so lookup_numeric
        // doesn't have to rebuild it per call.
        for (norm_key, v) in header_norm.iter().zip(record.iter()) {
            values.insert(norm_key.clone(), v.to_string());
        }
        rows.push(TableRow { entity, values });
    }
    Ok(rows)
}

fn lookup_numeric(values: &BTreeMap<String, String>, columns: &[String]) -> Option<f64> {
    // Values is already normalized at load time (see
    // `load_table_rows`); look up directly without a per-call rebuild.
    // Only the needle needs normalization.
    for col in columns {
        if let Some(raw) = values.get(&normalize(col)) {
            if let Ok(v) = raw.parse::<f64>() {
                return Some(v);
            }
        }
    }
    None
}

/// As [`lookup_numeric`], but also returns WHICH configured column matched
/// (the configured name, not the normalized key) so the verifier can classify
/// that column's [`EffectScale`]. First parseable column wins, identical to
/// `lookup_numeric`'s order, so the value returned here is the same value
/// `lookup_numeric` would return — only the column identity is added. (VF-19)
fn matched_numeric_column<'a>(
    values: &BTreeMap<String, String>,
    columns: &'a [String],
) -> Option<(&'a str, f64)> {
    for col in columns {
        if let Some(raw) = values.get(&normalize(col)) {
            if let Ok(v) = raw.parse::<f64>() {
                return Some((col.as_str(), v));
            }
        }
    }
    None
}

/// Whether an effect-size column is on a LINEAR-ratio scale — no change at
/// 1.0 (hazard/odds/relative-risk ratios, fold-change) — or a LOG/additive
/// scale — no change at 0.0 (log2FC, NES, coefficients, mean/risk
/// differences). (VF-19)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EffectScale {
    /// Pivots at 0.0: sign gives direction (the established log2FC default).
    Log,
    /// Pivots at 1.0: value > 1 is up, < 1 is down (HR/OR/RR/fold-change).
    Ratio,
}

/// Classify an effect-size column name as [`EffectScale::Log`] or
/// [`EffectScale::Ratio`]. The `log` substring wins FIRST so a log-scaled
/// column that merely contains "ratio" (`log2_ratio`, `tmt_log2_ratio`,
/// `lfq_log2_ratio`) is correctly Log, NOT Ratio — the critical false-positive
/// guard. Additive/coefficient/difference families are Log. A named ratio
/// family or a `*ratio` name is Ratio. Anything unclassified DEFAULTS to Log,
/// so every existing RNA-seq column keeps today's pivot-at-0 semantics
/// (zero behaviour change for the whole existing corpus). (VF-19)
fn effect_column_scale(col: &str) -> EffectScale {
    let n = normalize(col);
    // 1. LOG wins first: any "log" substring forces Log even when "ratio" is
    //    also present (log2_ratio / tmt_log2_ratio). This is the FP guard.
    if n.contains("log") {
        return EffectScale::Log;
    }
    let tokens: Vec<&str> = n
        .split([' ', '_', '-', '.'])
        .filter(|t| !t.is_empty())
        .collect();
    // 2. RATIO: an exact ratio-family name (relative_risk has no "ratio"/"rr"
    //    token so it must be named explicitly), a name ending in "ratio", or a
    //    ratio-family token. Checked BEFORE the additive/difference families so
    //    `relative_risk` is Ratio while `risk_difference` falls through to Log.
    const RATIO_NAMES: &[&str] = &[
        "hazard_ratio",
        "odds_ratio",
        "relative_risk",
        "risk_ratio",
        "abundance_ratio",
        "fold_change",
        "foldchange",
    ];
    const RATIO_TOKENS: &[&str] = &["hr", "or", "rr", "fc", "fold", "ratio"];
    if RATIO_NAMES.contains(&n.as_str())
        || n.ends_with("ratio")
        || tokens.iter().any(|t| RATIO_TOKENS.contains(t))
    {
        return EffectScale::Ratio;
    }
    // 3. Additive / coefficient / statistic / DIFFERENCE families pivot at 0
    //    (mean_difference / risk_difference / NNT are additive or counts).
    const LOG_TOKENS: &[&str] = &[
        "nes",
        "es",
        "estimate",
        "coefficient",
        "coefficients",
        "coef",
        "beta",
        "effect",
        "effectsize",
        "score",
        "statistic",
        "diff",
        "difference",
        "md",
        "rd",
        "nnt",
    ];
    if tokens.iter().any(|t| LOG_TOKENS.contains(t)) {
        return EffectScale::Log;
    }
    // 4. Default: Log — preserves the established log2FC behaviour.
    EffectScale::Log
}

/// Observed direction implied by an effect value on its column's scale: Log
/// pivots at 0.0, Ratio pivots at 1.0. Exactly at the pivot → `None` (no
/// change agrees with NEITHER direction). (VF-19)
fn observed_effect_direction(obs: f64, scale: EffectScale) -> Option<Direction> {
    let pivot = match scale {
        EffectScale::Log => 0.0,
        EffectScale::Ratio => 1.0,
    };
    if obs > pivot {
        Some(Direction::Up)
    } else if obs < pivot {
        Some(Direction::Down)
    } else {
        None
    }
}

// ── Structured-claim verification ────────────────────────────────────────
//
// Real agent runs do not embed every claim as "GENEX upregulated
// (log2FC=2.1, Table S1)" prose. Per `AGENT-EXECUTOR.md` they emit a
// structured `claims` array in `result.json`, each entry pairing a
// free-text assertion with an `evidence` file path. That evidence path
// *is* the table citation — so these claims are verifiable even though
// the prose never says "Table S1", which the narrative regex path
// requires. The dominant real shape is also an *aggregate count*
// ("836 genes are differentially expressed at padj<0.05"), which the six
// per-entity contracts don't cover; those are recomputed directly from
// the evidence table here rather than trusting the agent's number.

/// A structured claim from an agent's `result.json` `claims[]` array: a
/// free-text assertion plus a pointer to the evidence file backing it.
#[derive(Debug, Clone, Deserialize)]
pub struct StructuredClaim {
    /// Free-text assertion the agent made.
    pub claim: String,
    /// Evidence file the claim cites (package-relative path or bare
    /// basename). `None` for claims with no evidence pointer.
    #[serde(default)]
    pub evidence: Option<String>,
}

/// Count-claim parse: "N <noun> ... <pvalue-col> < T", plus optional
/// direction / effect-magnitude constraints.
static COUNT_NOUN_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    // "N <up to 5 adjective words> <noun>" — the filler lets descriptors
    // sit between the count and its noun ("3 SME-supplied Drosophila gene
    // sets", "836 significantly differentially expressed genes").
    regex::Regex::new(
        r"(?i)\b(\d[\d,]*)\s+(?:[A-Za-z][\w-]*\s+){0,5}?(gene[\s-]?sets?|donor\s+cell[\s-]?lines?|cell[\s-]?lines?|cell[\s-]?types?|sub[\s-]?types?|entities?|genes?|features?|transcripts?|proteins?|peaks?|sites?|probes?|pathways?|terms?|cpgs?|loci|locus|snps?|variants?|regions?|clusters?|cells?|samples?|modules?|components?|domains?|communities|community|programs?|taxa|taxon|otus?|asvs?|species|genera|genus|families|family|phyla|phylum|lineages?)\b",
    )
    .expect("static regex")
});

/// Threshold parse: a p-value-family keyword followed by `<`/`≤` and a
/// number. Tolerates `padj<0.05`, `BH adj_p < 0.01`, `adj.p<0.01`,
/// `FDR < 0.05`, `q-value < 0.1`.
static THRESH_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    // Group 1 = the p-value-family keyword (so the verifier counts against
    // the column the claim actually names — `padj<0.05` must not be
    // checked against the raw `pvalue` column). Group 2 = the threshold.
    regex::Regex::new(
        r"(?i)(p[\s._-]?adj|adj[\s._-]?p(?:[\s._-]?val(?:ue)?)?|fdr|q[\s._-]?val(?:ue)?|adjusted\s+p[\s-]?val(?:ue)?|p[\s-]?val(?:ue)?|p)\)?\s*[<≤]\s*(\d*\.?\d+(?:[eE][+-]?\d+)?)",
    )
    .expect("static regex")
});

/// True when a p-value-family keyword (or column name) denotes a
/// *multiple-testing-adjusted* quantity rather than a raw p-value.
pub(crate) fn is_adjusted_pvalue_keyword(kw: &str) -> bool {
    let k = kw.to_ascii_lowercase().replace([' ', '.', '_', '-'], "");
    k.contains("adj") || k.contains("fdr") || k.starts_with('q') || k == "padj"
}

/// Effect-magnitude constraint parse: "LFC>1", "log2FC > 1.5",
/// "|log2FoldChange| > 1", "fold change > 2".
static EFFECT_THRESH_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\|?\s*(?:log2?\s*fc|log2?\s*fold[\s-]?change|lfc|fold[\s-]?change)\s*\|?\s*([<>])\s*(-?\d*\.?\d+)",
    )
    .expect("static regex")
});

/// True when the evidence's basename exists ANYWHERE under the package's result
/// trees (`runtime/outputs`, `results`, `runtime`), even in a location
/// `resolve_evidence_table` does not scan. VF-1 uses this to tell a PHANTOM
/// citation (file exists nowhere → fabrication) apart from a mere resolution
/// gap (file is present but unresolved). Depth-bounded so a pathological tree
/// cannot stall the verifier.
fn evidence_basename_exists(package_root: &Path, evidence: &str) -> bool {
    let Some(base) = Path::new(evidence.trim())
        .file_name()
        .and_then(|s| s.to_str())
    else {
        return false;
    };
    fn walk(dir: &Path, base: &str, depth: usize) -> bool {
        if depth > 6 {
            return false;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if walk(&p, base, depth + 1) {
                    return true;
                }
            } else if p
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.eq_ignore_ascii_case(base))
            {
                return true;
            }
        }
        false
    }
    ["runtime/outputs", "results", "runtime"]
        .iter()
        .any(|root| {
            let d = package_root.join(root);
            d.is_dir() && walk(&d, base, 0)
        })
}

/// Resolve a structured claim's `evidence` reference to a table file.
/// Tries, in order: the package-relative path verbatim; the bare
/// basename under `results/tables/`; the bare basename under any
/// `runtime/outputs/<task>/` directory. Returns `None` when nothing
/// matches. The bare-basename fallback is what makes a claim citing
/// `de_results.tsv` resolve to the file the agent actually wrote under
/// `runtime/outputs/differential_expression/`.
fn resolve_evidence_table(package_root: &Path, evidence: &str) -> Option<PathBuf> {
    let trimmed = evidence.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 1. Package-relative path verbatim (rejecting traversal).
    if !trimmed.contains("..") {
        let direct = package_root.join(trimmed);
        if direct.is_file() {
            return Some(direct);
        }
    }
    let base = Path::new(trimmed).file_name()?;
    // 2. results/tables/<base>
    let in_results = package_root.join("results").join("tables").join(base);
    if in_results.is_file() {
        return Some(in_results);
    }
    // 3. runtime/outputs/<task>/<base> for any task.
    let outputs = package_root.join("runtime").join("outputs");
    if let Ok(rd) = std::fs::read_dir(&outputs) {
        // Deterministic order: collect + sort task dirs.
        let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for d in dirs {
            let cand = d.join(base);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// Resolve the package's `claims_evidence_matrix.csv` for a literature-grounded
/// claim. Mirrors [`resolve_evidence_table`]'s deterministic, sorted lookup but
/// for the single PMID-anchored prior-work matrix the
/// `contextualize_findings_with_literature` atom writes.
///
/// Resolution order (each step deterministic):
/// 1. Canonical path
///    `runtime/outputs/contextualize_findings_with_literature/claims_evidence_matrix.csv`.
/// 2. `results/tables/claims_evidence_matrix.csv`.
/// 3. The first (sorted) `runtime/outputs/<task>/claims_evidence_matrix.csv`.
///
/// `finding_id`, `claimed_pmids`, and `cfg` are accepted for signature parity
/// with the structured verifier and to keep the call site self-documenting; the
/// matrix file is shared across findings, so the row filtering happens in
/// [`verify_literature_grounded`] rather than at resolution time.
fn resolve_evidence_literature(
    package_root: &Path,
    _finding_id: &str,
    _claimed_pmids: &[u64],
    _cfg: &ExtractorConfig,
) -> Option<PathBuf> {
    const MATRIX: &str = "claims_evidence_matrix.csv";
    // 1. Canonical contextualize_findings_with_literature output.
    let canonical = package_root
        .join("runtime")
        .join("outputs")
        .join("contextualize_findings_with_literature")
        .join(MATRIX);
    if canonical.is_file() {
        return Some(canonical);
    }
    // 2. results/tables.
    let in_results = package_root.join("results").join("tables").join(MATRIX);
    if in_results.is_file() {
        return Some(in_results);
    }
    // 3. Any runtime/outputs/<task>/claims_evidence_matrix.csv, sorted.
    let outputs = package_root.join("runtime").join("outputs");
    if let Ok(rd) = std::fs::read_dir(&outputs) {
        let mut dirs: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        dirs.sort();
        for d in dirs {
            let cand = d.join(MATRIX);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

/// One parsed row of `claims_evidence_matrix.csv`. Column meanings are pinned
/// by `config/stage-atoms/schemas/claims_evidence_matrix.schema.json`.
#[derive(Debug, Clone)]
struct LiteratureRow {
    finding_id: String,
    entity: String,
    prior_pmids: Vec<u64>,
    concordance_flag: String,
    source_kind: String,
    verified: bool,
    searched: bool,
}

/// `claims_evidence_matrix.csv` parsed into rows plus the *presence* of the
/// optional `verified` / `source_kind` columns. The contextualize atom's
/// emitted header (`finding_id,entity,entity_kind,pmid,evidence_quote,
/// concordance_flag`) OMITS both, so callers must distinguish "column absent"
/// (treat a recognized concordance_flag as the verification record) from
/// "column present and false" (a genuinely-unverified row).
#[derive(Debug, Clone)]
struct LiteratureMatrix {
    rows: Vec<LiteratureRow>,
    verified_present: bool,
    source_present: bool,
}

/// Load `claims_evidence_matrix.csv` into typed rows. `prior_pmids` is a
/// `;`-joined list per the schema; empty / non-numeric tokens are dropped.
/// The parse is pure CSV (comma-delimited, headers required) and tolerant of
/// missing optional columns — only `finding_id` and `entity` are required to
/// keep a row.
fn load_literature_rows(path: &Path) -> Result<LiteratureMatrix> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b',')
        .has_headers(true)
        .flexible(true)
        .from_reader(file);
    let headers = reader.headers()?.clone();
    let col = |name: &str| -> Option<usize> {
        let needle = normalize(name);
        headers.iter().position(|h| normalize(h) == needle)
    };
    let finding_idx = col("finding_id")
        .ok_or_else(|| anyhow!("claims_evidence_matrix.csv missing finding_id column"))?;
    let entity_idx =
        col("entity").ok_or_else(|| anyhow!("claims_evidence_matrix.csv missing entity column"))?;
    // The PMID column has three names across the schemas this file sees: the
    // canonical downstream-schema `prior_pmids` (plural, `;`-joined), the
    // singular `prior_pmid`, and the bare `pmid` the contextualize atom's
    // emitted header (`finding_id,entity,entity_kind,pmid,evidence_quote,
    // concordance_flag`) actually writes. Without the `pmid` alias every row's
    // PMID list parsed empty, so a narrative that correctly cited a prior PMID
    // was falsely flagged "cites PMID X but no supporting row" (Mismatch).
    let pmids_idx = col("prior_pmids")
        .or_else(|| col("prior_pmid"))
        .or_else(|| col("pmid"));
    let flag_idx = col("concordance_flag");
    let source_idx = col("source_kind");
    let verified_idx = col("verified");
    let searched_idx = col("searched");
    // The emitted contextualize header omits `verified` and `source_kind`
    // entirely. Distinguish "column absent" from "present-and-false" so the
    // caller can treat a recognized concordance_flag as the verification record
    // rather than forcing `any_verified = false`.
    let verified_present = verified_idx.is_some();
    let source_present = source_idx.is_some();

    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record?;
        let get = |i: Option<usize>| -> String {
            i.and_then(|k| record.get(k))
                .unwrap_or("")
                .trim()
                .to_string()
        };
        // Split on BOTH `;` and `,`: the canonical schema `;`-joins, but the
        // emitted `pmid` cell may carry a comma-separated list. (CSV-level
        // commas are field separators, so a multi-PMID `pmid` cell is quoted;
        // splitting the parsed cell on `,` recovers its tokens either way.)
        let prior_pmids = pmids_idx
            .and_then(|k| record.get(k))
            .map(|raw| {
                raw.split([';', ','])
                    .filter_map(|t| t.trim().parse::<u64>().ok())
                    .collect::<Vec<u64>>()
            })
            .unwrap_or_default();
        rows.push(LiteratureRow {
            finding_id: record.get(finding_idx).unwrap_or("").trim().to_string(),
            entity: record.get(entity_idx).unwrap_or("").trim().to_string(),
            prior_pmids,
            concordance_flag: get(flag_idx),
            source_kind: get(source_idx),
            verified: matches!(
                get(verified_idx).to_ascii_lowercase().as_str(),
                "true" | "1"
            ),
            searched: matches!(
                get(searched_idx).to_ascii_lowercase().as_str(),
                "true" | "1"
            ),
        });
    }
    Ok(LiteratureMatrix {
        rows,
        verified_present,
        source_present,
    })
}

/// Verify a literature-grounded support claim against the package's
/// `claims_evidence_matrix.csv`. The `TableIndex` only carries resolved table
/// paths, not the package root, so derive the root from the first indexed
/// path's `results/tables` or `runtime/outputs/<task>` ancestor; if the index
/// is empty the claim is `Unverifiable`.
fn verify_literature_grounded(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    _cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    let Some(package_root) = package_root_from_index(index) else {
        return ClaimStatus::Unverifiable {
            reason: "no package root resolvable for literature grounding".into(),
        };
    };
    verify_literature_grounded_at(claim, &package_root, cfg)
}

/// Derive a package root from any path the `TableIndex` resolved: walk up from a
/// `results/tables/*` or `runtime/outputs/<task>/*` file to the package root.
/// Returns `None` for an empty index or an unrecognized layout.
fn package_root_from_index(index: &TableIndex) -> Option<PathBuf> {
    let any = index.by_name.values().next()?;
    let cur = any.parent()?;
    // results/tables/<f> → up 2; runtime/outputs/<task>/<f> → up 3.
    let comps: Vec<&std::ffi::OsStr> = cur.iter().collect();
    let depth = if comps.iter().rev().take(2).any(|c| *c == "tables") {
        2
    } else if comps.iter().rev().take(3).any(|c| *c == "outputs") {
        3
    } else {
        1
    };
    let mut cur = cur;
    for _ in 0..depth {
        cur = cur.parent()?;
    }
    Some(cur.to_path_buf())
}

/// Package-root-explicit core of literature-grounded verification. Separated
/// from the index-derived wrapper so tests can drive it with a concrete root.
fn verify_literature_grounded_at(
    claim: &Claim,
    package_root: &Path,
    cfg: &ExtractorConfig,
) -> ClaimStatus {
    // Bind by the in-sentence finding_id/PMID when the narrative carried one;
    // otherwise FALL BACK to a gene-symbol lookup. This claim already classified
    // LiteratureGrounded (a prior-work / concordance cue is present), and
    // claims_evidence_matrix.csv keys findings by gene symbol in its `entity`
    // column — so a "concordant genes: SPARCL1, DUSP1, …" claim whose PMID sits
    // in a separate section header still adjudicates against the matrix BY SYMBOL
    // (via the entity-match filter below) rather than being stranded Unverifiable
    // for lack of an in-sentence citation. A non-empty entity is required to have
    // something to resolve; a gene absent from the matrix still falls through to
    // Unverifiable (no fabricated grounding, no over-binding).
    // `symbol_fallback` = there was no in-sentence citation, so grounding rests on
    // the matrix's OWN adjudication (a `verified` `same_direction` row) rather than
    // on the narrative citing papers. The `min_papers` / `min_sources` bars below
    // corroborate a NARRATIVE's citations; they do not apply to the fallback (a
    // genuine single-foundational-paper concordance — e.g. every airway gene
    // validated against Himes et al. 2014 — carries exactly one prior PMID and
    // would otherwise be stranded by the default `>= 2 papers` bar).
    //
    // The fallback signal is an EMPTY citation set, NOT the absence of the
    // LiteratureEvidence wrapper: the extractor attaches a wrapper to EVERY
    // LiteratureGrounded claim (`finding_id` = the entity, `cited_pmids` bound
    // from any inline PMID), so `literature_evidence == None` never occurs on real
    // extractor output — a bare "Concordant genes: SPARCL1, DUSP1, …" enumeration
    // whose PMID sits in a separate header arrives as `Some { finding_id: "SPARCL1",
    // cited_pmids: [] }`. Keying the waiver on `cited_pmids.is_empty()` is what
    // lets those genes bind by symbol; the finding_id (= entity) or, absent a
    // wrapper, the claim entity supplies the resolvable identifier.
    let (finding_id, cited_pmids, symbol_fallback): (String, Vec<u64>, bool) = match claim
        .literature_evidence
        .as_ref()
    {
        Some(evidence) if !evidence.cited_pmids.is_empty() => (
            evidence.finding_id.clone(),
            evidence.cited_pmids.clone(),
            false,
        ),
        Some(evidence) if !evidence.finding_id.trim().is_empty() => {
            (evidence.finding_id.clone(), Vec::new(), true)
        }
        _ if !claim.entity.trim().is_empty() => (claim.entity.clone(), Vec::new(), true),
        _ => {
            return ClaimStatus::Unverifiable {
                    reason: "literature-grounded claim carries no finding_id / cited PMIDs and no entity to resolve".into(),
                };
        }
    };
    let Some(matrix_path) =
        resolve_evidence_literature(package_root, &finding_id, &cited_pmids, cfg)
    else {
        return ClaimStatus::Unverifiable {
            reason: "claims_evidence_matrix.csv not found in package".into(),
        };
    };
    let matrix = match load_literature_rows(&matrix_path) {
        Ok(r) => r,
        Err(e) => {
            return ClaimStatus::Unverifiable {
                reason: format!("claims_evidence_matrix.csv unreadable: {:#}", e),
            }
        }
    };
    let LiteratureMatrix {
        rows,
        verified_present,
        source_present,
    } = &matrix;

    // VF-13 machinery (reused defensively): a symbol-keyed claim must still
    // match an Ensembl-keyed finding_id when an independent annotation map is
    // configured. Resolve the claim's entity / finding_id through the map so a
    // `KLF15` claim matches an `ENSG00000163884` row (and vice versa).
    let entity_norm = normalize(&claim.entity);
    let mut alias_ids: Vec<String> = Vec::new();
    if let Some(map) = &cfg.gene_annotation_map {
        // symbol → Ensembl
        if let Some(ens) = map.get(&claim.entity.to_ascii_lowercase()) {
            alias_ids.push(normalize(ens));
            alias_ids.push(normalize(&strip_id_version(ens)));
        }
        // Ensembl → symbol(s): the finding_id may be an Ensembl id whose symbol
        // is the claim entity; resolve back so an Ensembl-keyed claim matches a
        // symbol-keyed row too.
        let fid_norm = normalize(&strip_id_version(&finding_id));
        for (sym, ens) in map.iter() {
            if normalize(&strip_id_version(ens)) == fid_norm || normalize(ens) == entity_norm {
                alias_ids.push(normalize(sym));
            }
        }
    }
    let matches_alias = |r: &LiteratureRow| -> bool {
        if alias_ids.is_empty() {
            return false;
        }
        let fid = normalize(&strip_id_version(&r.finding_id));
        let ent = normalize(&r.entity);
        alias_ids.iter().any(|a| *a == fid || *a == ent)
    };

    // Rows backing this finding: prefer an exact finding_id match; fall back to
    // entity match (older matrices keyed only by entity), and finally an
    // annotation-map alias (symbol↔Ensembl).
    let matched: Vec<&LiteratureRow> = rows
        .iter()
        .filter(|r| {
            r.finding_id.eq_ignore_ascii_case(&finding_id)
                || normalize(&r.entity) == entity_norm
                || matches_alias(r)
        })
        .collect();
    if matched.is_empty() {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "no claims_evidence_matrix row for finding `{}` / entity `{}`",
                finding_id, claim.entity
            ),
        };
    }

    // A matched row flagged `opposite_direction` only CONTRADICTS the narrative
    // when the narrative positively asserts concordance with prior work — a
    // fabricated concordance. Gated on the same `AGREEMENT_CUES` as the
    // `no_prior_finding` branch below: a faithful description of the discordance
    // (e.g. "...showing a small, non-significant fold change in a discordant
    // direction in these data") or a neutral citation carries no cue and AGREES
    // with the matrix, so it falls through to be credited as a genuine
    // adjudication record (the same handling `same_direction` gets).
    if matched
        .iter()
        .any(|r| r.concordance_flag == "opposite_direction")
        && AGREEMENT_CUES
            .iter()
            .any(|c| claim.excerpt.to_lowercase().contains(c))
    {
        return ClaimStatus::Mismatch {
            detail: format!(
                "literature: matrix records opposite-direction prior finding for `{}`",
                claim.entity
            ),
        };
    }

    // VF-15a — a narrative that POSITIVELY ASSERTS agreement/concordance with
    // prior work, while the matrix flags the finding `no_prior_finding`, is a
    // fabricated concordance → Mismatch. Gated on an explicit agreement cue in
    // the excerpt: a neutral mention (no concordance claim) with a
    // no_prior_finding flag falls through to the Unverifiable arms below, so a
    // faithful "no prior work" statement is never flagged.
    if matched
        .iter()
        .any(|r| r.concordance_flag == "no_prior_finding")
    {
        let lower = claim.excerpt.to_lowercase();
        if AGREEMENT_CUES.iter().any(|c| lower.contains(c)) {
            return ClaimStatus::Mismatch {
                detail: format!(
                    "literature: narrative asserts prior-work concordance for `{}` but the matrix records no_prior_finding",
                    claim.entity
                ),
            };
        }
    }

    // A matched row carrying one of these concordance flags is a genuine
    // verification record: the contextualize step adjudicated the finding
    // against prior work. `opposite_direction` is included here too: a row
    // reaches this point only when the narrative did NOT assert concordance
    // (the gated Mismatch above did not fire), so a faithful discordance
    // description or a neutral mention is a genuine adjudication record, not a
    // contradiction. `no_prior_finding` is likewise treated as a (neutral)
    // adjudication so a faithful "no prior work" claim — which reached here
    // precisely because it carries NO fabricated-concordance cue — is not
    // stranded as Unverifiable.
    const RECOGNIZED_FLAGS: &[&str] = &[
        "same_direction",
        "opposite_direction",
        "unverifiable",
        "no_prior_finding",
        "not_assessed",
    ];

    // Every narrative-cited PMID must appear in the matrix's supporting set.
    let mut supporting: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    let mut sources: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut any_verified = false;
    for r in &matched {
        let recognized_flag = RECOGNIZED_FLAGS.contains(&r.concordance_flag.as_str());
        // The `verified` column is the canonical signal when present. When the
        // emitted header OMITS it, a matched row with a recognized
        // concordance_flag IS the verification record (the step ran and
        // adjudicated); without that fallback every emitted-schema claim would
        // be falsely Unverifiable.
        // `no_prior_finding` is a verified negative retrieval result when the
        // retained scope says the entity was searched. It intentionally has
        // `verified=false`: there is no evidence quote to substring-check.
        // Requiring a positive quote-verification bit here stranded faithful
        // "searched, no prior finding" statements as Unverifiable even though
        // the matrix retained exactly the machine-resolved support record.
        let verified_negative_retrieval = r.concordance_flag == "no_prior_finding" && r.searched;
        if r.verified || verified_negative_retrieval || (!verified_present && recognized_flag) {
            any_verified = true;
        }
        for p in &r.prior_pmids {
            supporting.insert(*p);
        }
        if !r.source_kind.is_empty() && r.source_kind != "none" {
            sources.insert(r.source_kind.clone());
        } else if !source_present && recognized_flag {
            // Emitted header carries no `source_kind` column. The adjudicated
            // row still represents one source kind (the contextualize evidence
            // record); credit it so a genuinely-supported claim is not tripped
            // by `literature_min_sources`. Keyed on finding_id so distinct rows
            // contribute distinct sources, matching the present-column shape.
            sources.insert(format!("contextualize:{}", r.finding_id));
        }
    }
    for cited in &cited_pmids {
        if !supporting.contains(cited) {
            return ClaimStatus::Mismatch {
                detail: format!(
                    "literature: narrative cites PMID {} but the matrix has no such supporting row for `{}`",
                    cited, claim.entity
                ),
            };
        }
    }
    if !any_verified {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "literature: no verified evidence row backs finding `{}`",
                finding_id
            ),
        };
    }
    // The `min_papers` / `min_sources` bars corroborate a NARRATIVE that cites
    // its own papers; they do not gate the symbol-fallback path, which is backed
    // by the matrix's own adjudication (see `symbol_fallback` above).
    if !symbol_fallback && supporting.len() < cfg.literature_min_papers {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "literature: {} supporting paper(s) for `{}`, policy requires >= {}",
                supporting.len(),
                claim.entity,
                cfg.literature_min_papers
            ),
        };
    }
    if !symbol_fallback && sources.len() < cfg.literature_min_sources {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "literature: {} distinct source kind(s) for `{}`, policy requires >= {}",
                sources.len(),
                claim.entity,
                cfg.literature_min_sources
            ),
        };
    }
    ClaimStatus::Verified
}

/// Strip thousands separators and parse a captured count.
fn parse_count(raw: &str) -> Option<f64> {
    raw.replace(',', "").parse::<f64>().ok()
}

#[derive(Debug)]
struct TestedSummary {
    total: usize,
    significant: usize,
    up: Option<usize>,
    down: Option<usize>,
}

static TESTED_SIGNIFICANT_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?is)\bof\s+(\d[\d,]*)\s+(?:[A-Za-z][\w-]*\s+){0,3}?(?:gene[\s-]?sets?|genes?|features?|transcripts?|proteins?|peaks?|sites?|probes?|pathways?|terms?|cpgs?|loci|locus|snps?|variants?|regions?)\s+tested\b.*?\b(\d[\d,]*)\s+(?:were\s+)?(?:statistically\s+)?significant\b",
    )
    .expect("static regex")
});

static UP_COUNT_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(\d[\d,]*)\s+(?:were\s+)?(?:up[\s-]?regulated|enriched)\b")
        .expect("static regex")
});

static DOWN_COUNT_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(\d[\d,]*)\s+(?:were\s+)?(?:down[\s-]?regulated|depleted)\b")
        .expect("static regex")
});

fn parse_tested_summary(text: &str) -> Option<TestedSummary> {
    let text = text.replace('*', "");
    let captures = TESTED_SIGNIFICANT_RE.captures(&text)?;
    let parse = |index| {
        captures
            .get(index)
            .and_then(|value| parse_count(value.as_str()))
            .map(|value| value as usize)
    };
    let directional = |pattern: &regex::Regex| {
        pattern
            .captures(&text)
            .and_then(|value| value.get(1))
            .and_then(|value| parse_count(value.as_str()))
            .map(|value| value as usize)
    };
    Some(TestedSummary {
        total: parse(1)?,
        significant: parse(2)?,
        up: directional(&UP_COUNT_RE),
        down: directional(&DOWN_COUNT_RE),
    })
}

fn coalesce_wrapped_prose(narrative: &str) -> String {
    fn is_structural(line: &str) -> bool {
        let line = line.trim_start();
        line.starts_with('#')
            || line.starts_with('|')
            || line.starts_with("- ")
            || line.starts_with("* ")
            || line.starts_with("+ ")
            || line.starts_with("> ")
            || line.starts_with("```")
            || line.starts_with("~~~")
            || line.starts_with("<!--")
            || line.starts_with("**")
    }

    let mut out = String::with_capacity(narrative.len());
    let mut prose = String::new();
    let flush = |out: &mut String, prose: &mut String| {
        if !prose.is_empty() {
            out.push_str(prose.trim());
            out.push('\n');
            prose.clear();
        }
    };
    for line in narrative.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush(&mut out, &mut prose);
            out.push('\n');
        } else if is_structural(trimmed) {
            flush(&mut out, &mut prose);
            out.push_str(trimmed);
            out.push('\n');
        } else {
            if !prose.is_empty() {
                prose.push(' ');
            }
            prose.push_str(trimmed);
        }
    }
    flush(&mut out, &mut prose);
    out
}

/// Attempt to verify `text` as an aggregate count claim against
/// `table_path`. Returns `None` when the text is not count-shaped (no
/// "N <noun>" + threshold), so the caller can fall back to per-entity
/// verification. Recomputes the count from the table rather than trusting
/// the agent's figure: counts rows whose configured p-value column is
/// below the claimed threshold and (when present) whose effect size
/// satisfies the claimed direction / magnitude constraint.
fn verify_count_claim(text: &str, table_path: &Path, cfg: &ExtractorConfig) -> Option<ClaimStatus> {
    let cached = load_table_rows(table_path, &cfg.entity_columns).ok()?;
    verify_count_claim_in_table(text, table_path, cfg, &cached)
}

/// Verify a count claim against an already loaded table.
fn verify_count_claim_in_table(
    text: &str,
    table_path: &Path,
    cfg: &ExtractorConfig,
    cached: &CachedTable,
) -> Option<ClaimStatus> {
    // "N of M <noun> significant" — the verifiable count is N (how many
    // passed), not M (the total tested). Prefer the leading number when
    // the "X of Y" shape is present; otherwise take the number written
    // directly before the noun.
    static COUNT_OF_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b(\d[\d,]*)\s+of\s+\d[\d,]*\s+(?:[A-Za-z][\w-]*\s+){0,5}?(?:genes?|features?|transcripts?|proteins?|peaks?|sites?|probes?|gene[\s-]?sets?|pathways?|terms?|cpgs?|loci|locus|snps?|variants?|regions?)\b").expect("static regex")
    });
    let noun_caps = COUNT_NOUN_RE.captures(text)?;
    let noun = noun_caps.get(2)?.as_str().to_lowercase();
    let claimed_n = if let Some(c) = COUNT_OF_RE.captures(text) {
        parse_count(c.get(1)?.as_str())?
    } else {
        parse_count(noun_caps.get(1)?.as_str())?
    };

    // No p-value threshold in the claim: handle the "N <grouping> identified"
    // shape ("6 clusters", "12 cell types", "8 taxa") by counting DISTINCT
    // values of the grouping column. Other threshold-less counts
    // ("8,766 genes tested") stay unverifiable — a raw row count would
    // false-mismatch NA-filtered tables.
    let Some(thresh_caps) = THRESH_RE.captures(text) else {
        if is_grouping_noun(&noun) {
            if let Some(col) = grouping_column(&cached, &noun) {
                let mut seen: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                for row in &cached.rows {
                    if let Some(v) = row.values.get(&col) {
                        let v = v.trim();
                        if !v.is_empty() {
                            seen.insert(v.to_string());
                        }
                    }
                }
                return Some(compare_count(
                    claimed_n,
                    seen.len(),
                    table_path,
                    &format!("distinct `{col}` values"),
                ));
            }
        }
        return None;
    };
    let threshold_kw = thresh_caps.get(1)?.as_str();
    let threshold: f64 = thresh_caps.get(2)?.as_str().parse().ok()?;

    // Count against the p-value column the claim *names*. A `padj<0.05`
    // claim must be checked against the adjusted column, not the raw
    // `pvalue` column that `lookup_numeric` would otherwise pick first
    // (DESeq2 tables carry both, and raw-p row counts are far larger).
    // Partition the configured columns into adjusted vs raw, then order
    // them so the claimed class wins while the other stays as a fallback.
    let want_adjusted = is_adjusted_pvalue_keyword(threshold_kw);
    let (adjusted_cols, raw_cols): (Vec<String>, Vec<String>) = cfg
        .pvalue_columns
        .iter()
        .cloned()
        .partition(|c| is_adjusted_pvalue_keyword(c));
    let ordered_cols: Vec<String> = if want_adjusted {
        adjusted_cols.into_iter().chain(raw_cols).collect()
    } else {
        raw_cols.into_iter().chain(adjusted_cols).collect()
    };

    // Resolve ONE significance column for the whole table — the first
    // configured column (claimed-class-first) that actually exists in this
    // table's header. Counting must NOT fall through to the raw `pvalue`
    // column on a per-row basis: a DESeq2 `padj` cell is NA exactly when
    // independent filtering excluded that gene, and such a row is *not* below
    // a `padj<0.05` threshold. A per-row `lookup_numeric` over an ordered list
    // silently consulted `pvalue` for those NA-`padj` rows and over-counted by
    // the number of independent-filtered-but-raw-significant genes (4017 →
    // 4146 on the Himes airway DE table). Pinning the column once means an NA
    // adjusted cell drops the row; the raw fallback applies only when the
    // table carries no adjusted column at all.
    let col_present = |col: &str| -> bool {
        let needle = normalize(col);
        cached
            .rows
            .first()
            .is_some_and(|r| r.values.contains_key(&needle))
    };
    let Some(count_col) = ordered_cols.iter().find(|c| col_present(c)).cloned() else {
        return None;
    };
    let count_cols = [count_col];

    // Optional effect-magnitude constraint ("LFC>1").
    let effect_thresh: Option<(char, f64)> = EFFECT_THRESH_RE.captures(text).and_then(|c| {
        let op = c.get(1)?.as_str().chars().next()?;
        let val: f64 = c.get(2)?.as_str().parse().ok()?;
        Some((op, val))
    });
    // Direction word (only the up/down sets; nearest-wins is irrelevant
    // for an aggregate count).
    // FP-A: for a gene-SET / pathway / term count, "enriched"/"depleted" denote
    // SIGNIFICANCE (a set is enriched at either tail of the ranked list), NOT a
    // gene-level up/down direction. Applying the effect-sign (NES) filter to
    // "N enriched gene sets at padj<0.05" wrongly drops the negative-NES
    // significant sets (453 → 334 on the Himes GSEA run). Skip the direction
    // filter entirely for set-level nouns so the count is over ALL significant
    // sets, matching how GSEA reports "enriched".
    let lower = text.to_lowercase();
    let set_level = is_set_level_noun(&noun);
    let has_up = !set_level
        && cfg
            .up_words
            .iter()
            .any(|w| lower.contains(&w.to_lowercase()));
    let has_down = !set_level
        && cfg
            .down_words
            .iter()
            .any(|w| lower.contains(&w.to_lowercase()));

    let mut observed = 0usize;
    for row in &cached.rows {
        let Some(p) = lookup_numeric(&row.values, &count_cols) else {
            continue;
        };
        if !(p.is_finite() && p < threshold) {
            continue;
        }
        // Effect constraints, when the claim states one.
        let eff = lookup_numeric(&row.values, &cfg.effect_size_columns);
        if let Some((op, val)) = effect_thresh {
            let Some(e) = eff else { continue };
            let ok = match op {
                '>' => e > val,
                '<' => e < val,
                _ => true,
            };
            // A bare "LFC>1" with a stated down-direction means the
            // magnitude band on the negative side (LFC < -1).
            let ok = if has_down && op == '>' && val > 0.0 {
                e < -val
            } else {
                ok
            };
            if !ok {
                continue;
            }
        } else if has_up || has_down {
            let Some(e) = eff else { continue };
            if has_up && !has_down && e <= 0.0 {
                continue;
            }
            if has_down && !has_up && e >= 0.0 {
                continue;
            }
        }
        observed += 1;
    }

    Some(compare_count(
        claimed_n,
        observed,
        table_path,
        "rows below the cited threshold",
    ))
}

/// Resolve the single significance (p-value) column to count against in
/// `cached`, preferring the adjusted family. Returns the configured column
/// name (so the caller can `lookup_numeric` on a one-element slice) or `None`
/// when the table carries no configured p-value column at all. Mirrors the
/// column-pinning logic inside [`verify_count_claim`] (an NA adjusted cell
/// must drop the row, never fall through to the raw column).
fn resolve_significance_column(cached: &CachedTable, cfg: &ExtractorConfig) -> Option<String> {
    let (adjusted_cols, raw_cols): (Vec<String>, Vec<String>) = cfg
        .pvalue_columns
        .iter()
        .cloned()
        .partition(|c| is_adjusted_pvalue_keyword(c));
    let ordered: Vec<String> = adjusted_cols.into_iter().chain(raw_cols).collect();
    let col_present = |col: &str| -> bool {
        let needle = normalize(col);
        cached
            .rows
            .first()
            .is_some_and(|r| r.values.contains_key(&needle))
    };
    ordered.into_iter().find(|c| col_present(c))
}

/// The significant / up / down split recomputed directly from a DE result
/// table at a fixed FDR threshold. `sig` is the number of rows whose adjusted
/// p-value is below `fdr`; `up`/`down` partition those by the sign of the
/// effect-size column. Rows lacking a finite significance value (NA-`padj`
/// independent-filtered rows) are dropped — never counted; rows that are
/// significant but lack a finite effect size count toward `sig` but neither
/// `up` nor `down`. Shared by [`verify_structured_counts`] (A4) so the
/// recompute is one canonical loop, not a re-derivation.
fn recompute_split(
    table_path: &Path,
    cfg: &ExtractorConfig,
    fdr: f64,
) -> Option<(usize, usize, usize)> {
    let cached = load_table_rows(table_path, &cfg.entity_columns).ok()?;
    recompute_split_in_table(&cached, cfg, fdr)
}

fn recompute_split_in_table(
    cached: &CachedTable,
    cfg: &ExtractorConfig,
    fdr: f64,
) -> Option<(usize, usize, usize)> {
    let sig_col = resolve_significance_column(cached, cfg)?;
    let sig_cols = [sig_col];
    let (mut sig, mut up, mut down) = (0usize, 0usize, 0usize);
    for row in &cached.rows {
        let Some(p) = lookup_numeric(&row.values, &sig_cols) else {
            continue;
        };
        if !(p.is_finite() && p < fdr) {
            continue;
        }
        sig += 1;
        if let Some(e) =
            lookup_numeric(&row.values, &cfg.effect_size_columns).filter(|v| v.is_finite())
        {
            if e > 0.0 {
                up += 1;
            } else if e < 0.0 {
                down += 1;
            }
        }
    }
    Some((sig, up, down))
}

/// A4 — recompute the structured up/down DE split from the cited result table
/// and emit a real `Mismatch` when `result.json`'s `n_up_fdr05`/`n_down_fdr05`
/// disagrees. Nothing previously recomputed these structured summary counts,
/// so an up/down split error (e.g. the result.json swapping or inflating the
/// directional counts) passed silently. This walks the package's
/// `de_results.tsv`, recomputes (sig, up, down) at FDR < 0.05 via the shared
/// [`recompute_split`], and compares against the agent-written summary.
///
/// Returns one [`ClaimVerdict`] per structured count that could be checked:
/// `Verified` when the recomputed value matches, `Mismatch` when it disagrees.
/// Returns an EMPTY vec (no false verdicts) when the de table or the summary
/// counts are absent — an honest abstention, not a pass.
pub fn verify_structured_counts(package_root: &Path, cfg: &ExtractorConfig) -> Vec<ClaimVerdict> {
    const FDR: f64 = 0.05;
    let mut out = Vec::new();

    // Locate the result.json summary counts.
    let result_json = package_root.join("result.json");
    let Ok(bytes) = std::fs::read(&result_json) else {
        return out;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return out;
    };
    let claimed_up = value.get("n_up_fdr05").and_then(|v| v.as_u64());
    let claimed_down = value.get("n_down_fdr05").and_then(|v| v.as_u64());
    if claimed_up.is_none() && claimed_down.is_none() {
        return out;
    }

    // Locate de_results.tsv and recompute the split.
    let Some(table_path) = resolve_evidence_table(package_root, "de_results.tsv") else {
        return out;
    };
    let Some((_sig, up, down)) = recompute_split(&table_path, cfg, FDR) else {
        return out;
    };
    let table_name = table_label(&table_path);

    let mk = |label: &str, claimed: u64, observed: usize| -> ClaimVerdict {
        let status = if claimed as usize == observed {
            ClaimStatus::Verified
        } else {
            ClaimStatus::Mismatch {
                detail: format!(
                    "structured count `{label}`: result.json says {claimed}, recompute from `{table_name}` (FDR<{FDR}) yields {observed}"
                ),
            }
        };
        ClaimVerdict {
            claim: Claim {
                entity: label.to_string(),
                direction: None,
                effect_size: None,
                pvalue: None,
                source_table: Some(table_name.clone()),
                excerpt: format!("structured summary count {label}"),
                contract: ClaimContract::ThresholdedDeOrEnrichment,
                literature_evidence: None,
                matched_pvalue_keyword: None,
                linear_fold: None,
                aggregate_kind: None,
                aggregate_column: None,
                aggregate_rowset: None,
                aggregate_value: None,
                collection: None,
                term: None,
                keyed_column: None,
                keyed_value: None,
            },
            status,
            strength: ClaimStrength::Exploratory,
            audit: None,
        }
    };

    if let Some(c) = claimed_up {
        out.push(mk("n_up_fdr05", c, up));
    }
    if let Some(c) = claimed_down {
        out.push(mk("n_down_fdr05", c, down));
    }
    out
}

/// A hedge / approximation token immediately preceding a count integer — the
/// VF-16 false-positive guard so "~2000 genes", "approximately 2,000", "at
/// least 5", ">1000" abstain rather than being checked against an EXACT
/// table recompute.
static HEDGE_BEFORE_COUNT_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)(?:~|≈|∼|about|approximately|approx|roughly|around|nearly|over|under|more than|fewer than|less than|up to|at least|at most|>=?|<=?)\s*$",
    )
    .expect("static regex")
});

static COUNT_OF_NOUN_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(\d[\d,]*)\s+of\s+(?:those\s+)?\d[\d,]*\s+(?:[A-Za-z][\w-]*\s+){0,5}?(?:gene[\s-]?sets?|donor\s+cell[\s-]?lines?|cell[\s-]?lines?|entities?|genes?|features?|pathways?|terms?|samples?)\b",
    )
    .expect("static regex")
});

static INPUT_DIMENSION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(\d[\d,]*)\s+genes?\s*[×x*]\s*(\d[\d,]*)\s+samples?\b")
        .expect("static regex")
});

static PREFILTER_SUMMARY_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:pre[\s-]?filter\s+)?removed\s+(\d[\d,]*)\s+of\s+(\d[\d,]*)\s+(?:input\s+)?genes?(?:\s*\([^)]{0,240}\))?\s*,\s+retaining\s+(\d[\d,]*)\b",
    )
    .expect("static regex")
});

static GENE_MAPPING_SUMMARY_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:of\s+)?(\d[\d,]*)\s+genes?(?:\s+in\s+[^,;]{1,160})?\s*,\s+(\d[\d,]*)\s+(?:were\s+)?mapped\b.{0,320}?[;,]\s*(\d[\d,]*)\s+(?:genes?\s+)?(?:remained\s+)?unmapped\b",
    )
    .expect("static regex")
});

static DUPLICATE_RANKING_SUMMARY_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\bafter\s+(?:removing|removal\s+of)\s+(\d[\d,]*)\s+duplicate(?:\s+gene[\s-]?symbol)?\s+labels?\s*,\s+(\d[\d,]*)\s+genes?\s+(?:were\s+)?ranked\b",
    )
    .expect("static regex")
});

static ESTIMABLE_GENE_SUMMARY_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(?:of\s+)?(\d[\d,]*)\s+genes?\s+in\s+(?:the\s+)?(?:filtered|tested|analysis[\s-]?ready)?\s*count\s+matrix\s*,\s+(\d[\d,]*)\s+(?:genes?\s+)?(?:had|have|with)\s+estimable\s+(?:adjusted\s+p[\s-]?values?|padj)\b",
    )
    .expect("static regex")
});

static RANKED_MAPPING_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(\d[\d,]*)\s+ranked\s+genes?.{0,160}?\bfrom\s+(\d[\d,]*)\s+tested\s+genes?\b",
    )
    .expect("static regex")
});

/// Closed semantic mapping from narrative wording to retained stage-summary
/// fields. The key selection is independent of the claimed number, preventing
/// coincidentally equal counts from being treated as evidence.
fn semantic_summary_fields(
    text: &str,
    noun: &str,
    has_up: bool,
    has_down: bool,
) -> Option<&'static [&'static str]> {
    let lower = text.to_ascii_lowercase();
    let normalized_lower = lower.replace(['-', '_'], " ");
    let noun = noun.to_ascii_lowercase().replace(['-', '_'], " ");
    let noun = noun.trim();

    if lower.contains("no usable") && (lower.contains("significance") || lower.contains("padj")) {
        return Some(&["top_k_missing_significance_count"]);
    }
    if normalized_lower.contains("not assessed")
        || normalized_lower.contains("not searched")
        || normalized_lower.contains("no prior work query")
    {
        return Some(&["n_entities_not_assessed"]);
    }
    if matches!(noun, "entity" | "entities")
        && (lower.contains("were searched")
            || lower.contains("was searched")
            || lower.contains("were queried")
            || lower.contains("was queried")
            || lower.contains("query was issued"))
    {
        return Some(&["n_entities_assessed"]);
    }
    if lower.contains("ranked") && matches!(noun, "gene" | "genes") {
        return Some(&["n_genes_ranked"]);
    }
    if noun.contains("donor cell line") {
        return Some(&["n_donors"]);
    }
    if matches!(noun, "sample" | "samples")
        && (lower.contains("input")
            || lower.contains("matrix")
            || lower.contains("librar")
            || lower.contains("assayed"))
    {
        return Some(&["n_samples"]);
    }
    if lower.contains("removed") && matches!(noun, "gene" | "genes") {
        return Some(&[
            "n_genes_prefilter_removed",
            "n_genes_removed",
            "n_genes_prefiltered",
        ]);
    }
    if matches!(noun, "gene" | "genes")
        && (lower.contains("input")
            || lower.contains("starting point")
            || lower.contains("matrix contained"))
    {
        return Some(&["n_genes_input", "n_genes"]);
    }
    if lower.contains("retained") && matches!(noun, "gene" | "genes") {
        return Some(&[
            "n_genes_retained",
            "tested_feature_count",
            "n_genes_in_matrix",
            "n_genes_tested",
        ]);
    }
    if lower.contains("tested") {
        if is_set_level_noun(noun) {
            return Some(&[
                "n_pathways_tested",
                "n_gene_sets_tested",
                "n_terms_tested",
                "n_sets_tested",
            ]);
        }
        if matches!(noun, "gene" | "genes" | "feature" | "features") {
            return Some(&["n_genes_tested", "tested_feature_count"]);
        }
    }
    if lower.contains("significant") {
        if is_set_level_noun(noun) {
            return None;
        }
        if has_up && !has_down {
            return Some(&["n_significant_up"]);
        }
        if has_down && !has_up {
            return Some(&["n_significant_down"]);
        }
        if matches!(
            noun,
            "gene" | "genes" | "feature" | "features" | "entity" | "entities"
        ) {
            return Some(&["n_significant"]);
        }
    }
    if lower.contains("no prior finding") {
        return Some(&["no_prior_finding"]);
    }
    None
}

/// Direction words elsewhere in a multi-fact sentence must not reclassify an
/// aggregate population count. Inspect only the matched "N ... noun" phrase
/// and its immediate following clause. This still recognizes forms such as
/// "2,208 genes associated with increased expression" while leaving "largest
/// positive effect among 4,030 significant genes" as an undirected universe
/// count.
fn aggregate_count_directions(
    text: &str,
    noun_match: &regex::Match<'_>,
    cfg: &ExtractorConfig,
) -> (bool, bool) {
    let tail = &text[noun_match.end()..];
    let punctuation_end = tail
        .char_indices()
        .find_map(|(offset, ch)| matches!(ch, '.' | ';' | ':' | '\n').then_some(offset))
        .unwrap_or(tail.len());
    let char_limit = tail
        .char_indices()
        .nth(120)
        .map(|(offset, _)| offset)
        .unwrap_or(tail.len());
    let suffix_end = punctuation_end.min(char_limit);
    let end = noun_match.end() + suffix_end;
    let local = text[noun_match.start()..end].to_ascii_lowercase();
    let has_up = cfg
        .up_words
        .iter()
        .any(|word| local.contains(&word.to_ascii_lowercase()));
    let has_down = cfg
        .down_words
        .iter()
        .any(|word| local.contains(&word.to_ascii_lowercase()));
    (has_up, has_down)
}

/// True for a round-number SUMMARY figure: ≥1000, a whole multiple of 100, with
/// fewer than 3 significant digits (2000, 12000 — but NOT 2209 or 12500). Such
/// figures read as rounded approximations, so VF-16 abstains rather than
/// exact-matching them against a recompute.
fn is_round_count(n: f64) -> bool {
    if !(n >= 1000.0) || n.fract() != 0.0 || (n % 100.0) != 0.0 {
        return false;
    }
    format!("{}", n as i64).trim_end_matches('0').len() < 3
}

/// VF-16 — verify aggregate COUNT claims in a narrative ("2209 genes were
/// upregulated at FDR < 0.05 (Table 1)"). Such sentences carry NO uppercase
/// entity, so they produce no per-claim `Claim` and bypass the per-entity
/// verifier entirely — an inflated/fabricated count escapes. This scan splits
/// the narrative with the SAME splitter as the extractor, and for each
/// count-shaped sentence recomputes the count from the cited result table, or
/// infers a support link when an emitted result table exactly reproduces an
/// uncited thresholded count.
///
/// ABSTAIN-FIRST (false-positive safety is paramount — this runs over every
/// production narrative): an ordinary count is checked only when it is not
/// hedged, is not a round-number summary, and resolves to a table carrying the
/// named significance column. A tested/significant summary may include both
/// directional counts because all four values are recomputed together. Other
/// combined-direction prose still abstains. A non-matching uncited table never
/// produces a Mismatch.
pub fn verify_narrative_counts(
    narrative: &str,
    tables_root: &Path,
    package_root: &Path,
    cfg: &ExtractorConfig,
) -> Vec<ClaimVerdict> {
    let index = TableIndex::scan(tables_root);
    let mut out: Vec<ClaimVerdict> = Vec::new();
    let mut cache: BTreeMap<PathBuf, CachedTable> = BTreeMap::new();
    let narrative = crate::claim_extractor::strip_system_generated_blocks(narrative);
    let narrative = coalesce_wrapped_prose(&narrative);
    let mut candidate_paths = index.distinct_paths();
    candidate_paths.extend(discovery_candidate_tables(package_root));
    candidate_paths.sort_by(|left, right| {
        let priority = |path: &Path| match path.file_name().and_then(|name| name.to_str()) {
            Some("de_results.tsv" | "pathway_results.tsv" | "ranked_genes.tsv") => 0,
            Some(name) if name.contains("results") => 1,
            _ => 2,
        };
        priority(left)
            .cmp(&priority(right))
            .then_with(|| left.cmp(right))
    });
    candidate_paths.dedup();

    // A directional/significant bullet often inherits its threshold from the
    // preceding paragraph. Retain the adjusted thresholds declared anywhere in
    // this report, then accept an inherited threshold only when a canonical
    // result table reproduces the exact count.
    let mut adjusted_thresholds: Vec<f64> = THRESH_RE
        .captures_iter(&narrative)
        .filter(|captures| {
            captures
                .get(1)
                .is_some_and(|keyword| is_adjusted_pvalue_keyword(keyword.as_str()))
        })
        .filter_map(|captures| captures.get(2)?.as_str().parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .collect();
    adjusted_thresholds.sort_by(f64::total_cmp);
    adjusted_thresholds.dedup_by(|left, right| left.total_cmp(right).is_eq());

    for sentence in crate::claim_extractor::split_sentences(&narrative) {
        let s = sentence.trim();
        // Skip markdown table rows (mined structurally elsewhere).
        let emphasized_heading =
            s.starts_with("**") && s.ends_with("**") && !s.contains(['.', '!', '?']);
        if s.is_empty()
            || crate::claim_extractor::is_markdown_heading(s)
            || emphasized_heading
            || s.starts_with('|')
            || s.matches('|').count() >= 2
        {
            continue;
        }
        let Some(noun_caps) = COUNT_NOUN_RE.captures(s) else {
            continue;
        };
        let Some(count_match) = noun_caps.get(1) else {
            continue;
        };
        // Rust's regex engine has no look-around. Without this explicit
        // boundary check, `0.25 pathway-level adjusted p-value` is captured as
        // the integer count `25 pathways` because the dot creates a word
        // boundary before `25`. A significance threshold is not a population
        // count and must not generate a spurious Unverifiable verdict.
        let preceding_decimal = count_match.start() >= 2
            && s.as_bytes()[count_match.start() - 1] == b'.'
            && s.as_bytes()[count_match.start() - 2].is_ascii_digit();
        let following_decimal = count_match.end() + 1 < s.len()
            && s.as_bytes()[count_match.end()] == b'.'
            && s.as_bytes()[count_match.end() + 1].is_ascii_digit();
        if preceding_decimal || following_decimal {
            continue;
        }
        let noun = noun_caps.get(2).map(|m| m.as_str()).unwrap_or("items");
        let lower = s.to_lowercase();
        // Display cardinality is presentation metadata, not an analytical
        // aggregate claim.
        if lower.contains("table below") && (lower.contains("shows") || lower.contains("contains"))
        {
            continue;
        }
        if lower.contains("leading ") && lower.contains(" of ") {
            continue;
        }
        let (has_up, has_down) = noun_caps
            .get(0)
            .map(|matched| aggregate_count_directions(s, &matched, cfg))
            .unwrap_or((false, false));
        let dir = if has_up && !has_down {
            "up "
        } else if has_down && !has_up {
            "down "
        } else {
            ""
        };
        let entity = format!("count:{dir}{noun}");
        let make =
            |claim_entity: &str, status: ClaimStatus, source: Option<String>| -> ClaimVerdict {
                ClaimVerdict {
                    claim: Claim {
                        entity: claim_entity.to_string(),
                        direction: None,
                        effect_size: None,
                        pvalue: None,
                        source_table: source,
                        excerpt: s.to_string(),
                        contract: crate::claim_contract::ClaimContract::ThresholdedDeOrEnrichment,
                        literature_evidence: None,
                        matched_pvalue_keyword: None,
                        linear_fold: None,
                        aggregate_kind: None,
                        aggregate_column: None,
                        aggregate_rowset: None,
                        aggregate_value: None,
                        collection: None,
                        term: None,
                        keyed_column: None,
                        keyed_value: None,
                    },
                    status,
                    strength: ClaimStrength::Exploratory,
                    audit: None,
                }
            };
        let unverifiable = |reason: &str| {
            make(
                &entity,
                ClaimStatus::Unverifiable {
                    reason: reason.to_string(),
                },
                None,
            )
        };
        let summary_fact = |claim_entity: &str, claimed: f64, fields: &[&str]| -> ClaimVerdict {
            let Some((path, field, observed)) = resolve_summary_count(package_root, fields) else {
                return make(
                    claim_entity,
                    ClaimStatus::Unverifiable {
                        reason: format!(
                            "no unambiguous retained stage-summary field matched `{}`",
                            fields.join("` or `")
                        ),
                    },
                    None,
                );
            };
            make(
                claim_entity,
                compare_exact_count(
                    claimed,
                    observed,
                    &path,
                    &format!("stage-summary field `{field}`"),
                ),
                Some(package_relative_label(&path, package_root)),
            )
        };

        // Preserve every independently checkable axis in compact summary
        // sentences. The generic noun matcher retains only the first
        // "N noun" pair and would otherwise lose the sample dimension, the
        // removed/starting/retained split, or the pre-/post-mapping pair.
        if let Some(captures) = INPUT_DIMENSION_RE.captures(s) {
            if let (Some(genes), Some(samples)) = (
                captures
                    .get(1)
                    .and_then(|value| parse_count(value.as_str())),
                captures
                    .get(2)
                    .and_then(|value| parse_count(value.as_str())),
            ) {
                out.push(summary_fact(
                    "count:input genes",
                    genes,
                    &["n_genes_input", "n_genes"],
                ));
                out.push(summary_fact("count:input samples", samples, &["n_samples"]));
                continue;
            }
        }
        if let Some(captures) = PREFILTER_SUMMARY_RE.captures(s) {
            if let (Some(removed), Some(input), Some(retained)) = (
                captures
                    .get(1)
                    .and_then(|value| parse_count(value.as_str())),
                captures
                    .get(2)
                    .and_then(|value| parse_count(value.as_str())),
                captures
                    .get(3)
                    .and_then(|value| parse_count(value.as_str())),
            ) {
                out.push(summary_fact(
                    "count:prefiltered genes",
                    removed,
                    &[
                        "n_genes_prefilter_removed",
                        "n_genes_removed",
                        "n_genes_prefiltered",
                    ],
                ));
                out.push(summary_fact(
                    "count:input genes",
                    input,
                    &[
                        "n_genes_pre_filter",
                        "n_features_pre_filter",
                        "n_genes_input",
                        "n_genes",
                    ],
                ));
                out.push(summary_fact(
                    "count:tested genes",
                    retained,
                    &[
                        "n_genes_retained",
                        "tested_feature_count",
                        "n_genes_in_matrix",
                        "n_genes_tested",
                    ],
                ));
                continue;
            }
        }
        if let Some(captures) = GENE_MAPPING_SUMMARY_RE.captures(s) {
            if let (Some(pre_mapping), Some(mapped), Some(unmapped)) = (
                captures
                    .get(1)
                    .and_then(|value| parse_count(value.as_str())),
                captures
                    .get(2)
                    .and_then(|value| parse_count(value.as_str())),
                captures
                    .get(3)
                    .and_then(|value| parse_count(value.as_str())),
            ) {
                out.push(summary_fact(
                    "count:pre-mapping genes",
                    pre_mapping,
                    &["n_genes_pre_mapping"],
                ));
                out.push(summary_fact(
                    "count:mapped genes",
                    mapped,
                    &["n_genes_mapped"],
                ));
                out.push(summary_fact(
                    "count:unmapped genes",
                    unmapped,
                    &["n_genes_unmapped"],
                ));
                continue;
            }
        }
        if let Some(captures) = DUPLICATE_RANKING_SUMMARY_RE.captures(s) {
            if let (Some(duplicates), Some(ranked)) = (
                captures
                    .get(1)
                    .and_then(|value| parse_count(value.as_str())),
                captures
                    .get(2)
                    .and_then(|value| parse_count(value.as_str())),
            ) {
                out.push(summary_fact(
                    "count:duplicate gene labels removed",
                    duplicates,
                    &["n_duplicate_gene_labels_removed"],
                ));
                out.push(summary_fact(
                    "count:ranked genes",
                    ranked,
                    &["n_genes_ranked"],
                ));
                continue;
            }
        }
        if let Some(captures) = ESTIMABLE_GENE_SUMMARY_RE.captures(s) {
            if let (Some(in_matrix), Some(estimable)) = (
                captures
                    .get(1)
                    .and_then(|value| parse_count(value.as_str())),
                captures
                    .get(2)
                    .and_then(|value| parse_count(value.as_str())),
            ) {
                out.push(summary_fact(
                    "count:genes in filtered matrix",
                    in_matrix,
                    &["n_genes_in_matrix", "tested_feature_count"],
                ));
                out.push(summary_fact(
                    "count:genes with estimable adjusted p-values",
                    estimable,
                    &["n_genes_tested"],
                ));
                continue;
            }
        }
        if let Some(captures) = RANKED_MAPPING_RE.captures(s) {
            if let (Some(ranked), Some(tested)) = (
                captures
                    .get(1)
                    .and_then(|value| parse_count(value.as_str())),
                captures
                    .get(2)
                    .and_then(|value| parse_count(value.as_str())),
            ) {
                out.push(summary_fact(
                    "count:ranked genes",
                    ranked,
                    &["n_genes_ranked"],
                ));
                out.push(summary_fact(
                    "count:tested genes",
                    tested,
                    &["n_genes_tested", "tested_feature_count"],
                ));
                continue;
            }
        }

        if let Some(summary) = parse_tested_summary(s) {
            let threshold = THRESH_RE
                .captures(s)
                .and_then(|captures| captures.get(2))
                .and_then(|value| value.as_str().parse::<f64>().ok());
            if let Some(threshold) = threshold {
                let observe = |path: &Path,
                               cache: &mut BTreeMap<PathBuf, CachedTable>|
                 -> Option<(usize, usize, usize, usize)> {
                    let cached = ensure_count_cached(cache, path, cfg, noun)?;
                    let (significant, up, down) = recompute_split_in_table(cached, cfg, threshold)?;
                    Some((cached.rows.len(), significant, up, down))
                };
                let agrees = |observed: (usize, usize, usize, usize)| {
                    observed.0 == summary.total
                        && observed.1 == summary.significant
                        && summary.up.is_none_or(|value| value == observed.2)
                        && summary.down.is_none_or(|value| value == observed.3)
                };

                let resolved = if let Some(src) = crate::claim_extractor::scan_table_reference(s) {
                    index.resolve(&src).map(Path::to_path_buf)
                } else {
                    candidate_paths
                        .iter()
                        .find(|path| observe(path, &mut cache).map(&agrees).unwrap_or(false))
                        .cloned()
                };

                if let Some(path) = resolved {
                    if let Some((total, significant, up, down)) = observe(&path, &mut cache) {
                        let source = package_relative_label(&path, package_root);
                        let comparisons = [
                            (format!("count:tested {noun}"), summary.total, total),
                            (
                                format!("count:significant {noun}"),
                                summary.significant,
                                significant,
                            ),
                        ];
                        for (claim_entity, claimed, observed) in comparisons {
                            out.push(make(
                                &claim_entity,
                                compare_count(
                                    claimed as f64,
                                    observed,
                                    &path,
                                    "tested/significant summary",
                                ),
                                Some(source.clone()),
                            ));
                        }
                        for (direction, claimed, observed) in
                            [("up", summary.up, up), ("down", summary.down, down)]
                        {
                            if let Some(claimed) = claimed {
                                out.push(make(
                                    &format!("count:{direction} {noun}"),
                                    compare_count(
                                        claimed as f64,
                                        observed,
                                        &path,
                                        "directional significant summary",
                                    ),
                                    Some(source.clone()),
                                ));
                            }
                        }
                        continue;
                    }
                }
            }
        }

        // FP guard 1 — hedged/approximate count.
        let int_start = noun_caps.get(1).map(|m| m.start()).unwrap_or(0);
        if HEDGE_BEFORE_COUNT_RE.is_match(&s[..int_start]) {
            out.push(unverifiable(
                "approximate/hedged count — not adjudicable against an exact recompute",
            ));
            continue;
        }
        // FP guard 2 — round-number summary figure.
        if noun_caps
            .get(1)
            .and_then(|m| parse_count(m.as_str()))
            .is_some_and(is_round_count)
        {
            out.push(unverifiable(
                "round-number summary count — treated as approximate",
            ));
            continue;
        }
        // FP guard 3 — combined up+down in one sentence (directions not separable).
        if has_up && has_down {
            out.push(unverifiable(
                "combined up/down counts in one sentence — directions not separable",
            ));
            continue;
        }

        // Threshold-less operational counts are checked against specifically
        // named stage-summary fields. "X of Y" wording reports X as the
        // asserted subset (for example, 4,024 of 4,030 were not assessed), not
        // the denominator immediately preceding the noun.
        if !THRESH_RE.is_match(s) {
            if let Some(fields) = semantic_summary_fields(s, noun, has_up, has_down) {
                let row_recompute_preferred = fields.first().is_some_and(|field| {
                    matches!(
                        *field,
                        "n_significant" | "n_significant_up" | "n_significant_down"
                    )
                }) && !adjusted_thresholds.is_empty();
                if !row_recompute_preferred {
                    let claimed = COUNT_OF_NOUN_RE
                        .captures(s)
                        .and_then(|captures| captures.get(1))
                        .or_else(|| noun_caps.get(1))
                        .and_then(|value| parse_count(value.as_str()));
                    if let Some(claimed) = claimed {
                        if let Some((path, field, observed)) =
                            resolve_summary_count(package_root, fields)
                        {
                            out.push(make(
                                &entity,
                                compare_exact_count(
                                    claimed,
                                    observed,
                                    &path,
                                    &format!("stage-summary field `{field}`"),
                                ),
                                Some(package_relative_label(&path, package_root)),
                            ));
                            continue;
                        }
                        if !is_grouping_noun(noun) {
                            out.push(make(
                                &entity,
                                ClaimStatus::Unverifiable {
                                    reason: format!(
                                        "no unambiguous retained stage-summary field matched `{}`",
                                        fields.join("` or `")
                                    ),
                                },
                                None,
                            ));
                            continue;
                        }
                        // A cohort/assignment table can still substantiate a
                        // grouping count by distinct labels. Fall through to
                        // the table resolver instead of requiring a redundant
                        // stage-summary field.
                    }
                }
            }
        }

        if noun_caps
            .get(0)
            .is_some_and(|matched| matched.as_str().to_ascii_lowercase().contains("top"))
        {
            // "top-15 genes" states a display/rank window, not an analytical
            // population count. A preceding subset count such as "0 of those
            // 15 top features carry no usable padj" is handled by the semantic
            // stage-summary branch above.
            continue;
        }

        // A list bullet may state only the directional/significant count while
        // the immediately surrounding report declares the threshold. Infer
        // that evidence link only on a positive exact reproduction; a
        // non-matching table never becomes a Mismatch.
        let threshold_in_sentence = THRESH_RE.is_match(s);
        let gene_level_noun = {
            let normalized = noun.to_ascii_lowercase().replace(['-', '_'], " ");
            matches!(
                normalized.trim(),
                "gene"
                    | "genes"
                    | "feature"
                    | "features"
                    | "transcript"
                    | "transcripts"
                    | "protein"
                    | "proteins"
                    | "probe"
                    | "probes"
                    | "site"
                    | "sites"
                    | "region"
                    | "regions"
            )
        };
        if !threshold_in_sentence
            && gene_level_noun
            && ((has_up ^ has_down) || lower.contains("significant"))
        {
            let claimed = noun_caps
                .get(1)
                .and_then(|matched| parse_count(matched.as_str()))
                .map(|value| value as usize);
            let mut inferred = None;
            if let Some(claimed) = claimed {
                'candidate: for path in &candidate_paths {
                    let Some(cached) = ensure_count_cached(&mut cache, path, cfg, noun) else {
                        continue;
                    };
                    for threshold in &adjusted_thresholds {
                        let Some((significant, up, down)) =
                            recompute_split_in_table(cached, cfg, *threshold)
                        else {
                            continue;
                        };
                        let observed = if has_up && !has_down {
                            up
                        } else if has_down && !has_up {
                            down
                        } else {
                            significant
                        };
                        if claimed == observed {
                            inferred = Some(path.clone());
                            break 'candidate;
                        }
                    }
                }
            }
            if let Some(path) = inferred {
                out.push(make(
                    &entity,
                    ClaimStatus::Verified,
                    Some(package_relative_label(&path, package_root)),
                ));
                continue;
            }
        }

        // Resolve a cited table before considering evidence discovery.
        if let Some(src) = crate::claim_extractor::scan_table_reference(s) {
            let Some(path) = index.resolve(&src) else {
                out.push(unverifiable(
                    "cited table for the aggregate count did not resolve",
                ));
                continue;
            };
            let path = path.to_path_buf();
            let status = ensure_count_cached(&mut cache, &path, cfg, noun)
                .and_then(|cached| verify_count_claim_in_table(s, &path, cfg, cached));
            match status {
                Some(status) => out.push(make(&entity, status, Some(table_label(&path)))),
                None => out.push(unverifiable(
                    "cited table lacks the named significance column or the count is not recomputable",
                )),
            }
            continue;
        }

        // Infer an evidence link only when an emitted result table positively
        // reproduces the exact thresholded count. A non-matching uncited table
        // never creates a Mismatch.
        let mut inferred = None;
        for path in &candidate_paths {
            let verified = ensure_count_cached(&mut cache, &path, cfg, noun)
                .and_then(|cached| verify_count_claim_in_table(s, &path, cfg, cached))
                .is_some_and(|status| matches!(status, ClaimStatus::Verified));
            if verified {
                inferred = Some(path.clone());
                break;
            }
        }
        if let Some(path) = inferred {
            out.push(make(
                &entity,
                ClaimStatus::Verified,
                Some(package_relative_label(&path, package_root)),
            ));
        } else {
            out.push(unverifiable(
                "no table cited and no emitted result table reproduced the aggregate count",
            ));
        }
    }
    out
}

/// §3.6 narrative↔domain cross-check (aggregate-N half).
///
/// Verifies a narrative's reported aggregate count against a result table on
/// TWO axes: (1) the count matches what the table actually contains — reusing
/// the table-recompute in [`verify_count_claim`]; and (2) the count falls
/// inside a domain-plausible `[reference_min, reference_max]` band (e.g. "DEGs
/// for a 20k-gene human RNA-seq" should not be 0 nor 19,999).
///
/// Returns:
/// * `Unverifiable` when `text` is not count-shaped (no recompute possible).
/// * `Mismatch` when the count contradicts the table OR sits outside the band.
/// * `Verified` only when both axes agree.
pub fn verify_aggregate_n_in_range(
    text: &str,
    table_path: &Path,
    cfg: &ExtractorConfig,
    reference_min: f64,
    reference_max: f64,
) -> ClaimStatus {
    // Axis 1: table concordance via the existing recompute path.
    let table_status = match verify_count_claim(text, table_path, cfg) {
        Some(s) => s,
        None => {
            return ClaimStatus::Unverifiable {
                reason: "claim is not aggregate-count-shaped — no N to range-check".into(),
            }
        }
    };
    if matches!(table_status, ClaimStatus::Mismatch { .. }) {
        return table_status;
    }

    // Axis 2: domain plausibility of the claimed N. Reparse the leading count.
    let claimed_n = match COUNT_NOUN_RE
        .captures(text)
        .and_then(|c| c.get(1).map(|m| m.as_str().to_string()))
        .and_then(|raw| parse_count(&raw))
    {
        Some(n) => n,
        None => {
            return ClaimStatus::Unverifiable {
                reason: "could not parse a numeric N for domain range check".into(),
            }
        }
    };
    if claimed_n < reference_min || claimed_n > reference_max {
        return ClaimStatus::Mismatch {
            detail: format!(
                "aggregate N {} is outside the domain-plausible range [{}, {}]",
                claimed_n as i64, reference_min as i64, reference_max as i64
            ),
        };
    }
    ClaimStatus::Verified
}

/// §3.6 narrative↔domain cross-check (filter-threshold half).
///
/// A narrative reporting a *filtered* artifact (e.g. a "significant DE table")
/// must declare its filter threshold ("padj < 0.05"); the cited artifact must
/// then contain no row that violates it. Reuses the `THRESH_RE` parse and the
/// per-row `lookup_numeric` probe.
///
/// Returns:
/// * `Unverifiable` when the narrative states no threshold (the SME never
///   declared a cut to enforce) or the table carries no matching p-value column.
/// * `Mismatch` when at least one row in the artifact violates the stated cut.
/// * `Verified` when every row with the named p-value honors the threshold.
pub fn verify_narrative_threshold_honored(
    text: &str,
    table_path: &Path,
    cfg: &ExtractorConfig,
) -> ClaimStatus {
    let Some(caps) = THRESH_RE.captures(text) else {
        return ClaimStatus::Unverifiable {
            reason: "narrative states no filter threshold to enforce against the artifact".into(),
        };
    };
    let threshold_kw = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let threshold: f64 = match caps.get(2).and_then(|m| m.as_str().parse().ok()) {
        Some(t) => t,
        None => {
            return ClaimStatus::Unverifiable {
                reason: "could not parse the stated threshold value".into(),
            }
        }
    };

    let cached = match load_table_rows(table_path, &cfg.entity_columns) {
        Ok(t) => t,
        Err(e) => {
            return ClaimStatus::Unverifiable {
                reason: format!("artifact `{}` unreadable: {:#}", table_label(table_path), e),
            }
        }
    };

    // Probe against the p-value class the narrative names (adjusted vs raw),
    // ordering the configured columns so the named class wins.
    let want_adjusted = is_adjusted_pvalue_keyword(threshold_kw);
    let (adjusted_cols, raw_cols): (Vec<String>, Vec<String>) = cfg
        .pvalue_columns
        .iter()
        .cloned()
        .partition(|c| is_adjusted_pvalue_keyword(c));
    let ordered_cols: Vec<String> = if want_adjusted {
        adjusted_cols.into_iter().chain(raw_cols).collect()
    } else {
        raw_cols.into_iter().chain(adjusted_cols).collect()
    };
    // Pin a single significance column (see `verify_count_claim`): a per-row
    // fall-through to raw `pvalue` for NA-`padj` rows would mis-enforce an
    // adjusted-threshold claim against independent-filtered genes.
    let col_present = |col: &str| -> bool {
        let needle = normalize(col);
        cached
            .rows
            .first()
            .is_some_and(|r| r.values.contains_key(&needle))
    };
    let probe_cols: Vec<String> = ordered_cols
        .iter()
        .find(|c| col_present(c))
        .cloned()
        .into_iter()
        .collect();

    let mut probed_any = false;
    for row in &cached.rows {
        if let Some(p) = lookup_numeric(&row.values, &probe_cols) {
            probed_any = true;
            if p.is_finite() && p >= threshold {
                return ClaimStatus::Mismatch {
                    detail: format!(
                        "artifact `{}` row `{}` has {} {:.4e} >= stated threshold {:.4e}",
                        table_label(table_path),
                        row.entity,
                        threshold_kw,
                        p,
                        threshold
                    ),
                };
            }
        }
    }
    if !probed_any {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "artifact `{}` has no `{}` column to enforce the stated threshold",
                table_label(table_path),
                threshold_kw
            ),
        };
    }
    ClaimStatus::Verified
}

/// Compare an unhedged integer claim against a recomputed integer count.
///
/// NA and tie handling belong in the recomputation, not in a tolerance around
/// its output. Once the narrative states an exact integer, only that integer
/// verifies.
fn compare_count(claimed_n: f64, observed: usize, table_path: &Path, what: &str) -> ClaimStatus {
    if claimed_n == observed as f64 {
        ClaimStatus::Verified
    } else {
        ClaimStatus::Mismatch {
            detail: format!(
                "count claim: narrative says {}, `{}` has {} ({})",
                claimed_n as i64,
                table_label(table_path),
                observed,
                what
            ),
        }
    }
}

fn compare_exact_count(
    claimed_n: f64,
    observed: usize,
    source_path: &Path,
    what: &str,
) -> ClaimStatus {
    if claimed_n == observed as f64 {
        ClaimStatus::Verified
    } else {
        ClaimStatus::Mismatch {
            detail: format!(
                "count claim: narrative says {}, `{}` has {} ({})",
                claimed_n as i64,
                table_label(source_path),
                observed,
                what
            ),
        }
    }
}

/// True for nouns that denote a *grouping* whose count is the number of
/// distinct labels (cluster ids, cell types, modules, taxa), as opposed
/// to a per-row entity (gene, peak) whose count needs a threshold.
/// True for nouns denoting a gene SET / pathway / term — the units of an
/// enrichment (GSEA/ORA) result. For these, "enriched"/"depleted" describe
/// SIGNIFICANCE (the set is enriched at either tail of the ranked list), not a
/// gene-level up/down direction, so an aggregate-count claim over them must NOT
/// apply the effect-sign (NES) filter. (FP-A)
fn is_set_level_noun(noun: &str) -> bool {
    let n = noun.replace(['-', '_'], " ");
    matches!(
        n.trim(),
        "gene set"
            | "gene sets"
            | "geneset"
            | "genesets"
            | "pathway"
            | "pathways"
            | "term"
            | "terms"
            | "gene ontology term"
            | "gene ontology terms"
            | "signature"
            | "signatures"
    )
}

fn is_grouping_noun(noun: &str) -> bool {
    let n = noun.replace(['-', '_'], " ");
    let n = n.trim();
    matches!(
        n,
        "cluster"
            | "clusters"
            | "donor cell line"
            | "donor cell lines"
            | "cell line"
            | "cell lines"
            | "cell type"
            | "cell types"
            | "celltype"
            | "celltypes"
            | "module"
            | "modules"
            | "component"
            | "components"
            | "domain"
            | "domains"
            | "community"
            | "communities"
            | "program"
            | "programs"
            | "taxon"
            | "taxa"
            | "otu"
            | "otus"
            | "asv"
            | "asvs"
            | "species"
            | "genus"
            | "genera"
            | "family"
            | "families"
            | "phylum"
            | "phyla"
            | "lineage"
            | "lineages"
            | "subtype"
            | "subtypes"
            | "sample"
            | "samples"
    )
}

/// Find the table column holding a grouping noun's labels: a header
/// containing the noun's stem or a generic grouping token. Iterates the
/// row's (BTreeMap-ordered) keys for determinism.
fn grouping_column(cached: &CachedTable, noun: &str) -> Option<String> {
    let row = cached.rows.first()?;
    let stem = noun.trim_end_matches('s').replace(['-', ' '], "_");
    let tokens = [
        stem.as_str(),
        "cluster",
        "donor_cell_line",
        "cell_line",
        "celltype",
        "cell_type",
        "sample",
        "sample_id",
        "type",
        "label",
        "module",
        "component",
        "domain",
        "community",
        "program",
        "taxon",
        "otu",
        "asv",
        "species",
        "genus",
        "family",
        "phylum",
        "lineage",
        "subtype",
        "assignment",
    ];
    row.values
        .keys()
        .find(|k| tokens.iter().any(|t| !t.is_empty() && k.contains(t)))
        .cloned()
}

/// Aggregate grouping tables (cohort manifests, cluster assignments) often do
/// not carry a gene/pathway identifier from `entityColumns`. Load them with a
/// closed set of grouping identifiers only for a grouping-count claim. This
/// does not broaden ordinary entity discovery, where treating the first
/// arbitrary column as an entity would create false matches.
fn ensure_count_cached<'c>(
    cache: &'c mut BTreeMap<PathBuf, CachedTable>,
    path: &Path,
    cfg: &ExtractorConfig,
    noun: &str,
) -> Option<&'c CachedTable> {
    if !cache.contains_key(path) {
        let mut entity_columns = cfg.entity_columns.clone();
        if is_grouping_noun(noun) {
            entity_columns.extend(
                [
                    "donor_cell_line",
                    "cell_line",
                    "cluster",
                    "cell_type",
                    "celltype",
                    "sample",
                    "sample_id",
                    "module",
                    "component",
                    "domain",
                    "community",
                    "program",
                    "taxon",
                    "otu",
                    "asv",
                    "species",
                    "genus",
                    "family",
                    "phylum",
                    "lineage",
                    "subtype",
                    "assignment",
                ]
                .into_iter()
                .map(str::to_string),
            );
        }
        let table = load_table_rows(path, &entity_columns).ok()?;
        cache.insert(path.to_path_buf(), table);
    }
    cache.get(path)
}

/// Verify a single structured claim.
fn verify_one_structured(
    sc: &StructuredClaim,
    package_root: &Path,
    cfg: &ExtractorConfig,
) -> ClaimVerdict {
    let excerpt = sc.claim.clone();
    let make = |entity: String, status: ClaimStatus, source_table: Option<String>| ClaimVerdict {
        claim: Claim {
            entity,
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table,
            excerpt: excerpt.clone(),
            contract: crate::claim_contract::ClaimContract::ThresholdedDeOrEnrichment,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        },
        status,
        strength: ClaimStrength::Exploratory,
        audit: None,
    };

    let Some(evidence) = sc.evidence.as_deref().filter(|e| !e.trim().is_empty()) else {
        return make(
            summarize_claim_subject(&sc.claim),
            // Never adjudicated: there is no evidence file to load, so no
            // adjudication ran. Pending, not Unverifiable.
            ClaimStatus::Pending {
                reason: "claim cites no evidence file".into(),
            },
            None,
        );
    };
    // FP-B: a `file::json-pointer` evidence reference (e.g.
    // `result.json::top_effect_abundance_ratio`) is a SELF-reference to a JSON
    // FIELD, not a result table. The `::pointer` form must NOT be treated as a
    // phantom filename. Strip the pointer; if the base file exists in the
    // package it is a legitimate (if not table-adjudicable) self-citation →
    // Unverifiable, NOT a phantom-file Mismatch. Only a base file that is itself
    // absent everywhere is a genuine fabricated citation.
    if let Some((base, _pointer)) = evidence.split_once("::") {
        let base = base.trim();
        let status = if !base.is_empty() && evidence_basename_exists(package_root, base) {
            // Never adjudicated: an in-result field self-reference is not a
            // result table, so no adjudication site ran. Pending.
            ClaimStatus::Pending {
                reason: format!(
                    "cited evidence `{evidence}` is an in-result field self-reference (not a result table); value not table-adjudicable"
                ),
            }
        } else {
            ClaimStatus::Mismatch {
                detail: format!(
                    "claim cites evidence `{evidence}` whose base file `{base}` does not exist anywhere in the package"
                ),
            }
        };
        return make(summarize_claim_subject(&sc.claim), status, None);
    }
    let Some(table_path) = resolve_evidence_table(package_root, evidence) else {
        let basename = Path::new(evidence)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(evidence)
            .to_string();
        // VF-1 — a structured claim citing an evidence file that exists NOWHERE
        // in the package is an unambiguous fabricated citation → Mismatch. If
        // the basename IS present somewhere the resolver did not scan, that is a
        // resolution gap, not a fabrication → stay Unverifiable (no false flag).
        let status = if evidence_basename_exists(package_root, evidence) {
            // Never adjudicated: a resolution gap (the basename exists somewhere
            // the resolver did not scan), so no table was loaded. Pending — an
            // honest claim about an unresolvable-but-present citation, not a
            // checked-but-undeterminable Unverifiable.
            ClaimStatus::Pending {
                reason: format!(
                    "cited evidence `{}` is present in the package but not at a resolvable result-table location",
                    evidence
                ),
            }
        } else {
            ClaimStatus::Mismatch {
                detail: format!(
                    "claim cites evidence file `{}` that does not exist anywhere in the package",
                    evidence
                ),
            }
        };
        return make(summarize_claim_subject(&sc.claim), status, Some(basename));
    };
    let table_name = table_label(&table_path);

    // 1. Aggregate count claim — recompute from the table.
    if let Some(status) = verify_count_claim(&sc.claim, &table_path, cfg) {
        return make(summarize_claim_subject(&sc.claim), status, Some(table_name));
    }

    // 2. Per-entity claim: extract entity/direction/effect/pvalue from the
    //    claim text and check it against the cited table. The evidence
    //    path supplies the source_table the prose lacks.
    let extracted = crate::claim_extractor::extract_claims(&sc.claim, cfg);
    if let Some(mut claim) = extracted
        .into_iter()
        .find(|c| c.direction.is_some() || c.effect_size.is_some() || c.pvalue.is_some())
    {
        claim.source_table = Some(table_name.clone());
        let index = TableIndex::single(&table_path);
        let mut cache: BTreeMap<PathBuf, CachedTable> = BTreeMap::new();
        let (status, audit) = verify_for_contract_audited(&claim, &index, cfg, &mut cache);
        return ClaimVerdict {
            claim,
            status,
            strength: ClaimStrength::Exploratory,
            audit,
        };
    }

    // 3. Nothing numeric/countable to check (e.g. a methodological note).
    //    Never adjudicated: the claim carries no countable/per-entity quantity,
    //    so no adjudication site ran. Pending, not Unverifiable.
    make(
        summarize_claim_subject(&sc.claim),
        ClaimStatus::Pending {
            reason: "no countable or per-entity quantity in claim to cross-check".into(),
        },
        Some(table_name),
    )
}

/// Short, SME-safe subject label for a structured claim verdict row:
/// the claim's leading clause, truncated, with surrounding whitespace
/// collapsed. Keeps the Claims-tab row readable without dumping the full
/// sentence into the `entity` slot.
fn summarize_claim_subject(claim: &str) -> String {
    let head = claim
        .split([';', '.', '('])
        .next()
        .unwrap_or(claim)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if head.chars().count() > 80 {
        let truncated: String = head.chars().take(77).collect();
        format!("{}…", truncated)
    } else {
        head
    }
}

/// Verify a task's structured claims against their cited evidence tables.
pub fn verify_structured_claims(
    claims: &[StructuredClaim],
    package_root: &Path,
    cfg: &ExtractorConfig,
) -> Vec<ClaimVerdict> {
    claims
        .iter()
        .map(|sc| verify_one_structured(sc, package_root, cfg))
        .collect()
}

/// Candidate result tables for prose-claim discovery: every `.tsv`/`.csv`
/// directly under `results/tables/` and one level under each
/// `runtime/outputs/<task>/`, sorted for determinism.
fn discovery_candidate_tables(package_root: &Path) -> Vec<PathBuf> {
    fn push_tables(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_file() {
                    if let Some(ext) = p.extension().and_then(|s| s.to_str()) {
                        let ext = ext.to_ascii_lowercase();
                        if ext == "tsv" || ext == "csv" {
                            out.push(p);
                        }
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    push_tables(&package_root.join("results").join("tables"), &mut out);
    let outputs = package_root.join("runtime").join("outputs");
    if let Ok(rd) = std::fs::read_dir(&outputs) {
        let mut tasks: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        tasks.sort();
        for t in &tasks {
            push_tables(t, &mut out);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Canonical stage summaries that can substantiate an aggregate narrative
/// count when the report does not cite a row-level table. Only `result.json`
/// and explicitly named summary JSON files are eligible. This keeps provenance
/// discovery away from arbitrary JSON numbers such as seeds, versions, or plot
/// dimensions.
fn discovery_candidate_summaries(package_root: &Path) -> Vec<PathBuf> {
    fn push_summaries(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            if name == "result.json" || name.ends_with("_summary.json") {
                out.push(path);
            }
        }
    }

    let mut out = Vec::new();
    let root_result = package_root.join("result.json");
    if root_result.is_file() {
        out.push(root_result);
    }
    let outputs = package_root.join("runtime").join("outputs");
    if let Ok(entries) = std::fs::read_dir(&outputs) {
        let mut tasks: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        tasks.sort();
        for task in tasks {
            push_summaries(&task, &mut out);
        }
    }
    out.sort_by(|left, right| {
        let priority = |path: &Path| {
            if path.file_name().and_then(|value| value.to_str()) == Some("result.json") {
                0
            } else {
                1
            }
        };
        priority(left)
            .cmp(&priority(right))
            .then_with(|| left.cmp(right))
    });
    out.dedup();
    out
}

fn collect_json_integer_fields(value: &serde_json::Value, wanted: &str, found: &mut Vec<usize>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(number) = map.get(wanted) {
                if let Some(value) = number
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                {
                    found.push(value);
                } else if let Some(value) = number
                    .as_i64()
                    .filter(|value| *value >= 0)
                    .and_then(|value| usize::try_from(value).ok())
                {
                    found.push(value);
                } else if let Some(value) = number
                    .as_f64()
                    .filter(|value| value.is_finite() && *value >= 0.0 && value.fract() == 0.0)
                {
                    found.push(value as usize);
                }
            }
            for child in map.values() {
                collect_json_integer_fields(child, wanted, found);
            }
        }
        serde_json::Value::Array(values) => {
            for child in values {
                collect_json_integer_fields(child, wanted, found);
            }
        }
        _ => {}
    }
}

/// Resolve a semantic count to a named field in retained stage summaries.
///
/// Field names come from a closed allow-list selected from the claim wording;
/// numeric equality alone never creates a link. If the same preferred field is
/// present with conflicting values, the resolver abstains instead of choosing
/// whichever file sorts first.
fn resolve_summary_count(
    package_root: &Path,
    field_names: &[&str],
) -> Option<(PathBuf, String, usize)> {
    let summaries = discovery_candidate_summaries(package_root);
    for field in field_names {
        let mut matches = Vec::new();
        for path in &summaries {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            let mut observed = Vec::new();
            collect_json_integer_fields(&value, field, &mut observed);
            for value in observed {
                matches.push((path.clone(), value));
            }
        }
        if matches.is_empty() {
            continue;
        }
        let first = matches[0].1;
        if matches.iter().all(|(_, observed)| *observed == first) {
            return Some((matches.remove(0).0, (*field).to_string(), first));
        }
        return None;
    }
    None
}

/// Verify prose / markdown-table claims, discovering the backing table by
/// entity membership when the claim cites none.
///
/// A claim with an explicit `source_table` resolves against
/// `effective_root` exactly as before. A claim without one — e.g. a gene
/// named only in a narrative markdown row — is checked against the first
/// candidate result table (in deterministic sorted order) whose entity
/// column contains the entity, so it still cross-checks against the DE /
/// enrichment table the agent wrote under `runtime/outputs/`. First-match
/// ordering keeps the verdict deterministic when the agent wrote
/// near-duplicate tables (e.g. `de_results.tsv` + `de_table.tsv`).
pub fn verify_claims_with_discovery(
    claims: &[Claim],
    effective_root: &Path,
    package_root: &Path,
    cfg: &ExtractorConfig,
) -> Vec<ClaimVerdict> {
    let cited_index = TableIndex::scan(effective_root);
    let candidates = discovery_candidate_tables(package_root);
    let mut cache: BTreeMap<PathBuf, CachedTable> = BTreeMap::new();
    let mut unusable_candidates: BTreeSet<PathBuf> = BTreeSet::new();
    let mut verdicts = Vec::new();
    for claim in claims {
        // Literature-grounded claims are verified against the package's
        // claims_evidence_matrix.csv, never a numeric result table — route
        // them before the table-discovery branch.
        if claim.contract == ClaimContract::LiteratureGrounded {
            let base = verify_literature_grounded_at(claim, package_root, cfg);
            // CV-1/CV-2: cross-check any numeric quantity the literature claim
            // asserts against the discovered result tables. The literature
            // handler is numeric-blind; without this a concordance claim
            // carrying a wrong effect size / p-value verifies on its
            // literature-direction predicate alone.
            let gate_index = TableIndex::from_paths(&candidates);
            let (status, audit) = run_numeric_gate(base, claim, &gate_index, cfg, &mut cache);
            let mut out = claim.clone();
            // Record the matrix as the supporting evidence for an ADJUDICATED
            // verdict (Verified/Suspicious project `supported_by` from
            // `source_table`; the sink drops it for the non-adjudicated arms).
            // Without this a verified literature claim carries an empty
            // `supported_by`, breaking the C→V evidence link the audit-proof
            // `evidence_coverage`/`cross_graph_integrity` invariants rely on.
            if matches!(
                status,
                ClaimStatus::Verified | ClaimStatus::Suspicious { .. }
            ) {
                if let Some(matrix) = resolve_evidence_literature(package_root, "", &[], cfg) {
                    out.source_table = Some(package_relative_label(&matrix, package_root));
                }
            }
            verdicts.push(ClaimVerdict {
                claim: out,
                status,
                strength: ClaimStrength::Exploratory,
                audit,
            });
            continue;
        }
        if claim.source_table.is_some() {
            let (status, audit) = verify_for_contract_audited(claim, &cited_index, cfg, &mut cache);
            verdicts.push(ClaimVerdict {
                claim: claim.clone(),
                status,
                strength: ClaimStrength::Exploratory,
                audit,
            });
            continue;
        }
        // Discover the backing table by entity membership. The agent
        // often writes the same entity into several near-duplicate tables
        // (e.g. `de_results.tsv` + `de_table.tsv`) with rounding-level
        // differences, so checking only the first match risks a *false*
        // mismatch against a table the narrative wasn't derived from.
        // Verify against every containing table and let agreement win:
        // Verified if any matching table confirms the claim; Mismatch
        // only when a table is found but none confirm; Unverifiable when
        // no result table contains the entity at all.
        let needle = normalize(&claim.entity);
        let containing: Vec<PathBuf> = candidates
            .iter()
            .filter(|cand| {
                if unusable_candidates.contains(*cand) {
                    return false;
                }
                if !cache.contains_key(*cand) {
                    match load_table_rows(cand, &cfg.entity_columns) {
                        Ok(t) => {
                            cache.insert((*cand).clone(), t);
                        }
                        Err(e) => {
                            unusable_candidates.insert((*cand).clone());
                            tracing::warn!(
                                target: "ecaa::claim_verifier",
                                table = %cand.display(),
                                error = %e,
                                "table not usable for claim discovery (e.g. a non-result table with no configured entity column, such as method_landscape.csv, or a genuine parse/IO error — see `error`); excluding it"
                            );
                            return false;
                        }
                    }
                }
                cache
                    .get(*cand)
                    .map(|t| t.get_by_normalized(&needle).is_some())
                    .unwrap_or(false)
            })
            .cloned()
            .collect();

        let (claim_out, status, audit) = if containing.is_empty() {
            (
                claim.clone(),
                ClaimStatus::Unverifiable {
                    reason: format!("entity `{}` not found in any result table", claim.entity),
                },
                Some(VerdictAudit::class_only(
                    verdict_class_of(claim),
                    entity_opt(claim),
                )),
            )
        } else {
            let mut best: Option<ClaimStatus> = None;
            let mut best_audit: Option<VerdictAudit> = None;
            let mut chosen = claim.clone();
            for path in &containing {
                let mut c = claim.clone();
                // D1: record the table as a package-root-relative path (it
                // contains `/`), so the evidence reference projected into
                // `supported_by` points at the directory the table actually
                // lives in (e.g. data_acquisition/) rather than being
                // re-prefixed with the *claim's* own task id. `evidence_ref_for`
                // passes a path containing `/` through verbatim.
                c.source_table = Some(package_relative_label(path, package_root));
                let idx = TableIndex::single(path);
                let (status, cand_audit) = verify_for_contract_audited(&c, &idx, cfg, &mut cache);
                let verified = matches!(status, ClaimStatus::Verified);
                let prefer = match &best {
                    None => true,
                    // Verified beats everything; Mismatch beats Unverifiable.
                    Some(ClaimStatus::Verified) => false,
                    Some(ClaimStatus::Mismatch { .. }) => verified,
                    // Unverifiable and Pending rank identically here: a stronger
                    // Verified/Mismatch wins over either.
                    Some(ClaimStatus::Unverifiable { .. }) | Some(ClaimStatus::Pending { .. }) => {
                        verified || matches!(status, ClaimStatus::Mismatch { .. })
                    }
                    // Suspicious only arises for entities ABSENT from a table,
                    // which cannot happen on this containing-tables path; rank
                    // it like Unverifiable (a stronger Verified/Mismatch wins)
                    // for completeness.
                    Some(ClaimStatus::Suspicious { .. }) => {
                        verified || matches!(status, ClaimStatus::Mismatch { .. })
                    }
                };
                if prefer {
                    chosen = c;
                    best = Some(status);
                    best_audit = cand_audit;
                }
                if matches!(best, Some(ClaimStatus::Verified)) {
                    break;
                }
            }
            (chosen, best.expect("non-empty containing set"), best_audit)
        };
        verdicts.push(ClaimVerdict {
            claim: claim_out,
            status,
            strength: ClaimStrength::Exploratory,
            audit,
        });
    }
    demote_contradicted_missing_row_mismatches(&mut verdicts);
    verdicts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim_extractor::extract_claims;
    use crate::decision_log::{DecisionActor, DecisionRecord, DecisionType};
    use serde_json::json;
    use tempfile::tempdir;

    /// D1: a claim discovered against a table that physically lives under a
    /// DIFFERENT task directory must record that table as a package-relative
    /// path (so the projected `supported_by` points at the real directory),
    /// not a bare basename that would be re-prefixed with the claim's task id.
    /// D1: a claim discovered against a table that physically lives under a
    /// DIFFERENT task directory must record that table as a package-relative
    /// path, so the projected `supported_by` points at the real directory
    /// instead of being re-prefixed with the claim's own task id.
    #[test]
    fn discovered_source_table_is_package_relative_path() {
        let pkg = tempdir().unwrap();
        let acq = pkg
            .path()
            .join("runtime")
            .join("outputs")
            .join("data_acquisition");
        std::fs::create_dir_all(&acq).unwrap();
        // entity column `gene`, plus an effect-size slot so the row verifies.
        std::fs::write(
            acq.join("cohort_manifest.tsv"),
            "gene\tlog2FC\nCRISPLD2\t2.6\n",
        )
        .unwrap();

        let cfg = cfg_with_entity_cols(&["gene", "gene_id"]);
        // No cited source_table -> the claim goes through table discovery.
        let claim = Claim {
            entity: "CRISPLD2".into(),
            direction: Some(Direction::Up),
            effect_size: Some(2.6),
            pvalue: None,
            source_table: None,
            excerpt: "CRISPLD2 is present.".into(),
            contract: ClaimContract::NumericTableLookup,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let verdicts = verify_claims_with_discovery(&[claim], pkg.path(), pkg.path(), &cfg);
        let st = verdicts[0].claim.source_table.as_deref().unwrap_or("");
        assert_eq!(
            st, "runtime/outputs/data_acquisition/cohort_manifest.tsv",
            "discovered source_table must be the package-relative path to the real dir, got {st:?}"
        );
    }

    #[test]
    fn demote_claims_skips_non_confirmatory_sessions() {
        let mut report = ClaimVerificationReport::empty();
        report.verdicts.push(ClaimVerdict {
            claim: Claim {
                entity: "TNF".into(),
                direction: None,
                effect_size: None,
                pvalue: None,
                source_table: Some("primary_endpoint.tsv".into()),
                excerpt: "TNF is upregulated in primary_endpoint".into(),
                contract: crate::claim_contract::ClaimContract::NumericTableLookup,
                literature_evidence: None,
                matched_pvalue_keyword: None,
                linear_fold: None,
                aggregate_kind: None,
                aggregate_column: None,
                aggregate_rowset: None,
                aggregate_value: None,
                collection: None,
                term: None,
                keyed_column: None,
                keyed_value: None,
            },
            status: ClaimStatus::Verified,
            strength: ClaimStrength::Exploratory,
            audit: None,
        });
        let dec = DecisionRecord::new(
            "session-1",
            DecisionType::PostHocDeviation {
                target_stage: "primary_endpoint".into(),
                prior_method: "MMRM".into(),
                new_method: "CMH".into(),
                reason: "site imbalance".into(),
            },
            DecisionActor::Sme,
            None,
        );
        demote_claims_from_deviations(&mut report, &[dec], false);
        assert_eq!(report.verdicts[0].strength, ClaimStrength::Exploratory);
    }

    #[test]
    fn demote_claims_flags_deviated_stage_as_post_hoc() {
        let mut report = ClaimVerificationReport::empty();
        report.verdicts.push(ClaimVerdict {
            claim: Claim {
                entity: "HR".into(),
                direction: None,
                effect_size: Some(0.72),
                pvalue: None,
                source_table: Some("primary_endpoint_summary.tsv".into()),
                excerpt: "Primary endpoint HR = 0.72".into(),
                contract: crate::claim_contract::ClaimContract::NumericTableLookup,
                literature_evidence: None,
                matched_pvalue_keyword: None,
                linear_fold: None,
                aggregate_kind: None,
                aggregate_column: None,
                aggregate_rowset: None,
                aggregate_value: None,
                collection: None,
                term: None,
                keyed_column: None,
                keyed_value: None,
            },
            status: ClaimStatus::Verified,
            strength: ClaimStrength::Exploratory,
            audit: None,
        });
        report.verdicts.push(ClaimVerdict {
            claim: Claim {
                entity: "AE".into(),
                direction: None,
                effect_size: None,
                pvalue: None,
                source_table: Some("safety_summary.tsv".into()),
                excerpt: "AE rates in safety set".into(),
                contract: crate::claim_contract::ClaimContract::NumericTableLookup,
                literature_evidence: None,
                matched_pvalue_keyword: None,
                linear_fold: None,
                aggregate_kind: None,
                aggregate_column: None,
                aggregate_rowset: None,
                aggregate_value: None,
                collection: None,
                term: None,
                keyed_column: None,
                keyed_value: None,
            },
            status: ClaimStatus::Verified,
            strength: ClaimStrength::Exploratory,
            audit: None,
        });
        let dec = DecisionRecord::new(
            "session-1",
            DecisionType::PostHocDeviation {
                target_stage: "primary_endpoint".into(),
                prior_method: "MMRM".into(),
                new_method: "CMH".into(),
                reason: "x".into(),
            },
            DecisionActor::Sme,
            None,
        );
        demote_claims_from_deviations(&mut report, &[dec], true);
        // Primary endpoint claim derives from deviated stage → PostHoc.
        assert_eq!(report.verdicts[0].strength, ClaimStrength::PostHoc);
        // Safety claim doesn't reference the deviated stage → Prespecified.
        assert_eq!(report.verdicts[1].strength, ClaimStrength::Prespecified);
    }

    fn policy_json() -> serde_json::Value {
        json!({
            "verifiableEntities": {
                "enabled": true,
                "entityNamePatterns": [
                    "[A-Z][A-Z0-9]+(?:_[A-Z0-9]+)+",
                    "[A-Z][A-Z0-9]{1,}"
                ],
                "entityNameExcludePatterns": ["^NES$"],
                "directionVocab": {
                    "up": ["upregulated", "increased", "elevated"],
                    "down": ["downregulated", "decreased", "reduced"]
                },
                "effectSizeColumns": ["log2FC", "logFC", "NES"],
                "entityColumns": ["gene", "symbol", "pathway"],
                "pvalueColumns": ["padj", "pvalue"]
            }
        })
    }

    fn write_table(dir: &Path, name: &str, body: &str) {
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
    }

    #[test]
    fn verifies_matching_claim() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_summary_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.001\nCOL2A1\t-1.5\t0.003\n",
        );
        let claims = extract_claims(
            "ACAN was upregulated (log2FC=2.1, padj=0.001, Table S1).",
            &cfg,
        );
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let acan = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(acan.status, ClaimStatus::Verified),
            "got {:?}",
            acan.status
        );
    }

    #[test]
    fn verifies_pathway_statistic_without_mining_leading_edge_members() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        let pathway_dir = tmp
            .path()
            .join("runtime")
            .join("outputs")
            .join("pathway_enrichment");
        std::fs::create_dir_all(&pathway_dir).unwrap();
        write_table(
            &pathway_dir,
            "pathway_results.tsv",
            "collection\tpathway\tpval\tpadj\tES\tNES\tleadingEdge\n\
             HALLMARK\tHALLMARK_ADIPOGENESIS\t1e-7\t4.68e-06\t0.8\t1.961\tSPARCL1;GPX3;STOM;COL4A1\n",
        );
        let claims = extract_claims(
            "Notable among enriched pathways: HALLMARK_ADIPOGENESIS \
             (NES=1.961, padj=4.68e-06); leading-edge genes include \
             SPARCL1, GPX3, STOM, and COL4A1.",
            &cfg,
        );
        let verdicts = verify_claims_with_discovery(&claims, tmp.path(), tmp.path(), &cfg);

        assert_eq!(verdicts.len(), 1, "{verdicts:#?}");
        assert_eq!(verdicts[0].claim.entity, "HALLMARK_ADIPOGENESIS");
        assert!(
            matches!(verdicts[0].status, ClaimStatus::Verified),
            "{verdicts:#?}"
        );
    }

    #[test]
    fn flags_sign_mismatch() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // Table says ACAN is DOWNregulated, narrative claims UP.
        write_table(
            tmp.path(),
            "de_summary_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t-1.2\t0.001\n",
        );
        let claims = extract_claims("ACAN was upregulated (log2FC=2.1, Table S1).", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert!(report.has_mismatch(), "expected at least one mismatch");
        let acan = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        match &acan.status {
            ClaimStatus::Mismatch { detail } => {
                assert!(detail.contains("effect"), "got: {}", detail);
            }
            other => panic!("expected Mismatch, got {:?}", other),
        }
    }

    #[test]
    fn flags_direction_word_against_opposite_table_sign() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // Narrative says "upregulated" but omits the numeric effect
        // size; table says the effect is negative — this is the classic
        // fabrication pattern (direction asserted, table disagrees).
        write_table(
            tmp.path(),
            "de_summary_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t-1.2\t0.001\n",
        );
        let claims = extract_claims("ACAN was upregulated (Table S1).", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert!(report.has_mismatch());
    }

    #[test]
    fn unverifiable_when_claimed_pvalue_has_no_table_evidence() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(tmp.path(), "de_summary_s1.tsv", "gene\tlog2FC\nACAN\t2.1\n");
        let claims = extract_claims(
            "ACAN was upregulated (log2FC=2.1, padj=0.001, Table S1).",
            &cfg,
        );
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let acan = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        match &acan.status {
            ClaimStatus::Unverifiable { reason } => {
                assert!(reason.contains("p-value"), "got: {}", reason);
            }
            other => panic!("expected Unverifiable, got {:?}", other),
        }
        assert!(report.n_unverifiable >= 1);
    }

    #[test]
    fn unverifiable_when_no_table_cited() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_summary_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.001\n",
        );
        let claims = extract_claims("ACAN was upregulated.", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let acan = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(matches!(acan.status, ClaimStatus::Unverifiable { .. }));
        assert_eq!(report.n_unverifiable, 1);
    }

    #[test]
    fn vf0_absent_entity_with_quant_same_namespace_is_suspicious() {
        // VF-0 catch: a SPECIFIC effect size is asserted for ACAN, but ACAN is
        // absent from the cited (symbol-keyed) table — the fabricated/untested
        // signature. Same namespace (both gene symbols) → Suspicious (soft),
        // not a silent Unverifiable pass.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_summary_s1.tsv",
            "gene\tlog2FC\tpadj\nCOL2A1\t-1.5\t0.003\n",
        );
        let claims = extract_claims("ACAN was upregulated (log2FC=2.1, Table S1).", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let acan = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(acan.status, ClaimStatus::Suspicious { .. }),
            "absent entity + specific effect size + same namespace must be Suspicious, got {:?}",
            acan.status
        );
        assert_eq!(report.n_suspicious, 1);
        assert_eq!(
            report.n_mismatch, 0,
            "Suspicious must not count as a mismatch"
        );
    }

    #[test]
    fn vf0_absent_entity_without_quant_stays_unverifiable() {
        // FP guard: a bare mention (no specific effect/p) of an absent entity
        // has nothing fabricated to flag — it must stay Unverifiable.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_summary_s1.tsv",
            "gene\tlog2FC\tpadj\nCOL2A1\t-1.5\t0.003\n",
        );
        // No number, no direction-bearing slot beyond the word — bare mention.
        let claims = extract_claims("ACAN is a chondrocyte matrix gene (Table S1).", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let acan = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(acan.status, ClaimStatus::Unverifiable { .. }),
            "bare mention of an absent entity must stay Unverifiable, got {:?}",
            acan.status
        );
        assert_eq!(report.n_suspicious, 0);
    }

    #[test]
    fn vf0_namespace_mismatch_stays_unverifiable() {
        // FP guard: a SYMBOL claim looked up in an ENSEMBL-keyed table is a
        // benign cross-namespace miss (handled by the symbol↔Ensembl validator),
        // NOT a fabrication. It must stay Unverifiable, never Suspicious — even
        // though a specific effect size is asserted.
        let mut policy = policy_json();
        // Ensure the Ensembl id pattern + gene_id entity column are configured.
        policy["verifiableEntities"]["entityNamePatterns"] =
            serde_json::json!(["[A-Z][A-Z0-9]{1,}", "ENS[A-Z]{0,4}[GTP]\\d{6,}"]);
        policy["verifiableEntities"]["entityColumns"] =
            serde_json::json!(["gene", "gene_id", "symbol"]);
        let cfg = ExtractorConfig::from_policy(&policy).unwrap();
        let tmp = tempdir().unwrap();
        // Ensembl-keyed table; the claim names a SYMBOL absent from it.
        write_table(
            tmp.path(),
            "de_summary_s1.tsv",
            "gene_id\tlog2FC\tpadj\nENSG00000139618\t-1.5\t0.003\n",
        );
        let claims = extract_claims("CRISPLD2 was upregulated (log2FC=2.6, Table S1).", &cfg);
        let crispld2 = {
            let report = verify_claims(&claims, tmp.path(), &cfg);
            report
                .verdicts
                .iter()
                .find(|v| v.claim.entity == "CRISPLD2")
                .unwrap()
                .status
                .clone()
        };
        assert!(
            matches!(crispld2, ClaimStatus::Unverifiable { .. }),
            "symbol-vs-Ensembl miss must stay Unverifiable (not Suspicious), got {:?}",
            crispld2
        );
    }

    #[test]
    fn a1_tnf_token_vs_pathway_set_name_is_unverifiable_not_suspicious() {
        // A1 FP fix: a bare single-token gene claim ("TNF (log2FC=3.0…)") cited
        // against a PATHWAY table whose entity column holds multi-word SET names
        // ("TNF-alpha Signaling via NF-kB") is a benign cross-namespace miss
        // (symbol vs set), NOT a fabrication. It must be Unverifiable, never
        // Suspicious — the old code classed both as `symbol` and wrongly fired
        // VF-0 Suspicious.
        let mut policy = policy_json();
        policy["verifiableEntities"]["effectSizeColumns"] =
            serde_json::json!(["NES", "log2FC", "logFC"]);
        policy["verifiableEntities"]["entityColumns"] =
            serde_json::json!(["gene", "term", "pathway", "symbol"]);
        let cfg = ExtractorConfig::from_policy(&policy).unwrap();
        let tmp = tempdir().unwrap();
        // Pathway table keyed on `term` whose only row is a multi-word set NAME.
        write_table(
            tmp.path(),
            "gsea_s1.tsv",
            "term\tNES\tpadj\nTNF-alpha Signaling via NF-kB\t2.3\t0.001\n",
        );
        let claims = extract_claims("TNF was elevated (log2FC=3.0, padj=0.01, Table S1).", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let tnf = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "TNF")
            .unwrap();
        assert!(
            matches!(tnf.status, ClaimStatus::Unverifiable { .. }),
            "bare TNF token vs a set-name-keyed pathway table is a cross-namespace miss → Unverifiable, got {:?}",
            tnf.status
        );
        assert_eq!(
            report.n_suspicious, 0,
            "set-vs-symbol namespace mismatch must NOT be flagged Suspicious"
        );
    }

    #[test]
    fn a1_preserved_positive_absent_single_token_gene_is_still_suspicious() {
        // A1 PRESERVED POSITIVE: the namespace fix must NOT weaken real
        // detection. A single-token gene (GENEX) with a specific asserted effect,
        // absent from a symbol-keyed DE table, is STILL Suspicious.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_summary_s1.tsv",
            "gene\tlog2FC\tpadj\nCOL2A1\t-1.5\t0.003\n",
        );
        let claims = extract_claims("GENEX was upregulated (log2FC=4.2, Table S1).", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let genex = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "GENEX")
            .unwrap();
        assert!(
            matches!(genex.status, ClaimStatus::Suspicious { .. }),
            "absent single-token gene with a specific effect must STILL be Suspicious, got {:?}",
            genex.status
        );
        assert_eq!(report.n_suspicious, 1);
    }

    #[test]
    fn a2_tnf_token_vs_pathway_table_with_single_word_first_row_is_unverifiable() {
        // A2 FP fix (root cause of the Himes `TNF` false positive): a pathway
        // table's FIRST (top-ranked) row can be a SINGLE-WORD term name
        // ("Adipogenesis"), which `id_namespace` classes as `symbol` — so
        // sampling the first row alone mis-typed the whole table as
        // symbol-keyed and VF-0 fired Suspicious on a bare "TNF" claim. The
        // pathway it names ("TNF-alpha Signaling via NF-kB") is PRESENT in the
        // table under its full set name, so this is a benign cross-namespace
        // miss, not a fabrication. Sampling a row window now recognises the
        // table as `set`-keyed → Unverifiable, never Suspicious.
        let mut policy = policy_json();
        policy["verifiableEntities"]["effectSizeColumns"] =
            serde_json::json!(["NES", "log2FC", "logFC"]);
        policy["verifiableEntities"]["entityColumns"] =
            serde_json::json!(["gene", "term", "pathway", "symbol"]);
        let cfg = ExtractorConfig::from_policy(&policy).unwrap();
        let tmp = tempdir().unwrap();
        // Pathway table whose FIRST row is a single-word term name; the TNF
        // pathway is present further down under its full multi-word set name.
        write_table(
            tmp.path(),
            "gsea_s1.tsv",
            "term\tNES\tpadj\n\
             Adipogenesis\t2.005\t0.0000005\n\
             TNF-alpha Signaling via NF-kB\t1.836\t0.0001\n\
             Autophagy\t1.982\t0.0002\n",
        );
        let claims = extract_claims(
            "Hallmark TNF-alpha Signaling via NF-kB was enriched (NES=1.836, padj=1.13e-04, Table S1).",
            &cfg,
        );
        let report = verify_claims(&claims, tmp.path(), &cfg);
        // The extractor may pull a bare `TNF` token from the prose; whatever its
        // verdict, it must not be Suspicious (the pathway IS present in-table).
        if let Some(tnf) = report.verdicts.iter().find(|v| v.claim.entity == "TNF") {
            assert!(
                matches!(tnf.status, ClaimStatus::Unverifiable { .. }),
                "bare TNF token vs a pathway table with a single-word first row must NOT be Suspicious, got {:?}",
                tnf.status
            );
        }
        // Whole-report invariant: nothing in this faithful narrative is flagged.
        assert_eq!(
            report.n_suspicious, 0,
            "a faithful pathway claim whose pathway IS present must yield zero Suspicious"
        );
    }

    #[test]
    fn a2_preserved_positive_absent_gene_in_symbol_table_with_set_lookalike_first_row() {
        // A2 PRESERVED POSITIVE: the row-window sampling must not let a single
        // multi-word junk value in row 1 mask a genuine symbol-keyed table and
        // thereby SUPPRESS a real fabrication. A symbol-keyed DE table whose
        // rows are all single-token gene symbols must still be classed `symbol`,
        // so an absent gene (GENEX) with a specific asserted effect is STILL
        // Suspicious.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_summary_s1.tsv",
            "gene\tlog2FC\tpadj\n\
             COL2A1\t-1.5\t0.003\n\
             ACAN\t2.1\t0.001\n\
             SOX9\t1.2\t0.01\n",
        );
        let claims = extract_claims("GENEX was upregulated (log2FC=4.2, Table S1).", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let genex = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "GENEX")
            .unwrap();
        assert!(
            matches!(genex.status, ClaimStatus::Suspicious { .. }),
            "absent single-token gene in an all-symbol DE table must STILL be Suspicious, got {:?}",
            genex.status
        );
        assert_eq!(report.n_suspicious, 1);
    }

    #[test]
    fn a2_pending_vs_unverifiable_split_cannot_inflate_coverage() {
        // A2: a NEVER-ADJUDICATED claim (no evidence file) counts n_pending; a
        // CHECKED-BUT-UNDETERMINABLE claim (table loaded, but no effect/p column
        // for the named entity) STILL counts n_unverifiable. Neither is Verified,
        // so the split cannot inflate any verified/coverage floor.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();

        // Never adjudicated: a structured claim citing NO evidence file.
        let no_evidence = StructuredClaim {
            claim: "Pathway activity increased overall".into(),
            evidence: None,
        };
        let pkg = tempdir().unwrap();
        let v_pending = verify_structured_claims(&[no_evidence], pkg.path(), &cfg);
        assert!(
            matches!(v_pending[0].status, ClaimStatus::Pending { .. }),
            "no-evidence claim must be Pending, got {:?}",
            v_pending[0].status
        );

        // Checked but undeterminable: table loads but carries no effect-size
        // column the claim could be adjudicated against → stays Unverifiable.
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "counts_s1.tsv",
            // No log2FC/logFC column at all; only a non-effect numeric column.
            "gene\tbase_mean\tpadj\nACAN\t120.0\t0.001\n",
        );
        let claims = extract_claims(
            "ACAN was upregulated (log2FC=2.1, padj=0.001, Table S1).",
            &cfg,
        );
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let acan = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(acan.status, ClaimStatus::Unverifiable { .. }),
            "table-loaded-but-no-effect-column claim must STAY Unverifiable (not Pending), got {:?}",
            acan.status
        );
        assert_eq!(
            report.n_unverifiable, 1,
            "checked-but-undeterminable still counts n_unverifiable"
        );
        assert_eq!(
            report.n_pending, 0,
            "an Unverifiable claim must not be relabeled Pending"
        );
        // And the push() bookkeeping keeps the two buckets disjoint.
        let mut combined = ClaimVerificationReport::empty();
        combined.push(v_pending[0].clone());
        combined.push(acan.clone());
        assert_eq!(combined.n_pending, 1);
        assert_eq!(combined.n_unverifiable, 1);
        assert_eq!(combined.n_verified, 0, "neither bucket can become Verified");
    }

    #[test]
    fn a3_extreme_value_correct_top_negative_nes_verifies_wrong_mismatches() {
        // A3: an ordinal/superlative extreme claim WITHOUT a rank digit ("the
        // most strongly DOWNregulated gene by log2FC") routes to ExtremeValue and
        // is verified against the actual argmin of the column.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.01\nCOL2A1\t-4.6\t0.001\nMMP13\t-1.2\t0.02\n",
        );

        // Correct: COL2A1 has the lowest (most negative) log2FC.
        let correct = Claim {
            entity: "COL2A1".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: Some("de_s1.tsv".into()),
            excerpt: "COL2A1 was the lowest log2FC gene (Table S1)".into(),
            contract: ClaimContract::ExtremeValue,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let report = verify_claims(std::slice::from_ref(&correct), tmp.path(), &cfg);
        assert!(
            matches!(report.verdicts[0].status, ClaimStatus::Verified),
            "entity that IS the argmin must Verify, got {:?}",
            report.verdicts[0].status
        );

        // Wrong: MMP13 is NOT the lowest log2FC — claiming it is must Mismatch.
        let wrong = Claim {
            entity: "MMP13".into(),
            excerpt: "MMP13 was the lowest log2FC gene (Table S1)".into(),
            ..correct.clone()
        };
        let report2 = verify_claims(std::slice::from_ref(&wrong), tmp.path(), &cfg);
        assert!(
            matches!(report2.verdicts[0].status, ClaimStatus::Mismatch { .. }),
            "entity that is NOT the argmin must Mismatch, got {:?}",
            report2.verdicts[0].status
        );
    }

    #[test]
    fn paired_positive_and_negative_association_extremes_bind_per_entity() {
        let mut policy = policy_json();
        policy["verifiableEntities"]["directionVocab"]["up"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("positive association"));
        policy["verifiableEntities"]["directionVocab"]["down"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!("negative association"));
        let cfg = ExtractorConfig::from_policy(&policy).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_results.tsv",
            "gene\tlog2FC\tpadj\n\
             ENSG00000109906\t4.84\t2.21e-40\n\
             ENSG00000162692\t-3.45\t5.12e-81\n\
             ENSG00000152583\t4.10\t7.06e-132\n",
        );
        let text = "The gene with the strongest positive association was \
                    ENSG00000109906 (log2FC = 4.84, padj = 2.21e-40), and the \
                    gene with the strongest negative association was \
                    ENSG00000162692 (log2FC = -3.45, padj = 5.12e-81).";
        let mut claims = crate::claim_extractor::extract_claims(text, &cfg);
        assert_eq!(claims.len(), 2, "{claims:?}");
        for claim in &mut claims {
            claim.source_table = Some("de_results.tsv".to_string());
        }
        assert_eq!(claims[0].direction, Some(Direction::Up));
        assert_eq!(claims[1].direction, Some(Direction::Down));

        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert_eq!(report.n_verified, 2, "{:?}", report.verdicts);
        assert_eq!(report.n_mismatch, 0, "{:?}", report.verdicts);
    }

    #[test]
    fn numeric_gate_does_not_hide_a_second_single_entity_value() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_results.tsv",
            "gene\tlog2FC\tpadj\nENSG00000109906\t4.84\t2.21e-40\n",
        );
        let claim = Claim {
            entity: "ENSG00000109906".into(),
            direction: Some(Direction::Up),
            effect_size: Some(4.84),
            pvalue: None,
            source_table: Some("de_results.tsv".into()),
            excerpt: "ENSG00000109906 had log2FC = 4.84, but the same sentence also \
                      reports log2FC = 9.99."
                .into(),
            contract: ClaimContract::NumericTableLookup,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let canonical = crate::claim_extractor::canonicalize_scientific(&claim.excerpt);
        let tokens = scan_numeric_tokens(&claim.excerpt);
        assert_eq!(tokens.len(), 2, "{tokens:#?}");
        assert!(
            tokens.iter().all(|token| numeric_token_owned_by_claim(
                token,
                &canonical,
                &claim,
                &cfg,
                &BTreeSet::from([normalize(&claim.entity)])
            ) != Some(false)),
            "both values belong to the only entity in the sentence"
        );

        let report = verify_claims(&[claim], tmp.path(), &cfg);
        assert!(
            matches!(report.verdicts[0].status, ClaimStatus::Mismatch { .. }),
            "one agreeing value must not mask a contradictory value for the same entity: {:?}",
            report.verdicts[0].status
        );
    }

    #[test]
    fn extreme_claim_resolves_symbol_alias_in_gene_keyed_table() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_results.tsv",
            "gene\tsymbol\tlog2FC\tpadj\n\
             ENSG00000109906\tZBTB16\t4.84\t2.21e-40\n\
             ENSG00000152583\tSPARCL1\t4.31\t7.06e-132\n",
        );
        let claim = Claim {
            entity: "ZBTB16".into(),
            direction: None,
            effect_size: Some(4.84),
            pvalue: Some(2.21e-40),
            source_table: Some("de_results.tsv".into()),
            excerpt:
                "ZBTB16 had the largest positive log2FC (log2FC=4.84, padj=2.21e-40, Table S1)"
                    .into(),
            contract: ClaimContract::ExtremeValue,
            literature_evidence: None,
            matched_pvalue_keyword: Some("padj".into()),
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let report = verify_claims(&[claim], tmp.path(), &cfg);
        assert!(
            matches!(report.verdicts[0].status, ClaimStatus::Verified),
            "a symbol alias must resolve the gene-keyed row: {:?}",
            report.verdicts[0].status
        );
        assert_eq!(
            report.verdicts[0]
                .audit
                .as_ref()
                .and_then(|audit| audit.entity_column.as_deref()),
            Some("symbol")
        );
    }

    #[test]
    fn anaphoric_subset_extreme_abstains_instead_of_using_global_extreme() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_results.tsv",
            "gene\tlog2FC\tpadj\n\
             ENSG00000162692\t-3.692579\t5.12e-81\n\
             ENSG00000167476\t-5.325\t1.00e-20\n",
        );
        let claim = Claim {
            entity: "ENSG00000162692".into(),
            direction: Some(Direction::Down),
            effect_size: Some(-3.693),
            pvalue: Some(5.12e-81),
            source_table: Some("de_results.tsv".into()),
            excerpt: "VCAM1 (ENSG00000162692, log2FC = -3.693, padj = 5.12e-81) is the most strongly down-associated gene within this top-by-significance tier".into(),
            contract: ClaimContract::ExtremeValue,
            literature_evidence: None,
            matched_pvalue_keyword: Some("padj".into()),
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let report = verify_claims(std::slice::from_ref(&claim), tmp.path(), &cfg);
        match &report.verdicts[0].status {
            ClaimStatus::Unverifiable { reason } => {
                assert!(reason.contains("displayed subset"), "{reason}");
                assert!(reason.contains("global argmax/argmin"), "{reason}");
            }
            status => {
                panic!("an anaphoric subset must not be compared with the full table: {status:?}")
            }
        }
    }

    #[test]
    fn a3_top_downregulated_most_negative_is_argmin_not_argmax() {
        // Regression (himes rerun audit 2026-07-21): a report line
        // "Top down-regulated gene (most negative log2FC ...): GENE" is an
        // ARGMIN claim over a signed effect column. The generic superlative
        // "Top" must NOT force an argmax reading — the down-regulation /
        // most-negative direction cue governs. Before the fix, "Top" matched
        // the max-word regex and "most negative" matched no min-word, so the
        // verifier checked the argMAX and the true argmin gene false-Mismatched.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_results.tsv",
            "gene\tlog2FC\tpadj\nGUP\t5.0\t0.001\nGDOWN\t-4.8\t0.002\nGMID\t-1.0\t0.01\n",
        );
        // GDOWN is the argmin of log2FC → the true top down-regulated gene.
        let down = Claim {
            entity: "GDOWN".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: Some("de_results.tsv".into()),
            excerpt:
                "Top down-regulated gene (most negative log2FC among significant genes): GDOWN"
                    .into(),
            contract: ClaimContract::ExtremeValue,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let report = verify_claims(std::slice::from_ref(&down), tmp.path(), &cfg);
        assert!(
            matches!(report.verdicts[0].status, ClaimStatus::Verified),
            "the true argmin named as 'top down-regulated (most negative)' must Verify, got {:?}",
            report.verdicts[0].status
        );
        // Guard the symmetric case: top up-regulated (largest positive) still
        // resolves to argmax and verifies GUP.
        let up = Claim {
            entity: "GUP".into(),
            excerpt: "Top up-regulated gene (largest positive log2FC among significant genes): GUP"
                .into(),
            ..down.clone()
        };
        let up_report = verify_claims(std::slice::from_ref(&up), tmp.path(), &cfg);
        assert!(
            matches!(up_report.verdicts[0].status, ClaimStatus::Verified),
            "top up-regulated (largest positive) must still Verify, got {:?}",
            up_report.verdicts[0].status
        );
    }

    #[test]
    fn a3_most_significant_by_padj_ranks_padj_despite_direction_aside() {
        // Real himes report line: "Most significant gene (by padj): SPARCL1 …
        // padj = 7.06e-132 - strongly upregulated". This is an argMIN over padj
        // (SPARCL1 has the smallest padj); the trailing "strongly upregulated"
        // is a descriptive aside and must NOT flip the extreme onto log2FC
        // (where SPARCL1 is not the maximum). Before the fix, the up-direction
        // cue set has_effect_direction_cue → over_pvalue=false → the verifier
        // checked the argMAX log2FC and false-Mismatched the true
        // most-significant gene.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // GSIG has the SMALLEST padj (most significant) but NOT the largest
        // log2FC; GBIG has the largest log2FC; GNEG the most-negative log2FC.
        write_table(
            tmp.path(),
            "de_results.tsv",
            "gene\tlog2FC\tpadj\nGBIG\t10.0\t1e-10\nGSIG\t4.0\t1e-100\nGNEG\t-5.0\t1e-3\n",
        );
        let claim = Claim {
            entity: "GSIG".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: Some("de_results.tsv".into()),
            excerpt: "Most significant gene (by padj): GSIG (log2FC = +4.0, padj = 1e-100) - \
                      strongly upregulated in dexamethasone-treated cells"
                .into(),
            contract: ClaimContract::ExtremeValue,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let report = verify_claims(std::slice::from_ref(&claim), tmp.path(), &cfg);
        assert!(
            matches!(report.verdicts[0].status, ClaimStatus::Verified),
            "'most significant (by padj)' must rank by padj (argmin) despite the \
             upregulated aside; got {:?}",
            report.verdicts[0].status
        );
        // Guard the 2026-07-22 incidental-padj case is untouched: a "most
        // negative log2FC … (padj ≈ 1e-100)" claim still ranks the EFFECT column
        // — GSIG (+4.0) is not the argmin log2FC (GNEG is), so it Mismatches,
        // proving over_pvalue did not over-trigger on the trailing padj.
        let incidental = Claim {
            entity: "GSIG".into(),
            excerpt: "Top down-regulated gene (most negative log2FC among significant genes): \
                      GSIG (padj ≈ 1e-100)"
                .into(),
            ..claim.clone()
        };
        let r2 = verify_claims(std::slice::from_ref(&incidental), tmp.path(), &cfg);
        assert!(
            matches!(r2.verdicts[0].status, ClaimStatus::Mismatch { .. }),
            "incidental trailing padj must not flip to the p-value column; GSIG is not the \
             most-negative log2FC → Mismatch; got {:?}",
            r2.verdicts[0].status
        );
    }

    #[test]
    fn a3_classifier_routes_superlative_to_extreme_value() {
        // The extractor must classify a digit-free superlative naming a column as
        // ExtremeValue (and a numeric rank as RankTopN, unchanged).
        assert_eq!(
            crate::claim_extractor::classify_contract("the most downregulated gene by log2FC"),
            ClaimContract::ExtremeValue
        );
        assert_eq!(
            crate::claim_extractor::classify_contract("TP53 had the lowest padj"),
            ClaimContract::ExtremeValue
        );
        assert_eq!(
            crate::claim_extractor::classify_contract("the top-10 genes by log2FC"),
            ClaimContract::RankTopN,
            "an explicit numeric rank must STAY RankTopN, not become ExtremeValue"
        );
    }

    // FAITHFUL TWIN (1): an EXPLICIT single-argmax superlative still enforces
    // strict argmax equality. SPARCL1 (the true highest log2FC) Verifies; the
    // same explicit "highest log2FC gene" claim naming CRISPLD2 (NOT the argmax)
    // still Mismatches — the explicit-superlative path is unchanged.
    #[test]
    fn a3_explicit_highest_superlative_still_enforces_argmax() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nSPARCL1\t3.9\t0.001\nCRISPLD2\t2.5\t0.002\nMMP13\t-1.2\t0.02\n",
        );

        let correct = Claim {
            entity: "SPARCL1".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: Some("de_s1.tsv".into()),
            excerpt: "SPARCL1 is the highest log2FC gene (Table S1)".into(),
            contract: ClaimContract::ExtremeValue,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        assert_eq!(
            crate::claim_extractor::classify_contract(&correct.excerpt),
            ClaimContract::ExtremeValue,
            "an explicit `highest log2FC` superlative must route to ExtremeValue"
        );
        let report = verify_claims(std::slice::from_ref(&correct), tmp.path(), &cfg);
        assert!(
            matches!(report.verdicts[0].status, ClaimStatus::Verified),
            "the true argmax must Verify, got {:?}",
            report.verdicts[0].status
        );

        let wrong = Claim {
            entity: "CRISPLD2".into(),
            excerpt: "CRISPLD2 is the highest log2FC gene (Table S1)".into(),
            ..correct.clone()
        };
        let report2 = verify_claims(std::slice::from_ref(&wrong), tmp.path(), &cfg);
        assert!(
            matches!(report2.verdicts[0].status, ClaimStatus::Mismatch { .. }),
            "a NON-argmax named as the explicit `highest` must still Mismatch, got {:?}",
            report2.verdicts[0].status
        );
    }

    // FAITHFUL TWIN (2): a HEDGED soft-top claim ("X is one of the top DE
    // genes") is a top-N MEMBERSHIP assertion, not a single-argmax claim. The
    // prior false positive (it was routed to ExtremeValue and flagged as a wrong
    // max-assertion) now flips: CRISPLD2, which IS within the top-N by
    // |log2FC|, Verifies; a soft-top claim for a gene OUTSIDE the top-N still
    // Mismatches (membership is genuinely checked, not waved through).
    #[test]
    fn a3_soft_top_membership_verifies_and_outsider_mismatches() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // CRISPLD2 (|2.5|) sits ABOVE the default top-N cutoff (default N=10);
        // FILLER0 (|0.05|) is at the bottom and OUTSIDE the top-N. Four genes
        // outrank CRISPLD2 (it is rank 5), and ELEVEN genes outrank FILLER0
        // (it is rank ≥12), so the cutoff genuinely separates the two.
        let mut body = String::from("gene\tlog2FC\tpadj\nCRISPLD2\t2.5\t0.002\n");
        for i in 0..4 {
            // Four genes that outrank CRISPLD2 by magnitude.
            body.push_str(&format!("BIG{i}\t{}\t0.01\n", 3.0 + i as f64 * 0.1));
        }
        for i in 0..7 {
            // Seven mid genes that outrank FILLER0 but not CRISPLD2, pushing the
            // table past 11 ranked genes so FILLER0 falls outside the top-10.
            body.push_str(&format!("MID{i}\t{}\t0.02\n", 1.0 + i as f64 * 0.05));
        }
        body.push_str("FILLER0\t0.05\t0.9\n");
        write_table(tmp.path(), "de_s1.tsv", &body);

        let crispld2 = Claim {
            entity: "CRISPLD2".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: Some("de_s1.tsv".into()),
            excerpt: "CRISPLD2 is one of the top DE genes (log2FC ~ 2.5, Table S1)".into(),
            contract: crate::claim_extractor::classify_contract(
                "CRISPLD2 is one of the top DE genes (log2FC ~ 2.5, Table S1)",
            ),
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        assert_eq!(
            crispld2.contract,
            ClaimContract::RankTopN,
            "a hedged `one of the top` must route to RankTopN membership, not ExtremeValue"
        );
        let report = verify_claims(std::slice::from_ref(&crispld2), tmp.path(), &cfg);
        assert!(
            matches!(report.verdicts[0].status, ClaimStatus::Verified),
            "a gene WITHIN the top-N named as `one of the top` must Verify (the prior false positive), got {:?}",
            report.verdicts[0].status
        );

        let outsider = Claim {
            entity: "FILLER0".into(),
            excerpt: "FILLER0 is one of the top DE genes (Table S1)".into(),
            contract: crate::claim_extractor::classify_contract(
                "FILLER0 is one of the top DE genes (Table S1)",
            ),
            ..crispld2.clone()
        };
        assert_eq!(outsider.contract, ClaimContract::RankTopN);
        let report2 = verify_claims(std::slice::from_ref(&outsider), tmp.path(), &cfg);
        assert!(
            matches!(report2.verdicts[0].status, ClaimStatus::Mismatch { .. }),
            "a gene OUTSIDE the top-N named as `one of the top` must still Mismatch, got {:?}",
            report2.verdicts[0].status
        );
    }

    // A HEDGED significance superlative ("one of the most significantly
    // upregulated genes") ranks membership by SIGNIFICANCE (padj), not by
    // |effect size|. CRISPLD2 here is the single most-significant gene (smallest
    // padj) yet sits OUTSIDE the top-N by |log2FC| — twelve genes carry a larger
    // fold-change. Before significance-aware ranking it false-Mismatched; it now
    // Verifies. The magnitude twin ("one of the most upregulated genes", no
    // "significantly") still Mismatches, proving the significance wording — not a
    // looser cutoff — is what flips the verdict.
    #[test]
    fn rank_top_n_significance_superlative_ranks_by_padj() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // CRISPLD2: modest +log2FC but the smallest padj (rank 1 by significance).
        let mut body = String::from("gene\tlog2FC\tpadj\nCRISPLD2\t2.6\t1e-80\n");
        // Twelve genes with a LARGER |log2FC| than CRISPLD2 but far weaker
        // significance, so CRISPLD2 is rank 13 by |log2FC| (outside the default
        // top-10) while remaining rank 1 by padj.
        for i in 0..12 {
            body.push_str(&format!("BIG{i}\t{}\t0.01\n", 3.0 + i as f64 * 0.1));
        }
        write_table(tmp.path(), "de_s1.tsv", &body);

        let sig = extract_claims(
            "CRISPLD2 is one of the most significantly upregulated genes (Table S1).",
            &cfg,
        );
        let sig_report = verify_claims(&sig, tmp.path(), &cfg);
        let crispld2 = sig_report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "CRISPLD2")
            .expect("CRISPLD2 claim extracted");
        assert_eq!(
            crispld2.claim.contract,
            ClaimContract::RankTopN,
            "a hedged `one of the most significantly …` must route to RankTopN"
        );
        assert!(
            matches!(crispld2.status, ClaimStatus::Verified),
            "the most-significant gene named `one of the most significantly upregulated` must Verify under significance ranking, got {:?}",
            crispld2.status
        );

        // Magnitude twin: drop "significantly" → ranked by |effect size|, where
        // CRISPLD2 is rank 13 and OUTSIDE the top-N → still a Mismatch.
        let mag = extract_claims(
            "CRISPLD2 is one of the most upregulated genes (Table S1).",
            &cfg,
        );
        let mag_report = verify_claims(&mag, tmp.path(), &cfg);
        let crispld2_mag = mag_report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "CRISPLD2")
            .expect("CRISPLD2 claim extracted");
        assert!(
            matches!(crispld2_mag.status, ClaimStatus::Mismatch { .. }),
            "without a significance cue the same gene ranks by |effect size| and stays a Mismatch, got {:?}",
            crispld2_mag.status
        );
    }

    // A genuine RankTopN Mismatch must record a rationale that reflects the
    // RANKING failure, never the "agrees" numeric rationale of the pass audit.
    // FILLER0's cited log2FC is correct (the numeric gate agrees) but it is far
    // outside the top-N by |effect size|, so the base verdict is a Mismatch. The
    // run_numeric_gate fall-through previously attached the agreeing numeric
    // audit verbatim, leaving a recorded Mismatch whose rationale read "agrees";
    // the audit rationale must now carry the base Mismatch detail.
    #[test]
    fn rank_top_n_mismatch_rationale_is_not_agrees() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // Twelve high-|log2FC| genes fill the top-N; FILLER0's |0.5| is far below.
        let mut body = String::from("gene\tlog2FC\tpadj\n");
        for i in 0..12 {
            body.push_str(&format!("BIG{i}\t{}\t0.01\n", 3.0 + i as f64 * 0.1));
        }
        body.push_str("FILLER0\t0.5\t0.5\n");
        write_table(tmp.path(), "de_s1.tsv", &body);

        // The cited log2FC (0.5) MATCHES the table, so the numeric gate produces
        // an "agrees" pass audit — exactly the situation that surfaced the bug.
        let claims = extract_claims(
            "FILLER0 shows one of the largest fold-changes (log2FC = 0.5, Table S1).",
            &cfg,
        );
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let filler = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "FILLER0")
            .expect("FILLER0 claim extracted");
        assert_eq!(filler.claim.contract, ClaimContract::RankTopN);
        assert!(
            matches!(filler.status, ClaimStatus::Mismatch { .. }),
            "a gene outside the top-N by |effect size| must Mismatch, got {:?}",
            filler.status
        );
        let rationale = filler
            .audit
            .as_ref()
            .and_then(|a| a.rationale.as_deref())
            .expect("a Mismatch carries an audit rationale");
        assert!(
            rationale.contains("ranked by") && !rationale.contains("agrees"),
            "a recorded Mismatch must carry the ranking-failure rationale, not an `agrees` numeric rationale, got {rationale:?}"
        );
    }

    // FAITHFUL TWIN (3): the SAME explicit-superlative claim by gene SYMBOL and
    // by Ensembl ACCESSION yields an IDENTICAL verdict, because the extreme
    // verifier resolves the entity through the annotation map before lookup.
    // The table is keyed by accession; the symbol-only claim must still find its
    // row. A genuinely-wrong claim (a non-argmax) Mismatches under BOTH forms.
    #[test]
    fn a3_extreme_symbol_and_accession_resolve_identically() {
        let mut map = BTreeMap::new();
        map.insert("sparcl1".to_string(), "ENSG00000152583".to_string());
        map.insert("crispld2".to_string(), "ENSG00000103196".to_string());
        let mut cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        cfg.gene_annotation_map = Some(map);

        let tmp = tempdir().unwrap();
        // Table keyed by ACCESSION; SPARCL1's accession holds the true argmax.
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nENSG00000152583\t3.9\t0.001\nENSG00000103196\t2.5\t0.002\n",
        );

        let base = Claim {
            entity: "SPARCL1".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: Some("de_s1.tsv".into()),
            excerpt: "SPARCL1 is the highest log2FC gene (Table S1)".into(),
            contract: ClaimContract::ExtremeValue,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let by_symbol = verify_claims(std::slice::from_ref(&base), tmp.path(), &cfg);
        let by_accession = verify_claims(
            std::slice::from_ref(&Claim {
                entity: "ENSG00000152583".into(),
                excerpt: "ENSG00000152583 is the highest log2FC gene (Table S1)".into(),
                ..base.clone()
            }),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(by_symbol.verdicts[0].status, ClaimStatus::Verified)
                && matches!(by_accession.verdicts[0].status, ClaimStatus::Verified),
            "symbol-keyed and accession-keyed argmax claims must BOTH Verify, got symbol={:?} accession={:?}",
            by_symbol.verdicts[0].status,
            by_accession.verdicts[0].status,
        );

        // Same class of error under both forms: CRISPLD2 is NOT the argmax.
        let wrong_symbol = verify_claims(
            std::slice::from_ref(&Claim {
                entity: "CRISPLD2".into(),
                excerpt: "CRISPLD2 is the highest log2FC gene (Table S1)".into(),
                ..base.clone()
            }),
            tmp.path(),
            &cfg,
        );
        let wrong_accession = verify_claims(
            std::slice::from_ref(&Claim {
                entity: "ENSG00000103196".into(),
                excerpt: "ENSG00000103196 is the highest log2FC gene (Table S1)".into(),
                ..base.clone()
            }),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(wrong_symbol.verdicts[0].status, ClaimStatus::Mismatch { .. })
                && matches!(wrong_accession.verdicts[0].status, ClaimStatus::Mismatch { .. }),
            "a non-argmax must Mismatch under BOTH symbol and accession forms, got symbol={:?} accession={:?}",
            wrong_symbol.verdicts[0].status,
            wrong_accession.verdicts[0].status,
        );
    }

    #[test]
    fn a4_structured_count_split_mismatch_and_match() {
        // A4: result.json's n_up_fdr05 / n_down_fdr05 are recomputed from
        // de_results.tsv. A disagreeing split is a real Mismatch; a matching one
        // Verifies.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();

        // de_results.tsv: 2 up (log2FC>0, padj<0.05), 1 down — but more rows so
        // the recompute is non-trivial. Build a small deterministic table.
        let mut body = String::from("gene\tlog2FC\tpadj\n");
        // 3 significant up genes:
        for i in 0..3 {
            body.push_str(&format!("UP{i}\t1.5\t0.001\n"));
        }
        // 2 significant down genes:
        for i in 0..2 {
            body.push_str(&format!("DN{i}\t-1.5\t0.001\n"));
        }
        // 1 non-significant gene (must be excluded):
        body.push_str("NS0\t3.0\t0.9\n");

        // MISMATCH: result.json claims a swapped/wrong split (5 up / 0 down).
        let pkg = tempdir().unwrap();
        write_pkg_table(
            pkg.path(),
            "differential_expression",
            "de_results.tsv",
            &body,
        );
        std::fs::write(
            pkg.path().join("result.json"),
            r#"{"n_up_fdr05": 5, "n_down_fdr05": 0}"#,
        )
        .unwrap();
        let verdicts = verify_structured_counts(pkg.path(), &cfg);
        assert!(
            verdicts
                .iter()
                .any(|v| matches!(v.status, ClaimStatus::Mismatch { .. })),
            "a wrong up/down split must yield a Mismatch, got {:?}",
            verdicts.iter().map(|v| &v.status).collect::<Vec<_>>()
        );

        // MATCH: result.json claims the correct split (3 up / 2 down).
        let pkg2 = tempdir().unwrap();
        write_pkg_table(
            pkg2.path(),
            "differential_expression",
            "de_results.tsv",
            &body,
        );
        std::fs::write(
            pkg2.path().join("result.json"),
            r#"{"n_up_fdr05": 3, "n_down_fdr05": 2}"#,
        )
        .unwrap();
        let verdicts2 = verify_structured_counts(pkg2.path(), &cfg);
        assert!(
            !verdicts2.is_empty()
                && verdicts2
                    .iter()
                    .all(|v| matches!(v.status, ClaimStatus::Verified)),
            "a correct up/down split must Verify, got {:?}",
            verdicts2.iter().map(|v| &v.status).collect::<Vec<_>>()
        );
    }

    #[test]
    fn csv_delimiter_is_autodetected() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "table_s1.csv",
            "gene,log2FC,padj\nACAN,2.1,0.001\n",
        );
        let claims = extract_claims("ACAN was upregulated (log2FC=2.1, Table S1).", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert_eq!(report.n_verified, 1, "verdicts: {:?}", report.verdicts);
    }

    #[test]
    fn empty_report_has_no_mismatch() {
        let r = ClaimVerificationReport::empty();
        assert!(!r.has_mismatch());
        assert_eq!(r.n_checked, 0);
    }

    // ── Clinical-trial overlay round-trip ───────────────────

    #[test]
    fn clinical_trial_overlay_verifies_hazard_ratio_claim() {
        use crate::claim_extractor::extract_claims;
        let base = policy_json();
        let config_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config");
        let cfg = ExtractorConfig::from_policy_for_class(
            &base,
            &config_dir,
            crate::project_class::ProjectClass::ClinicalTrial,
        )
        .unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "primary_endpoint.tsv",
            "arm\tendpoint\thazard_ratio\tpvalue\n\
             treatment\tprimary endpoint\t0.72\t0.003\n",
        );
        // Claim mirrors the row exactly.
        let claims = extract_claims(
            "The primary endpoint was improved in the treatment arm \
             (HR=0.72, p=0.003, primary_endpoint.tsv).",
            &cfg,
        );
        assert!(!claims.is_empty(), "expected at least one extracted claim");
        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert!(report.n_checked > 0);
    }

    // C22 / R-7: pure parser regression. Exercises
    // `parse_table_rows_from_reader` without any fs I/O, confirming the
    // post-extraction split still produces the same TableRow shape as
    // the path-based loader.
    #[test]
    fn parse_table_rows_from_reader_returns_normalized_rows() {
        let tsv = "gene\tlog2FC\tpadj\nACAN\t2.1\t0.001\nCOL2A1\t-1.5\t0.003\n";
        let cols = vec!["gene".to_string()];
        let rows = parse_table_rows_from_reader(tsv.as_bytes(), b'\t', &cols).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].entity, "ACAN");
        assert_eq!(rows[1].entity, "COL2A1");
        // header keys are NFC + ASCII-lowercased
        assert!(rows[0].values.contains_key("log2fc"));
        assert!(rows[0].values.contains_key("padj"));
    }

    #[test]
    fn parse_table_rows_from_reader_csv_delimiter() {
        let csv = "symbol,fc\nFOO,1.5\n";
        let cols = vec!["symbol".to_string()];
        let rows = parse_table_rows_from_reader(csv.as_bytes(), b',', &cols).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].entity, "FOO");
    }

    #[test]
    fn parse_table_rows_from_reader_errors_on_missing_entity_column() {
        // A table with NO configured entity column must ERROR (so the discovery
        // path warns-and-excludes it) rather than fall back to column 0 — the
        // fallback let non-result tables (method_landscape `axis`,
        // mean_variance `feature`) load and produce false Mismatch verdicts.
        let tsv = "some_other\tvalue\nFOO\t1\n";
        let cols = vec!["gene".to_string(), "symbol".to_string()];
        let err = parse_table_rows_from_reader(tsv.as_bytes(), b'\t', &cols).unwrap_err();
        assert!(
            err.to_string().contains("no configured entity column"),
            "expected the no-entity-column error, got: {err}"
        );
    }

    // ── Per-contract dispatch tests (E17) ────────────────────────────────

    /// NumericTableLookup: the pre-existing path — exact cell match is Verified.
    /// The narrative must not contain threshold keywords (padj, FDR, etc.) so that
    /// classify_contract returns NumericTableLookup rather than ThresholdedDeOrEnrichment.
    #[test]
    fn contract_numeric_lookup_verified() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.001\n",
        );
        // No threshold keywords in the sentence → NumericTableLookup fallback.
        let claims = extract_claims("ACAN was upregulated (log2FC=2.1, Table S1).", &cfg);
        let acan = claims.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(
            acan.contract,
            ClaimContract::NumericTableLookup,
            "plain numeric claim without threshold keywords should classify as NumericTableLookup"
        );
        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert!(matches!(
            report
                .verdicts
                .iter()
                .find(|v| v.claim.entity == "ACAN")
                .unwrap()
                .status,
            ClaimStatus::Verified
        ));
    }

    /// ThresholdedDeOrEnrichment: claim with FDR keyword classifies and
    /// verifies when the table p-value is below 0.05.
    #[test]
    fn contract_thresholded_verified_when_pvalue_below_threshold() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.001\n",
        );
        // Sentence contains "FDR" → ThresholdedDeOrEnrichment.
        let claims = extract_claims(
            "ACAN was upregulated with FDR < 0.05 (log2FC=2.1, Table S1).",
            &cfg,
        );
        let acan = claims.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(acan.contract, ClaimContract::ThresholdedDeOrEnrichment);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert!(matches!(
            report
                .verdicts
                .iter()
                .find(|v| v.claim.entity == "ACAN")
                .unwrap()
                .status,
            ClaimStatus::Verified
        ));
    }

    /// ThresholdedDeOrEnrichment: claim fails when observed p-value ≥ 0.05.
    #[test]
    fn contract_thresholded_mismatch_when_pvalue_at_threshold() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // padj = 0.20 — not significant by the FDR < 0.05 threshold.
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.20\n",
        );
        let claims = extract_claims(
            "ACAN was upregulated with FDR < 0.05 (log2FC=2.1, Table S1).",
            &cfg,
        );
        let report = verify_claims(&claims, tmp.path(), &cfg);
        // Either a mismatch on threshold or on the pvalue slot itself —
        // either outcome is a failure.
        let verdict = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(verdict.status, ClaimStatus::Mismatch { .. }),
            "expected Mismatch for above-threshold p-value, got {:?}",
            verdict.status
        );
    }

    /// VF-7 — a bare "significant at FDR < 0.05" claim must be judged on the
    /// ADJUSTED column. A gene with raw p < 0.05 but padj ≥ 0.05 is NOT
    /// FDR-significant; the old raw-first probe passed it silently. The
    /// faithful twin (padj < 0.05) still Verifies.
    #[test]
    fn thresholded_significance_uses_adjusted_not_raw_column() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // Raw pvalue 0.042 (< 0.05) but padj 0.16 (>= 0.05): NOT FDR-significant.
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpvalue\tpadj\nCASP3\t0.9\t0.042\t0.16\n",
        );
        // No quoted p-number → bare-significance probe path.
        let fab = extract_claims(
            "CASP3 was upregulated and significant at FDR < 0.05 (Table S1).",
            &cfg,
        );
        let report = verify_claims(&fab, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "CASP3")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Mismatch { .. }),
            "raw-sig/adj-nonsig FDR claim must be caught on the adjusted column, got {:?}",
            v.status
        );

        // Faithful twin: padj 0.01 (< 0.05) → Verified.
        let tmp2 = tempdir().unwrap();
        write_table(
            tmp2.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpvalue\tpadj\nCASP3\t0.9\t0.042\t0.01\n",
        );
        let faithful = extract_claims(
            "CASP3 was upregulated and significant at FDR < 0.05 (Table S1).",
            &cfg,
        );
        let report2 = verify_claims(&faithful, tmp2.path(), &cfg);
        let v2 = report2
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "CASP3")
            .unwrap();
        assert!(
            matches!(v2.status, ClaimStatus::Verified),
            "faithful FDR-significant claim must Verify, got {:?}",
            v2.status
        );
    }

    /// VF-7 precision: a claim that EXPLICITLY names RAW/nominal significance
    /// ("significant at raw p < 0.05") must be judged on the RAW column, not the
    /// stricter adjusted column — otherwise a correct nominal-significance
    /// statement (raw p < 0.05 while padj >= 0.05) is a false Mismatch. The
    /// adjusted-named claim on the same row stays a Mismatch (the catch).
    #[test]
    fn thresholded_raw_significance_judged_on_raw_column() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // raw pvalue 0.042 (< 0.05) but padj 0.16 (>= 0.05).
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpvalue\tpadj\nCASP3\t0.9\t0.042\t0.16\n",
        );
        // Explicit RAW significance → judged on the raw column → Verified.
        let raw_claim = extract_claims("CASP3 was significant at raw p < 0.05 (Table S1).", &cfg);
        let report = verify_claims(&raw_claim, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "CASP3")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Verified),
            "explicit raw-significance claim must verify on the raw column, got {:?}",
            v.status
        );

        // Same row, but the claim names FDR → judged on padj=0.16 → Mismatch.
        let fdr_claim = extract_claims("CASP3 was significant at FDR < 0.05 (Table S1).", &cfg);
        let report2 = verify_claims(&fdr_claim, tmp.path(), &cfg);
        let v2 = report2
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "CASP3")
            .unwrap();
        assert!(
            matches!(v2.status, ClaimStatus::Mismatch { .. }),
            "FDR-named claim on a raw-only-significant gene must Mismatch, got {:?}",
            v2.status
        );
    }

    /// VF-19 — column-scale classifier. The CRITICAL false-positive guard is
    /// that a log-scaled column containing the word "ratio" (log2_ratio,
    /// tmt_log2_ratio) classifies as Log (pivot 0), NOT Ratio (pivot 1).
    #[test]
    fn vf19_effect_column_scale_classification() {
        for c in [
            "hazard_ratio",
            "HR",
            "odds_ratio",
            "OR",
            "relative_risk",
            "RR",
            "abundance_ratio",
            "fold_change",
            "ratio",
        ] {
            assert_eq!(
                effect_column_scale(c),
                EffectScale::Ratio,
                "{c} should be Ratio"
            );
        }
        for c in [
            "log2_ratio",
            "tmt_log2_ratio",
            "lfq_log2_ratio",
            "log2FC",
            "logFC",
            "nes",
            "NES",
            "estimate",
            "mean_difference",
            "risk_difference",
            "nnt",
            "coefficient",
            "beta",
        ] {
            assert_eq!(
                effect_column_scale(c),
                EffectScale::Log,
                "{c} should be Log"
            );
        }
        assert_eq!(
            observed_effect_direction(0.72, EffectScale::Ratio),
            Some(Direction::Down)
        );
        assert_eq!(
            observed_effect_direction(2.0, EffectScale::Ratio),
            Some(Direction::Up)
        );
        assert_eq!(observed_effect_direction(1.0, EffectScale::Ratio), None);
        assert_eq!(
            observed_effect_direction(0.72, EffectScale::Log),
            Some(Direction::Up)
        );
        assert_eq!(observed_effect_direction(0.0, EffectScale::Log), None);
    }

    /// VF-19 — ratio-column direction end to end. A faithful "reduced
    /// (hazard_ratio=0.72)" must Verify (0.72 < 1 = down); the old pivot-at-0
    /// logic read 0.72 > 0 as "up" and false-Mismatched it. An "increased"
    /// claim on the same row is a genuine Mismatch.
    #[test]
    fn vf19_ratio_direction_verifies_reduced_flags_increased() {
        let policy = serde_json::json!({"verifiableEntities": {
            "enabled": true,
            "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}"],
            "directionVocab": {"up": ["increased", "elevated"], "down": ["reduced", "decreased"]},
            "effectSizeColumns": ["hazard_ratio"],
            "entityColumns": ["gene"],
            "pvalueColumns": ["pvalue", "padj"]
        }});
        let cfg = ExtractorConfig::from_policy(&policy).unwrap();
        let tmp = tempdir().unwrap();
        write_table(tmp.path(), "hr_s1.tsv", "gene\thazard_ratio\nGENE1\t0.72\n");

        let faithful = extract_claims(
            "GENE1 showed reduced hazard (hazard_ratio=0.72, Table S1).",
            &cfg,
        );
        let rv = verify_claims(&faithful, tmp.path(), &cfg);
        let v = rv
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "GENE1")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Verified),
            "reduced hazard with HR<1 must Verify, got {:?}",
            v.status
        );

        let fab = extract_claims(
            "GENE1 showed increased hazard (hazard_ratio=0.72, Table S1).",
            &cfg,
        );
        let rf = verify_claims(&fab, tmp.path(), &cfg);
        let m = rf
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "GENE1")
            .unwrap();
        assert!(
            matches!(m.status, ClaimStatus::Mismatch { .. }),
            "increased hazard with HR<1 must Mismatch, got {:?}",
            m.status
        );
    }

    /// VF-11 — a claim citing a table that does NOT resolve (phantom/garbled
    /// label) falls back to sibling-membership discovery: a real containing
    /// table that contradicts → Mismatch; one that matches → Verified; an entity
    /// in no table but with a quantitative slot → Suspicious. The old behaviour
    /// was a blanket Unverifiable that let the phantom-cite fabrication escape.
    #[test]
    fn vf11_phantom_citation_recovers_via_sibling_discovery() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_a.tsv",
            "gene\tlog2FC\tpadj\nGENE1\t2.0\t0.001\n",
        );
        write_table(
            tmp.path(),
            "de_b.tsv",
            "gene\tlog2FC\tpadj\nGENE2\t-3.0\t0.001\n",
        );

        // CATCH: "Table Z9" resolves to nothing; GENE2 lives in de_b at -3.0
        // (down), contradicting the "upregulated (log2FC=3.0)" claim.
        let fab = extract_claims("GENE2 was upregulated (log2FC=3.0, Table Z9).", &cfg);
        let rf = verify_claims(&fab, tmp.path(), &cfg);
        let m = rf
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "GENE2")
            .unwrap();
        assert!(
            matches!(m.status, ClaimStatus::Mismatch { .. }),
            "phantom-cite contradicting a real sibling must Mismatch, got {:?}",
            m.status
        );

        // FAITHFUL TWIN: same phantom cite, but GENE1's value matches de_a.
        let faithful = extract_claims("GENE1 was upregulated (log2FC=2.0, Table Z9).", &cfg);
        let rv = verify_claims(&faithful, tmp.path(), &cfg);
        let v = rv
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "GENE1")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Verified),
            "phantom-cite matching a real sibling must Verify, got {:?}",
            v.status
        );

        // ABSENT: phantom cite + entity in NO table + quantitative slot →
        // Suspicious (not a free Unverifiable, not a Mismatch).
        let ghost = extract_claims("GHOST9 was upregulated (log2FC=2.0, Table Z9).", &cfg);
        let rg = verify_claims(&ghost, tmp.path(), &cfg);
        let g = rg
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "GHOST9")
            .unwrap();
        assert!(
            matches!(g.status, ClaimStatus::Suspicious { .. }),
            "phantom-cite absent quantitative entity must be Suspicious, got {:?}",
            g.status
        );
    }

    /// VF-16 — aggregate count claims. An inflated count is caught; the exact
    /// count verifies; and every FP guard (hedge, round-number, combined
    /// up/down, uncited) abstains to Unverifiable rather than risking a false
    /// Mismatch against an exact recompute.
    #[test]
    fn vf16_narrative_count_catch_and_abstain_guards() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // 3 up-significant, 2 down-significant at padj < 0.05.
        write_table(
            tmp.path(),
            "de_c.tsv",
            "gene\tlog2FC\tpvalue\tpadj\nU1\t2.0\t1e-8\t1e-6\nU2\t1.5\t2e-8\t2e-6\nU3\t1.2\t3e-8\t3e-6\nD1\t-2.0\t1e-7\t1e-5\nD2\t-1.5\t2e-7\t2e-5\n",
        );
        let run = |t: &str| verify_narrative_counts(t, tmp.path(), tmp.path(), &cfg);

        // CATCH: inflated count (12 up vs 3).
        let fab = run("12 genes were upregulated at FDR < 0.05 (Table C).");
        assert_eq!(fab.len(), 1, "one count verdict expected");
        assert!(
            matches!(fab[0].status, ClaimStatus::Mismatch { .. }),
            "inflated count must Mismatch, got {:?}",
            fab[0].status
        );

        // FAITHFUL: exact count (3 up).
        let ok = run("3 genes were upregulated at FDR < 0.05 (Table C).");
        assert!(
            matches!(ok[0].status, ClaimStatus::Verified),
            "exact count must Verify, got {:?}",
            ok[0].status
        );
        let near = run("4 genes were upregulated at FDR < 0.05 (Table C).");
        assert!(
            matches!(near[0].status, ClaimStatus::Mismatch { .. }),
            "an unhedged integer that differs by one must not verify through a tolerance: {:?}",
            near[0].status
        );
        let inferred = run("3 genes were upregulated at FDR < 0.05.");
        assert!(
            matches!(inferred[0].status, ClaimStatus::Verified),
            "an exact uncited count may infer emitted evidence, got {:?}",
            inferred[0].status
        );
        assert_eq!(inferred[0].claim.source_table.as_deref(), Some("de_c.tsv"));

        let combined_summary = run(
            "Of 5 genes tested, 5 were statistically significant at FDR\n\
             < 0.05 (Table C): 3 were up-regulated and 2 were down-regulated.",
        );
        assert_eq!(
            combined_summary.len(),
            4,
            "tested, significant, up, and down counts must each be retained"
        );
        assert!(
            combined_summary
                .iter()
                .all(|verdict| matches!(verdict.status, ClaimStatus::Verified)),
            "a wrapped combined summary must verify all four counts: {:?}",
            combined_summary
                .iter()
                .map(|verdict| &verdict.status)
                .collect::<Vec<_>>()
        );

        let inherited_bullets = run(
            "Of 5 genes tested, **5 were statistically significant at gene-level \
             FDR (padj) < 0.05**:\n\
             - **3 genes** associated with increased expression (positive log2FC)\n\
             - **2 genes** associated with decreased expression (negative log2FC)",
        );
        assert_eq!(
            inherited_bullets.len(),
            4,
            "tested, significant, inherited-up, and inherited-down counts must be retained: \
             {inherited_bullets:?}"
        );
        assert!(
            inherited_bullets
                .iter()
                .all(|verdict| matches!(verdict.status, ClaimStatus::Verified)),
            "separate directional bullets inherit only an exactly reproduced report threshold: \
             {inherited_bullets:?}"
        );

        for heading in [
            "# Top 10 genes",
            "**Top-hits table (top 10 genes):**",
            "The table below shows the 10 genes.",
            "The table represents the leading 25 of 2,208 up-regulated significant genes.",
            "The primary threshold was 0.25 pathway-level adjusted p-value.",
        ] {
            assert!(
                run(heading).is_empty(),
                "presentation headings must not become aggregate claims: {heading}"
            );
        }

        // ABSTAIN guards — each must be Unverifiable, never Mismatch:
        for (label, text) in [
            (
                "hedge",
                "Approximately 3 genes were upregulated at FDR < 0.05 (Table C).",
            ),
            (
                "round",
                "2000 genes were upregulated at FDR < 0.05 (Table C).",
            ),
            ("uncited", "12 genes were upregulated at FDR < 0.05."),
        ] {
            let v = run(text);
            assert!(
                !v.is_empty()
                    && v.iter()
                        .all(|x| matches!(x.status, ClaimStatus::Unverifiable { .. })),
                "{label} count must abstain (Unverifiable), got {:?}",
                v.iter().map(|x| &x.status).collect::<Vec<_>>()
            );
        }

        // COMBINED up+down in one sentence: directions not separable → abstain.
        let combined =
            run("12 genes were upregulated and 9 were downregulated at FDR < 0.05 (Table C).");
        assert!(
            combined
                .iter()
                .all(|x| matches!(x.status, ClaimStatus::Unverifiable { .. })),
            "combined up/down must abstain, got {:?}",
            combined.iter().map(|x| &x.status).collect::<Vec<_>>()
        );
    }

    /// VF-13 — a gene SYMBOL paired with the WRONG Ensembl id (the
    /// CRISPLD2→wrong-ENSG hallucination class), caught against an INDEPENDENT
    /// reference map. Strictly inert unless a map is configured. Covers the pure
    /// detector (incl. FP guards) and the inert-by-default end-to-end path.
    #[test]
    fn vf13_wrong_symbol_ensembl_pairing() {
        let mut map = BTreeMap::new();
        map.insert("crispld2".to_string(), "ENSG00000103196".to_string());
        map.insert("tp53".to_string(), "ENSG00000141510".to_string());

        // Detector: catches wrong pairing in both apposition orders.
        assert!(detect_wrong_id_pairing("CRISPLD2 (ENSG00000197142) up", &map).is_some());
        assert!(detect_wrong_id_pairing("ENSG00000197142 (CRISPLD2) up", &map).is_some());
        // Abstains: correct pairing, version-suffixed correct, symbol not in the
        // (incomplete) map, and a descriptive non-Ensembl parenthetical.
        assert!(detect_wrong_id_pairing("CRISPLD2 (ENSG00000103196) up", &map).is_none());
        assert!(detect_wrong_id_pairing("CRISPLD2 (ENSG00000103196.12) up", &map).is_none());
        assert!(detect_wrong_id_pairing("NOVELX (ENSG00000999999) up", &map).is_none());
        assert!(detect_wrong_id_pairing("IL6 (interleukin-6) elevated", &map).is_none());

        // Inert by default: no shipped policy configures a map.
        let inert = ExtractorConfig::from_policy(&policy_json()).unwrap();
        assert!(
            inert.gene_annotation_map.is_none(),
            "VF-13 must be inert without a configured map"
        );

        // End to end with a configured map.
        let mut cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        cfg.gene_annotation_map = Some(map);
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nCRISPLD2\t2.6\t1e-60\n",
        );

        let fab = extract_claims(
            "CRISPLD2 (ENSG00000197142) was upregulated (log2FC=2.6, Table S1).",
            &cfg,
        );
        let rf = verify_claims(&fab, tmp.path(), &cfg);
        let m = rf
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "CRISPLD2")
            .unwrap();
        assert!(
            matches!(m.status, ClaimStatus::Mismatch { .. }),
            "wrong symbol↔Ensembl pairing must Mismatch, got {:?}",
            m.status
        );

        // FAITHFUL TWIN: the correct Ensembl id → VF-13 abstains, normal
        // verification runs and Verifies.
        let faithful = extract_claims(
            "CRISPLD2 (ENSG00000103196) was upregulated (log2FC=2.6, Table S1).",
            &cfg,
        );
        let rv = verify_claims(&faithful, tmp.path(), &cfg);
        let v = rv
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "CRISPLD2")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Verified),
            "correct symbol↔Ensembl pairing must Verify, got {:?}",
            v.status
        );
    }

    /// FP-A — a gene-SET / pathway count with "enriched" must count ALL
    /// significant sets (either NES sign), not just positive-NES ones. "enriched"
    /// for a set is significance, NOT a gene-level up direction. (Exposed on the
    /// fresh Himes GSEA run: "453 enriched gene sets (padj<0.05)" wrongly read as
    /// 334 = padj<0.05 AND NES>0.)
    #[test]
    fn fpa_enriched_set_count_is_significance_not_direction() {
        let policy = serde_json::json!({"verifiableEntities": {
            "enabled": true,
            "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}"],
            "directionVocab": {"up": ["enriched", "increased"], "down": ["depleted", "decreased"]},
            "effectSizeColumns": ["NES"],
            "entityColumns": ["pathway", "term"],
            "pvalueColumns": ["padj", "pval"]
        }});
        let cfg = ExtractorConfig::from_policy(&policy).unwrap();
        let tmp = tempdir().unwrap();
        // 3 significant at padj<0.05: 2 positive-NES, 1 negative-NES.
        write_table(
            tmp.path(),
            "gsea.tsv",
            "pathway\tNES\tpval\tpadj\nPONE\t2.1\t1e-5\t1e-3\nPTWO\t1.5\t2e-5\t2e-3\nPTHREE\t-1.8\t3e-5\t3e-3\n",
        );
        let path = tmp.path().join("gsea.tsv");
        // Faithful: all 3 significant sets counted as "enriched" → Verified.
        let s = verify_count_claim(
            "3 gene sets were significantly enriched (padj < 0.05)",
            &path,
            &cfg,
        );
        assert!(
            matches!(s, Some(ClaimStatus::Verified)),
            "all-significant set count must Verify (not filter to NES>0), got {s:?}"
        );
        // Inflated count is still caught.
        let s2 = verify_count_claim(
            "9 gene sets were significantly enriched (padj < 0.05)",
            &path,
            &cfg,
        );
        assert!(
            matches!(s2, Some(ClaimStatus::Mismatch { .. })),
            "inflated set count must still Mismatch, got {s2:?}"
        );
    }

    /// FP-B — a `file::json-pointer` evidence reference (a self-citation to a
    /// JSON field that exists) must NOT be a phantom-file Mismatch; it is an
    /// Unverifiable self-reference. A pointer whose base file is genuinely absent
    /// is still a Mismatch.
    #[test]
    fn fpb_json_pointer_evidence_is_not_phantom() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let pkg = tempdir().unwrap();
        let dir = pkg.path().join("runtime").join("outputs").join("de");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("result.json"),
            r#"{"top_effect_abundance_ratio": 0.208}"#,
        )
        .unwrap();

        let sc = StructuredClaim {
            claim: "Top genes have median base_mean ratio = 0.208".into(),
            evidence: Some("result.json::top_effect_abundance_ratio".into()),
        };
        let v = verify_one_structured(&sc, pkg.path(), &cfg);
        assert!(
            matches!(v.status, ClaimStatus::Pending { .. }),
            "file::pointer self-reference to an existing field is never-adjudicated → Pending, got {:?}",
            v.status
        );

        let sc2 = StructuredClaim {
            claim: "x".into(),
            evidence: Some("ghost_nowhere.json::field".into()),
        };
        let v2 = verify_one_structured(&sc2, pkg.path(), &cfg);
        assert!(
            matches!(v2.status, ClaimStatus::Mismatch { .. }),
            "pointer into a genuinely-absent base file must Mismatch, got {:?}",
            v2.status
        );
    }

    /// VF-8 — p-value laundering. A narrative that quotes a value under an
    /// ADJUSTED label ("padj=…") which actually matches only the table's RAW
    /// `pvalue` column (the adjusted column disagrees by orders of magnitude)
    /// is a fabrication: the author dressed the smaller raw p as an adjusted
    /// one to inflate significance. The old lenient "match ANY p-column" rule
    /// passed it. Three twins prove the catch is asymmetric and FP-safe.
    #[test]
    fn pvalue_laundering_raw_quoted_as_adjusted_is_mismatch() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();

        // CATCH: padj column is 0.45 (not significant) but the raw pvalue is
        // 0.0001; the narrative quotes 0.0001 under the "padj" label.
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.1\t0.0001\t0.45\n",
        );
        let fab = extract_claims(
            "ACAN was upregulated (log2FC=2.1, padj=0.0001, Table S1).",
            &cfg,
        );
        let acan = fab.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(
            acan.matched_pvalue_keyword.as_deref(),
            Some("padj"),
            "extractor should record the adjusted keyword the prose used"
        );
        let report = verify_claims(&fab, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Mismatch { .. }),
            "raw p quoted under an adjusted label must be a Mismatch, got {:?}",
            v.status
        );

        // FAITHFUL TWIN 1: the padj column genuinely matches the quoted value
        // (raw is smaller). Honest adjusted claim → Verified.
        let tmp2 = tempdir().unwrap();
        write_table(
            tmp2.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.1\t0.00005\t0.0001\n",
        );
        let honest = extract_claims(
            "ACAN was upregulated (log2FC=2.1, padj=0.0001, Table S1).",
            &cfg,
        );
        let report2 = verify_claims(&honest, tmp2.path(), &cfg);
        let v2 = report2
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(v2.status, ClaimStatus::Verified),
            "honest adjusted claim matching the padj column must Verify, got {:?}",
            v2.status
        );

        // FAITHFUL TWIN 2 (asymmetry): the SAME raw value quoted under a RAW
        // label ("pvalue=…") is legitimate — quoting the raw p is not laundering
        // even when padj is large. Must Verify, proving VF-8 only fires in the
        // significance-inflating direction.
        let tmp3 = tempdir().unwrap();
        write_table(
            tmp3.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.1\t0.0001\t0.45\n",
        );
        let raw_label = extract_claims(
            "ACAN was upregulated (log2FC=2.1, pvalue=0.0001, Table S1).",
            &cfg,
        );
        let rl = raw_label.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(
            rl.matched_pvalue_keyword.as_deref(),
            Some("pvalue"),
            "extractor should record the raw keyword the prose used"
        );
        let report3 = verify_claims(&raw_label, tmp3.path(), &cfg);
        let v3 = report3
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(v3.status, ClaimStatus::Verified),
            "raw value under a raw label is not laundering and must Verify, got {:?}",
            v3.status
        );
    }

    /// VF-4 — linear-fold magnitude. A narrative that states a LINEAR fold
    /// ("upregulated 10-fold") which grossly disagrees with the table's
    /// |log2FC| is a magnitude fabrication that otherwise escapes the verifier
    /// (the effect-size slot only parses "log2FC=…"). Faithful twins (honest
    /// fold; "log2 fold change" effect-size phrasing) prove the catch is
    /// FP-safe and does not collide with the log2 keyword.
    #[test]
    fn linear_fold_magnitude_overclaim_is_mismatch() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();

        // CATCH: prose claims 10-fold (log2 ≈ 3.32) but the table shows
        // log2FC = 0.5 (≈ 1.4×) — off by far more than one doubling.
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t0.5\t0.001\n",
        );
        let fab = extract_claims("ACAN was upregulated 10-fold (Table S1).", &cfg);
        let acan = fab.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(
            acan.linear_fold,
            Some(10.0),
            "extractor should parse the linear fold magnitude from prose"
        );
        let report = verify_claims(&fab, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Mismatch { .. }),
            "gross linear-fold magnitude overclaim must be a Mismatch, got {:?}",
            v.status
        );

        // FAITHFUL TWIN 1: an honest 8-fold claim against log2FC = 3.0 (= 8×).
        let tmp2 = tempdir().unwrap();
        write_table(
            tmp2.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t3.0\t0.001\n",
        );
        let honest = extract_claims("ACAN was upregulated 8-fold (Table S1).", &cfg);
        let report2 = verify_claims(&honest, tmp2.path(), &cfg);
        let v2 = report2
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(v2.status, ClaimStatus::Verified),
            "honest linear-fold claim within band must Verify, got {:?}",
            v2.status
        );

        // FAITHFUL TWIN 2 (keyword exclusion): "log2 fold change of 3.0" is an
        // effect-size phrase (VF-3), NOT a 2× linear claim — the "2" in "log2"
        // must not be parsed as a linear fold. Effect size 3.0 matches → Verified.
        let tmp3 = tempdir().unwrap();
        write_table(
            tmp3.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t3.0\t0.001\n",
        );
        let log2_phrase = extract_claims(
            "ACAN was upregulated with a log2 fold change of 3.0 (Table S1).",
            &cfg,
        );
        let lp = log2_phrase.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(
            lp.linear_fold, None,
            "the '2' in 'log2 fold change' must not be parsed as a linear fold"
        );
        let report3 = verify_claims(&log2_phrase, tmp3.path(), &cfg);
        let v3 = report3
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(v3.status, ClaimStatus::Verified),
            "log2-fold-change effect-size phrasing must Verify, got {:?}",
            v3.status
        );
    }

    /// RankTopN: entity in top-5 rows → Verified.
    #[test]
    fn contract_rank_top_n_entity_in_top5_verified() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // ACAN is the first row — rank 1.
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t3.0\t0.001\nCOL2A1\t2.0\t0.002\nTNF\t1.0\t0.01\n",
        );
        let claims = extract_claims("ACAN is in the top-5 hits (Table S1).", &cfg);
        let acan = claims.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(acan.contract, ClaimContract::RankTopN);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert!(matches!(
            report
                .verdicts
                .iter()
                .find(|v| v.claim.entity == "ACAN")
                .unwrap()
                .status,
            ClaimStatus::Verified
        ));
    }

    /// RankTopN regression: a non-finite effect size (e.g. an "NA" cell) in the
    /// table must NOT panic the top-N sort. Before the fix the comparator used
    /// `partial_cmp().unwrap_or(Equal)`, which makes NaN compare Equal to
    /// everything — an invalid total order that Rust 1.81+ panics on
    /// ("user-provided comparison function does not correctly implement a total
    /// order"). The fix drops non-finite effect sizes and sorts with total_cmp.
    #[test]
    fn contract_rank_top_n_does_not_panic_on_non_finite_effect_size() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // One real top hit + several rows whose log2FC is "NA" (parses to NaN).
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t3.0\t0.001\nA\tNA\t0.2\nB\tNA\t0.3\nC\tNA\t0.4\nD\tNA\t0.5\n",
        );
        let claims = extract_claims("ACAN is in the top-5 hits (Table S1).", &cfg);
        let acan = claims.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(acan.contract, ClaimContract::RankTopN);
        // Must complete without panicking; ACAN (the only finite row) is top-N.
        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert!(matches!(
            report
                .verdicts
                .iter()
                .find(|v| v.claim.entity == "ACAN")
                .unwrap()
                .status,
            ClaimStatus::Verified
        ));
    }

    /// RankTopN: entity not in top-2 rows → Mismatch.
    #[test]
    fn contract_rank_top_n_entity_outside_top_mismatch() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // TNF is 3rd — not in top-2.
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t3.0\t0.001\nCOL2A1\t2.0\t0.002\nTNF\t1.0\t0.01\n",
        );
        let claims = extract_claims("TNF is in the top-2 hits (Table S1).", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let verdict = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "TNF")
            .unwrap();
        assert!(
            matches!(verdict.status, ClaimStatus::Mismatch { .. }),
            "TNF not in top-2; expected Mismatch, got {:?}",
            verdict.status
        );
    }

    /// RankTopN must rank by |effect size|, NOT trust physical row order.
    /// This table is sorted by gene name (alphabetical), so row order does
    /// not match |log2FC| order. The true |log2FC| ranking is
    /// ZED(5.0) > ALK(4.0) > BAR(2.0) > COL2A1(0.5). A "top-2" claim about
    /// ALK (physically the FIRST row, but rank 2 by |log2FC|) must verify,
    /// and a "top-2" claim about BAR (physically 2nd row, but rank 3) must
    /// be rejected — exactly the case the old row-order code got wrong.
    #[test]
    fn rank_top_n_ranks_by_effect_size_not_row_order() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // Alphabetical row order — NOT sorted by |log2FC|.
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nALK\t4.0\t0.01\nBAR\t2.0\t0.02\nCOL2A1\t0.5\t0.03\nZED\t5.0\t0.04\n",
        );

        // ALK is physically first but is rank 2 by |log2FC| — in the top-2.
        let alk_claims = extract_claims("ALK is in the top-2 hits (Table S1).", &cfg);
        let alk = alk_claims.iter().find(|c| c.entity == "ALK").unwrap();
        assert_eq!(alk.contract, ClaimContract::RankTopN);
        let alk_report = verify_claims(&alk_claims, tmp.path(), &cfg);
        let alk_v = alk_report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ALK")
            .unwrap();
        assert!(
            matches!(alk_v.status, ClaimStatus::Verified),
            "ALK ranks 2 by |log2FC|; expected Verified, got {:?}",
            alk_v.status
        );

        // BAR is physically 2nd but is rank 3 by |log2FC| — NOT in top-2.
        // The old row-order code would have falsely confirmed this.
        let bar_claims = extract_claims("BAR is in the top-2 hits (Table S1).", &cfg);
        let bar = bar_claims.iter().find(|c| c.entity == "BAR").unwrap();
        assert_eq!(bar.contract, ClaimContract::RankTopN);
        let bar_report = verify_claims(&bar_claims, tmp.path(), &cfg);
        let bar_v = bar_report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "BAR")
            .unwrap();
        assert!(
            matches!(bar_v.status, ClaimStatus::Mismatch { .. }),
            "BAR ranks 3 by |log2FC|; expected Mismatch, got {:?}",
            bar_v.status
        );
    }

    /// VF-6 — RankTopN must cross-check a stated DIRECTION against the named
    /// entity's own sign. A "top-N most UP-regulated" claim naming a large
    /// NEGATIVE gene (in the top-N by magnitude, opposite by sign) is a
    /// fabrication; a faithful one (positive gene) still Verifies.
    #[test]
    fn rank_top_n_direction_must_match_entity_sign() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // DOWN has the largest |log2FC| (down); UP is a positive top hit.
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nDOWN\t-5.0\t0.01\nUP\t4.0\t0.02\nMID\t1.0\t0.03\n",
        );

        // Fabrication: DOWN is rank-1 by |effect| so it IS "in the top-2", but
        // it is DOWN-regulated — calling it "upregulated" must be caught.
        let fab = extract_claims(
            "DOWN is among the top-2 upregulated genes (Table S1).",
            &cfg,
        );
        let dc = fab.iter().find(|c| c.entity == "DOWN").unwrap();
        assert_eq!(dc.contract, ClaimContract::RankTopN);
        let report = verify_claims(&fab, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "DOWN")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Mismatch { .. }),
            "top-N claim with wrong direction must be caught, got {:?}",
            v.status
        );

        // Faithful twin: UP is positive and in the top-2 → Verified.
        let faithful = extract_claims("UP is among the top-2 upregulated genes (Table S1).", &cfg);
        let report2 = verify_claims(&faithful, tmp.path(), &cfg);
        let v2 = report2
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "UP")
            .unwrap();
        assert!(
            matches!(v2.status, ClaimStatus::Verified),
            "faithful top-N upregulated claim must Verify, got {:?}",
            v2.status
        );
    }

    /// RankTopN: claimed entity present but lacking a numeric effect size
    /// cannot be ranked → Unverifiable (not a silent keep / false confirm).
    #[test]
    fn rank_top_n_entity_without_effect_size_is_unverifiable() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // GAP has no parseable log2FC value.
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nALK\t4.0\t0.01\nGAP\tNA\t0.02\nZED\t5.0\t0.04\n",
        );
        let claims = extract_claims("GAP is in the top-5 hits (Table S1).", &cfg);
        let gap = claims.iter().find(|c| c.entity == "GAP").unwrap();
        assert_eq!(gap.contract, ClaimContract::RankTopN);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "GAP")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Unverifiable { .. }),
            "GAP has no effect size; expected Unverifiable, got {:?}",
            v.status
        );
    }

    /// Build a DE table whose `n_rows` genes are ranked by |log2FC| descending:
    /// row `i` (0-based) is gene `Gi` with `|log2FC| = (n_rows - i) * 0.001`, so
    /// `G0` is rank 1 and `G{n_rows-1}` is rank `n_rows`. The named `verb` is
    /// just used to seed a deterministic effect-size spread. Returns the TSV.
    fn ranked_de_table(n_rows: usize) -> String {
        let mut body = String::from("gene\tlog2FC\tpadj\n");
        for i in 0..n_rows {
            // Strictly decreasing |log2FC|; positive so direction is "up".
            let mag = (n_rows - i) as f64 * 0.001;
            body.push_str(&format!("G{i}\t{mag:.5}\t0.01\n"));
        }
        body
    }

    /// SOFT top-N (the false positive being fixed): a vague "one of the top DE
    /// genes" claim with NO explicit number, naming a gene at rank ~31 in a
    /// ~3,200-row table. Rank 31 is OUTSIDE the strict top-10 (which the old
    /// code used) but well INSIDE the top 1% (`ceil(0.01 × 3200) = 32`), so the
    /// generous percentile floor must now Verify it. This mirrors the executed
    /// Himes package, where CRISPLD2 ranks 31 of 22,369 tested genes.
    #[test]
    fn soft_top_n_high_ranked_gene_verifies() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // 3,200 ranked rows → soft floor = max(10, ceil(0.01*3200)) = 32.
        // G30 is the 31st row → rank 31: outside top-10, inside top-32.
        write_table(tmp.path(), "de_s1.tsv", &ranked_de_table(3200));
        let claims = extract_claims("G30 is one of the top DE genes (Table S1).", &cfg);
        let g30 = claims.iter().find(|c| c.entity == "G30").unwrap();
        assert_eq!(
            g30.contract,
            ClaimContract::RankTopN,
            "a hedged `one of the top` with no number must route to RankTopN"
        );
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "G30")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Verified),
            "G30 at rank 31 of 3,200 (top 1%) called `one of the top` must Verify under the percentile floor, got {:?}",
            v.status
        );
    }

    /// SOFT top-N genuine overclaim: the same ~3,200-row table, but the named
    /// gene sits at rank 100 — well OUTSIDE the top 1% cutoff (32). A vague "one
    /// of the top" claim about a genuinely low-ranked gene must still Mismatch;
    /// the percentile relaxation does not wave low ranks through.
    #[test]
    fn soft_top_n_genuine_overclaim_still_mismatches() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // Same 3,200-row table; soft floor = 32. G99 is rank 100 → outside it.
        write_table(tmp.path(), "de_s1.tsv", &ranked_de_table(3200));
        let claims = extract_claims("G99 is one of the top DE genes (Table S1).", &cfg);
        let g99 = claims.iter().find(|c| c.entity == "G99").unwrap();
        assert_eq!(g99.contract, ClaimContract::RankTopN);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "G99")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Mismatch { .. }),
            "G99 at rank 100 of 3,200 (beyond the top 1%) called `one of the top` must still Mismatch, got {:?}",
            v.status
        );
    }

    /// EXPLICIT "top N" is NOT relaxed: a claim that names a number ("in the top
    /// 5") holds the entity to that exact N. The percentile floor applies ONLY
    /// to soft, no-number claims. Here G7 is rank 8 — outside the explicit top
    /// 5 — and must Mismatch even on a table large enough that the soft floor
    /// (which an explicit claim must NOT use) would have admitted it.
    #[test]
    fn explicit_top_n_unchanged() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // 3,200 rows → soft floor would be 32, which WOULD admit rank 8; the
        // explicit "top 5" must override that and reject rank 8.
        write_table(tmp.path(), "de_s1.tsv", &ranked_de_table(3200));
        let claims = extract_claims("G7 is in the top 5 hits (Table S1).", &cfg);
        let g7 = claims.iter().find(|c| c.entity == "G7").unwrap();
        assert_eq!(
            g7.contract,
            ClaimContract::RankTopN,
            "an explicit `top 5` must route to RankTopN"
        );
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "G7")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Mismatch { .. }),
            "G7 at rank 8 vs an EXPLICIT top 5 must Mismatch (explicit N is not relaxed), got {:?}",
            v.status
        );
    }

    /// Direction cross-check: an "upregulated" claim on a row whose observed
    /// effect size is exactly 0.0 must NOT be confirmed — zero change agrees
    /// with neither Up nor Down. Guards against the `obs >= 0.0 => Up` bug.
    #[test]
    fn direction_zero_effect_size_does_not_confirm_upregulated() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nFLAT\t0.0\t0.001\n",
        );
        let claim = Claim {
            entity: "FLAT".into(),
            direction: Some(Direction::Up),
            effect_size: None,
            pvalue: None,
            source_table: Some("de_s1.tsv".into()),
            excerpt: "FLAT was upregulated (Table S1).".into(),
            contract: ClaimContract::NumericTableLookup,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let report = verify_claims(&[claim], tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "FLAT")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Mismatch { .. }),
            "log2FC=0.0 is not upregulated; expected Mismatch, got {:?}",
            v.status
        );
    }

    /// Companion to the zero-effect direction guard: a strictly positive
    /// effect size still confirms an "upregulated" claim (no over-correction).
    #[test]
    fn direction_positive_effect_size_confirms_upregulated() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nRISE\t1.2\t0.001\n",
        );
        let claim = Claim {
            entity: "RISE".into(),
            direction: Some(Direction::Up),
            effect_size: None,
            pvalue: None,
            source_table: Some("de_s1.tsv".into()),
            excerpt: "RISE was upregulated (Table S1).".into(),
            contract: ClaimContract::NumericTableLookup,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let report = verify_claims(&[claim], tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "RISE")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Verified),
            "log2FC=1.2 is upregulated; expected Verified, got {:?}",
            v.status
        );
    }

    /// GroupComparison: direction word "higher than" → GroupComparison contract,
    /// verifies when table effect size is positive.
    #[test]
    fn contract_group_comparison_direction_verified() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.001\n",
        );
        let claims = extract_claims(
            "ACAN expression was higher than controls (log2FC=2.1, Table S1).",
            &cfg,
        );
        let acan = claims.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(acan.contract, ClaimContract::GroupComparison);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert!(matches!(
            report
                .verdicts
                .iter()
                .find(|v| v.claim.entity == "ACAN")
                .unwrap()
                .status,
            ClaimStatus::Verified
        ));
    }

    /// Categorical: cluster label found in excerpt → Verified.
    #[test]
    fn contract_categorical_label_in_excerpt_verified() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // Table has a "label" column with value "cardiomyocytes".
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlabel\tlog2FC\nACAN\tcardiomyocytes\t1.2\n",
        );
        let claims = extract_claims(
            "Cluster 5 was identified as cardiomyocytes based on ACAN expression (Table S1).",
            &cfg,
        );
        let acan = claims.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(acan.contract, ClaimContract::Categorical);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert!(matches!(
            report
                .verdicts
                .iter()
                .find(|v| v.claim.entity == "ACAN")
                .unwrap()
                .status,
            ClaimStatus::Verified
        ));
    }

    /// Categorical-evasion regression (the proven scenario-11 false negative):
    /// a "marker" sentence routes to the Categorical contract, but when the
    /// cited table has NO label column the verifier must STILL check the stated
    /// direction against the table — not fall through to a blanket Verified. A
    /// planted sign-flip ("ACAN was upregulated as a marker"; ACAN is -2.8/down)
    /// must Mismatch; the faithful twin ("downregulated as a marker") must
    /// Verify (no false positive).
    #[test]
    fn categorical_no_label_column_still_checks_direction() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // DE table — gene/log2FC/padj, NO label/type/cluster column.
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t-2.8\t0.001\n",
        );

        // Fabrication: claims UP, table is DOWN → must be caught.
        let fab = extract_claims(
            "ACAN was upregulated as a marker of healthy phenotype (Table S1).",
            &cfg,
        );
        let acan = fab.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(acan.contract, ClaimContract::Categorical);
        let report = verify_claims(&fab, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Mismatch { .. }),
            "planted categorical sign-flip must be caught, got {:?}",
            v.status
        );

        // Faithful twin: claims DOWN, table is DOWN → must Verify (no FP).
        let faithful = extract_claims(
            "ACAN was downregulated as a marker of healthy phenotype (Table S1).",
            &cfg,
        );
        let report2 = verify_claims(&faithful, tmp.path(), &cfg);
        let v2 = report2
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(v2.status, ClaimStatus::Verified),
            "faithful categorical direction must stay Verified, got {:?}",
            v2.status
        );
    }

    /// TimeSeriesSummary: entity in table, time value mentioned in excerpt → Verified.
    /// Narrative must not contain threshold keywords (padj, FDR) because those
    /// fire at higher priority than the time-series patterns in classify_contract.
    #[test]
    fn contract_time_series_peak_day_verified() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // Table has a "day" column with value "14".
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tday\tlog2FC\nACAN\t14\t2.1\n",
        );
        // No threshold keyword — "day 14" triggers TimeSeriesSummary.
        let claims = extract_claims("ACAN peaked at day 14 (log2FC=2.1, Table S1).", &cfg);
        let acan = claims.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(acan.contract, ClaimContract::TimeSeriesSummary);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert!(matches!(
            report
                .verdicts
                .iter()
                .find(|v| v.claim.entity == "ACAN")
                .unwrap()
                .status,
            ClaimStatus::Verified
        ));
    }

    /// Edge: contract field round-trips through JSON serialization.
    #[test]
    fn contract_field_serializes_and_deserializes() {
        use crate::claim_contract::ClaimContract;
        use crate::claim_extractor::Claim;
        let claim = Claim {
            entity: "TNF".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: None,
            excerpt: "TNF was elevated".into(),
            contract: ClaimContract::GroupComparison,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let json = serde_json::to_string(&claim).unwrap();
        let round_tripped: Claim = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped.contract, ClaimContract::GroupComparison);
    }

    /// Edge: old JSON without `contract` field deserializes to NumericTableLookup.
    #[test]
    fn contract_field_defaults_on_old_json() {
        use crate::claim_contract::ClaimContract;
        use crate::claim_extractor::Claim;
        // Simulate a serialized Claim from before the `contract` field was added.
        let old_json = r#"{"entity":"ACAN","excerpt":"ACAN was upregulated"}"#;
        let claim: Claim = serde_json::from_str(old_json).unwrap();
        assert_eq!(
            claim.contract,
            ClaimContract::NumericTableLookup,
            "missing field should default to NumericTableLookup"
        );
    }

    // ── Structured / count / discovery coverage ───────────────────────────

    fn write_pkg_table(root: &Path, task: &str, name: &str, body: &str) {
        let dir = root.join("runtime").join("outputs").join(task);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn structured_count_claim_verified_and_fabricated_mismatch() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // 3 of 4 genes have padj < 0.05.
        write_pkg_table(
            tmp.path(),
            "differential_expression",
            "de.tsv",
            "gene\tlog2FC\tpadj\nA\t2.0\t0.001\nB\t-1.0\t0.02\nC\t1.0\t0.049\nD\t0.1\t0.5\n",
        );
        let good = StructuredClaim {
            claim: "3 genes are differentially expressed (padj < 0.05)".into(),
            evidence: Some("de.tsv".into()),
        };
        let bad = StructuredClaim {
            claim: "9999 genes are differentially expressed (padj < 0.05)".into(),
            evidence: Some("de.tsv".into()),
        };
        let v = verify_structured_claims(&[good, bad], tmp.path(), &cfg);
        assert!(
            matches!(v[0].status, ClaimStatus::Verified),
            "{:?}",
            v[0].status
        );
        assert!(
            matches!(v[1].status, ClaimStatus::Mismatch { .. }),
            "fabricated count must mismatch: {:?}",
            v[1].status
        );
    }

    #[test]
    fn vf1_structured_phantom_evidence_file_is_mismatch() {
        // VF-1 — a structured claim citing an evidence file that exists NOWHERE
        // in the package is a fabricated citation → Mismatch. A claim citing a
        // real file present in an unscanned subdir is only a resolution gap →
        // Unverifiable (no false flag).
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_pkg_table(
            tmp.path(),
            "differential_expression",
            "de.tsv",
            "gene\tlog2FC\tpadj\nA\t2.0\t0.001\n",
        );
        // Phantom: cites a file that is not anywhere in the package.
        let phantom = StructuredClaim {
            claim: "5 genes are differentially expressed (padj < 0.05)".into(),
            evidence: Some("ghost_table.tsv".into()),
        };
        let v = verify_structured_claims(&[phantom], tmp.path(), &cfg);
        assert!(
            matches!(v[0].status, ClaimStatus::Mismatch { .. }),
            "phantom evidence file must be a Mismatch, got {:?}",
            v[0].status
        );

        // Resolution-gap twin: the file EXISTS (in an intermediates subdir the
        // resolver does not scan) → Unverifiable, not Mismatch.
        let nested = tmp
            .path()
            .join("runtime/outputs/differential_expression/intermediates");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(
            nested.join("aux.tsv"),
            "gene\tlog2FC\tpadj\nA\t2.0\t0.001\n",
        )
        .unwrap();
        let present = StructuredClaim {
            claim: "5 genes are differentially expressed (padj < 0.05)".into(),
            evidence: Some("aux.tsv".into()),
        };
        let v2 = verify_structured_claims(&[present], tmp.path(), &cfg);
        assert!(
            matches!(v2[0].status, ClaimStatus::Pending { .. }),
            "present-but-unresolved evidence is a never-adjudicated resolution gap → Pending (not Mismatch, not Unverifiable), got {:?}",
            v2[0].status
        );
    }

    #[test]
    fn count_claim_uses_named_pvalue_column_not_raw() {
        // padj<0.05 count must use the adjusted column, not raw pvalue
        // (which would over-count). 1 row has padj<0.05; 3 have raw p<0.05.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_pkg_table(
            tmp.path(),
            "de",
            "de.tsv",
            "gene\tlog2FC\tpvalue\tpadj\nA\t2.0\t0.001\t0.01\nB\t1.0\t0.01\t0.6\nC\t1.0\t0.02\t0.7\n",
        );
        let claim = StructuredClaim {
            claim: "1 gene is significant at padj < 0.05".into(),
            evidence: Some("de.tsv".into()),
        };
        let v = verify_structured_claims(&[claim], tmp.path(), &cfg);
        assert!(
            matches!(v[0].status, ClaimStatus::Verified),
            "{:?}",
            v[0].status
        );
    }

    #[test]
    fn count_claim_excludes_na_adjusted_rows_no_raw_fallthrough() {
        // Regression for the NA-`padj` fall-through (Himes airway: 4017 → 4146).
        // DESeq2 sets `padj` = NA for independent-filtered genes. A `padj<0.05`
        // count must EXCLUDE those rows, never fall through per-row to the raw
        // `pvalue` column. Here exactly 2 rows have `padj<0.05` (A, B); two more
        // (D, E) have NA `padj` but raw `pvalue<0.05` — they must NOT be counted.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_pkg_table(
            tmp.path(),
            "de",
            "de.tsv",
            "gene\tlog2FC\tpvalue\tpadj\n\
             A\t2.0\t0.0001\t0.001\n\
             B\t1.5\t0.0002\t0.01\n\
             C\t1.0\t0.02\t0.7\n\
             D\t1.0\t0.001\tNA\n\
             E\t1.0\t0.002\tNA\n",
        );
        let claim = StructuredClaim {
            claim: "2 genes are significant at padj < 0.05".into(),
            evidence: Some("de.tsv".into()),
        };
        let v = verify_structured_claims(&[claim], tmp.path(), &cfg);
        assert!(
            matches!(v[0].status, ClaimStatus::Verified),
            "NA-padj rows D,E must not fall through to raw pvalue; got {:?}",
            v[0].status
        );
    }

    #[test]
    fn per_entity_pvalue_matches_adjusted_column_when_both_present() {
        // Narrative quotes padj; table carries both raw pvalue (far smaller)
        // and padj. Must verify against padj, not false-mismatch on raw.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpvalue\tpadj\nACAN\t2.1\t1.7e-9\t1.49e-5\n",
        );
        let claims = extract_claims(
            "ACAN was upregulated (log2FC=2.1, padj=1.49e-5, Table S1).",
            &cfg,
        );
        let report = verify_claims(&claims, tmp.path(), &cfg);
        assert_eq!(report.n_mismatch, 0, "{:?}", report.verdicts);
        assert_eq!(report.n_verified, 1);
    }

    #[test]
    fn distinct_count_grouping_claim() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // 6 distinct cluster labels.
        let mut body = String::from("gene\tcluster\n");
        for i in 0..30 {
            body.push_str(&format!("G{i}\t{}\n", i % 6));
        }
        write_pkg_table(tmp.path(), "clustering", "clusters.tsv", &body);
        let good = StructuredClaim {
            claim: "6 clusters identified at resolution=1.0".into(),
            evidence: Some("clusters.tsv".into()),
        };
        let bad = StructuredClaim {
            claim: "20 clusters identified at resolution=1.0".into(),
            evidence: Some("clusters.tsv".into()),
        };
        let v = verify_structured_claims(&[good, bad], tmp.path(), &cfg);
        assert!(
            matches!(v[0].status, ClaimStatus::Verified),
            "{:?}",
            v[0].status
        );
        assert!(
            matches!(v[1].status, ClaimStatus::Mismatch { .. }),
            "{:?}",
            v[1].status
        );
    }

    /// Build an `ExtractorConfig` from the test `policy_json()` with its entity
    /// columns overridden so a `gene_id`-headed DE table loads (mirrors the
    /// production A1 policy edit without touching the committed policy file).
    fn cfg_with_entity_cols(cols: &[&str]) -> ExtractorConfig {
        let mut cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        cfg.entity_columns = cols.iter().map(|c| c.to_string()).collect();
        cfg
    }

    #[test]
    fn discovery_skips_unloadable_table_but_uses_good_sibling() {
        // A candidate table that fails to load (genuinely unparseable bytes)
        // must be excluded *and logged* (A2), and discovery must still resolve
        // the claim against a good sibling table rather than reporting it
        // Unverifiable because of the bad sibling.
        let tmp = tempdir().unwrap();
        let outputs = tmp.path().join("runtime").join("outputs").join("de");
        std::fs::create_dir_all(&outputs).unwrap();
        // Bad table: invalid UTF-8 header bytes — `load_table_rows` errors, so
        // this exercises the A2 warn-and-exclude path rather than silently
        // masquerading as "entity absent".
        std::fs::write(outputs.join("broken.tsv"), b"\xff\xfe\tcol\nX\t1\n").unwrap();
        // Good table: gene_id + log2FC + padj, containing CRISPLD2 upregulated.
        std::fs::write(
            outputs.join("de_results.tsv"),
            "gene_id\tlog2FC\tpadj\nCRISPLD2\t2.6\t1e-60\n",
        )
        .unwrap();
        let cfg = cfg_with_entity_cols(&["gene_id", "gene"]);
        let claim = Claim {
            entity: "CRISPLD2".into(),
            direction: Some(Direction::Up),
            effect_size: Some(2.6),
            pvalue: None,
            source_table: None,
            excerpt: "CRISPLD2 was upregulated".into(),
            contract: ClaimContract::NumericTableLookup,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let v = verify_claims_with_discovery(&[claim], tmp.path(), tmp.path(), &cfg);
        assert!(
            matches!(
                v[0].status,
                ClaimStatus::Verified | ClaimStatus::Mismatch { .. }
            ),
            "CRISPLD2 must resolve against de_results.tsv even though broken.tsv \
             fails to load; got {:?}",
            v[0].status
        );
    }

    #[test]
    fn discovery_prefers_any_agreeing_table() {
        // Entity present in two tables with different values; the claim
        // matches one. Discovery must return Verified (not a false
        // mismatch against the disagreeing duplicate).
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_pkg_table(
            tmp.path(),
            "a",
            "de_results.tsv",
            "gene\tlog2FC\tpadj\nACAN\t2.10\t0.001\n",
        );
        write_pkg_table(
            tmp.path(),
            "a",
            "de_table.tsv",
            "gene\tlog2FC\tpadj\nACAN\t9.90\t0.5\n",
        );
        let claim = Claim {
            entity: "ACAN".into(),
            direction: Some(Direction::Up),
            effect_size: Some(2.10),
            pvalue: Some(0.001),
            source_table: None,
            excerpt: "row".into(),
            contract: ClaimContract::NumericTableLookup,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let v = verify_claims_with_discovery(&[claim], tmp.path(), tmp.path(), &cfg);
        assert!(
            matches!(v[0].status, ClaimStatus::Verified),
            "{:?}",
            v[0].status
        );
    }

    #[test]
    fn cited_exact_name_wins_over_twin_tables() {
        // Two near-duplicate tables; a claim citing one by its exact file
        // name must resolve to it, NOT collapse to None (the laundering
        // case the recall floor closes — F4).
        let dir = tempfile::tempdir().unwrap();
        for name in ["de_results.tsv", "de_results_v2.tsv"] {
            std::fs::write(dir.path().join(name), "gene\tlog2FC\nTP53\t1.0\n").unwrap();
        }
        let idx = TableIndex::scan(dir.path());
        // Exact-name citation resolves deterministically (Step 1).
        let resolved = idx.resolve("de_results.tsv");
        assert!(
            resolved.is_some(),
            "exact cited name must resolve, not collapse"
        );
        assert!(resolved.unwrap().ends_with("de_results.tsv"));
    }

    #[test]
    fn cited_stem_wins_over_twin_tables() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "differential_expression.tsv",
            "differential_expression_alt.tsv",
        ] {
            std::fs::write(dir.path().join(name), "gene\tlog2FC\nTP53\t1.0\n").unwrap();
        }
        let idx = TableIndex::scan(dir.path());
        // Citing the exact stem must hit Step 2, not the fuzzy collapse.
        let resolved = idx.resolve("differential_expression");
        assert!(
            resolved.is_some(),
            "exact stem citation must resolve even when a sibling shares the prefix"
        );
        assert!(resolved.unwrap().ends_with("differential_expression.tsv"));
    }

    #[test]
    fn cited_path_basename_wins_over_twin_tables() {
        // A claim citing a full relative path whose basename names one of
        // two fuzzy-token-sharing twins must resolve to that exact file,
        // never the ≥2-candidate None-collapse (F4 laundering).
        let dir = tempfile::tempdir().unwrap();
        for name in ["de_results.tsv", "de_results_v2.tsv"] {
            std::fs::write(dir.path().join(name), "gene\tlog2FC\nTP53\t1.0\n").unwrap();
        }
        let idx = TableIndex::scan(dir.path());
        let resolved = idx.resolve("results/tables/de_results.tsv");
        assert!(
            resolved.is_some(),
            "cited path basename must resolve, not collapse to None"
        );
        assert!(resolved.unwrap().ends_with("de_results.tsv"));
    }

    #[test]
    fn cited_single_digit_table_not_ambiguous_with_double_digit_sibling() {
        // A cite of "Table 5" must resolve to `de_table_5.tsv` even when a
        // `scrna_table_15.tsv` sibling exists. The fuzzy token step peels
        // "table" off the cite, leaving the numeric token "5"; that token must
        // match the "5" segment of `de_table_5` but NOT the "5" inside the
        // "15" run of `scrna_table_15`. Before the digit-boundary guard, "5"
        // matched both, so resolution collapsed to None (≥2 candidates) and the
        // numeric gate scanned every table — reading the wrong entity row.
        let dir = tempfile::tempdir().unwrap();
        for name in ["de_table_5.tsv", "scrna_table_15.tsv"] {
            std::fs::write(dir.path().join(name), "gene\tlog2FC\nTP53\t1.0\n").unwrap();
        }
        let idx = TableIndex::scan(dir.path());
        let resolved = idx.resolve("Table 5");
        assert!(
            resolved.is_some(),
            "cited 'Table 5' must resolve, not collapse to None due to a double-digit sibling"
        );
        assert!(resolved.unwrap().ends_with("de_table_5.tsv"));
        // The double-digit sibling must still resolve on its own cite.
        let r15 = idx.resolve("Table 15");
        assert!(r15.is_some_and(|p| p.ends_with("scrna_table_15.tsv")));
    }

    #[test]
    fn direction_on_nonsig_near_zero_is_unverifiable() {
        // Option 2: a bare direction claim on a non-significant, near-zero
        // effect (|log2FC| < 0.5, adjusted p >= 0.05) is Unverifiable — the
        // direction is not mechanically determinable.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t0.3\t0.70\n",
        );
        let claims = extract_claims("ACAN was upregulated (Table S1).", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Unverifiable { .. }),
            "non-sig near-zero direction must be Unverifiable, got {:?}",
            v.status
        );
    }

    #[test]
    fn wrong_direction_on_nonsig_near_zero_is_not_a_mismatch() {
        // Even a contradicting direction on a non-significant near-zero effect
        // is Unverifiable, not Mismatch — a direction that isn't statistically
        // established can be neither confirmed nor refuted.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t0.3\t0.70\n",
        );
        let claims = extract_claims("ACAN was downregulated (Table S1).", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Unverifiable { .. }),
            "contradicting direction on non-sig near-zero must be Unverifiable, got {:?}",
            v.status
        );
    }

    #[test]
    fn significant_near_zero_direction_is_still_checked() {
        // The gate requires BOTH near-zero AND non-significant: a near-zero but
        // significant effect keeps a determinable direction.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_s1.tsv",
            "gene\tlog2FC\tpadj\nACAN\t0.3\t0.001\n",
        );
        let claims = extract_claims("ACAN was upregulated (Table S1).", &cfg);
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Verified),
            "significant near-zero up-claim should verify, got {:?}",
            v.status
        );
    }

    /// A concordance/reporting table row renders a tiny-but-significant adjusted
    /// p as "0.000" (fixed-decimal display rounding), which the extractor parses
    /// to `pvalue = 0.0`. Because the row carries no `padj`/`log2FC` keyword, the
    /// class-agnostic numeric gate is inert and the verdict comes entirely from
    /// `verify_one`'s p-value comparison. A claimed 0.0 against an observed tiny
    /// positive padj (7.04e-05) must VERIFY — it is display rounding of a truly
    /// significant value, not a fabrication.
    #[test]
    fn display_rounded_zero_padj_against_tiny_observed_verifies() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // Real DE-style row: raw pvalue + adjusted padj, both far below the
        // reporting floor; log2FC matches the claimed effect size.
        write_table(
            tmp.path(),
            "de_results.tsv",
            "gene\tlog2FC\tpvalue\tpadj\nNFKBIA\t0.7352\t1.0e-06\t7.04e-05\n",
        );
        let index = TableIndex::scan(tmp.path());
        let mut cache = std::collections::BTreeMap::new();
        let claim = Claim {
            entity: "NFKBIA".into(),
            direction: Some(Direction::Up),
            effect_size: Some(0.735),
            // "0.000" cell → display-rounded zero.
            pvalue: Some(0.0),
            source_table: Some("de_results.tsv".into()),
            // Markdown row shape: no `padj`/`log2FC` keyword, so the numeric gate
            // scans zero tokens and the base verifier alone decides.
            excerpt: "| NFKBIA | +0.735 | 0.000 | PMID 27371733 |".into(),
            contract: ClaimContract::NumericTableLookup,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let (status, _audit) = verify_for_contract_audited(&claim, &index, &cfg, &mut cache);
        assert!(
            matches!(status, ClaimStatus::Verified),
            "display-rounded padj 0.000 with tiny observed padj must verify, got {:?}",
            status
        );
    }

    /// The control for the display-rounding leniency: a claimed p of 0.0 against
    /// an observed padj that is clearly NON-significant (0.3) is not display
    /// rounding — the entity simply is not significant — so it stays a Mismatch,
    /// and that Mismatch must carry a non-empty rationale (no empty-audit
    /// mismatches; the gate's inert early-return still records the base detail).
    #[test]
    fn display_rounded_zero_padj_against_nonsignificant_observed_still_mismatches() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "de_results.tsv",
            "gene\tlog2FC\tpadj\nNFKBIA\t0.7352\t0.3\n",
        );
        let index = TableIndex::scan(tmp.path());
        let mut cache = std::collections::BTreeMap::new();
        let claim = Claim {
            entity: "NFKBIA".into(),
            direction: Some(Direction::Up),
            effect_size: Some(0.735),
            pvalue: Some(0.0),
            source_table: Some("de_results.tsv".into()),
            excerpt: "| NFKBIA | +0.735 | 0.000 | PMID 27371733 |".into(),
            contract: ClaimContract::NumericTableLookup,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let (status, audit) = verify_for_contract_audited(&claim, &index, &cfg, &mut cache);
        assert!(
            matches!(status, ClaimStatus::Mismatch { .. }),
            "claimed 0.0 vs non-significant observed padj 0.3 must Mismatch, got {:?}",
            status
        );
        let rationale = audit.and_then(|a| a.rationale);
        assert!(
            rationale.as_deref().is_some_and(|r| !r.is_empty()),
            "a genuine Mismatch must carry a non-empty rationale, got {:?}",
            rationale
        );
    }

    #[test]
    fn pvalue_tolerance_helpers_treat_claimed_zero_as_display_rounding() {
        // Unit-level lock on both p-value comparators: a claimed 0.0 agrees with
        // an observed value below the reporting floor and disagrees otherwise.
        assert!(pvalue_within_tolerance(0.0, 7.04e-05, 0.1));
        assert!(!pvalue_within_tolerance(0.0, 0.3, 0.1));
        assert!(pvalue_orders_within(0.0, 7.04e-05, PVALUE_ORDERS_TOLERANCE));
        assert!(!pvalue_orders_within(0.0, 0.3, PVALUE_ORDERS_TOLERANCE));
        // A non-p comparison is untouched: exact-equality zero still agrees, and
        // a claimed 0.0 never matches an observed that is not below the floor.
        assert!(pvalue_within_tolerance(0.0, 0.0, 0.1));
        assert!(!pvalue_within_tolerance(
            0.0,
            PVALUE_ZERO_DISPLAY_FLOOR * 10.0,
            0.1
        ));
    }

    #[test]
    fn stated_effect_size_contrastive_direction_word_on_nonsig_near_zero_is_not_a_mismatch() {
        // Real himes FP (IL11): a NON-REPLICATION sentence STATES the observed
        // effect size (+0.098, agreeing with the table) alongside a direction
        // WORD ("repression") that refers to the PRIOR study, on a gene that is
        // near-zero (|log2FC| < 0.5) AND non-significant (padj 0.82). The stated
        // number agrees, so this must NOT be a Mismatch — on a non-significant
        // near-zero effect the sign is statistical noise and the direction word
        // is not mechanically refutable. The pre-existing near-zero guard only
        // covered BARE direction claims (no stated effect size); an
        // effect-size-stating claim fell through to the direction hard-mismatch.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_pkg_table(
            tmp.path(),
            "de",
            "de_results.tsv",
            "gene\tlog2FC\tpadj\nACAN\t0.098\t0.82\n",
        );
        let claim = Claim {
            entity: "ACAN".into(),
            direction: Some(Direction::Down),
            effect_size: Some(0.098),
            pvalue: Some(0.82),
            source_table: None,
            excerpt: "ACAN was repressed in the prior study; here log2FC = +0.098 \
                      and padj = 0.82 — not significant and in the opposite \
                      direction from repression"
                .into(),
            contract: ClaimContract::NumericTableLookup,
            literature_evidence: None,
            matched_pvalue_keyword: Some("padj".into()),
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let v = verify_claims_with_discovery(&[claim], tmp.path(), tmp.path(), &cfg);
        assert!(
            !matches!(v[0].status, ClaimStatus::Mismatch { .. }),
            "a stated near-zero effect that agrees with the table must not Mismatch \
             on a contrastive direction word; got {:?}",
            v[0].status
        );
        assert!(
            matches!(v[0].status, ClaimStatus::Verified),
            "the stated log2FC (+0.098) and padj (0.82) both agree with the table → \
             Verified; got {:?}",
            v[0].status
        );
    }

    #[test]
    fn multi_entity_sentence_does_not_cross_flag_other_entitys_value() {
        // Real himes FP: one sentence names TWO genes each with its own log2FC
        // ("… GENEA (log2FC = +10.9963) and … GENEB (log2FC = -4.8136)"). The
        // extractor binds each gene's OWN value, but run_numeric_gate scanned
        // EVERY excerpt token and compared the OTHER gene's value against the
        // resolved row → two false mismatches. Both must Verify.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_pkg_table(
            tmp.path(),
            "de",
            "de_results.tsv",
            "gene\tlog2FC\tpadj\nGENEA\t10.9963\t1.1e-17\nGENEB\t-4.8136\t2.0e-4\n",
        );
        let claims = extract_claims(
            "Representative changes included GENEA (log2FC = +10.9963) and GENEB (log2FC = -4.8136).",
            &cfg,
        );
        // Guard the code path: these must be plain table-lookups (the numeric
        // gate runs), not ExtremeValue/RankTopN claims routed elsewhere.
        for entity in ["GENEA", "GENEB"] {
            let c = claims.iter().find(|c| c.entity == entity).unwrap();
            assert_eq!(
                c.contract,
                crate::claim_contract::ClaimContract::NumericTableLookup,
                "{entity} should be a plain table-lookup so the numeric gate runs"
            );
        }
        let v = verify_claims_with_discovery(&claims, tmp.path(), tmp.path(), &cfg);
        for entity in ["GENEA", "GENEB"] {
            let vd = v
                .iter()
                .find(|x| x.claim.entity == entity)
                .unwrap_or_else(|| panic!("no verdict for {entity}"));
            assert!(
                !matches!(vd.status, ClaimStatus::Mismatch { .. }),
                "{entity} must not Mismatch on the OTHER entity's token; got {:?}",
                vd.status
            );
        }
    }

    #[test]
    fn vf16_grouping_count_resolves_donor_cell_lines() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "cohort_manifest.tsv",
            "sample_id\tdonor_cell_line\ttreatment\n\
             N1_untrt\tN1\tuntrt\nN1_trt\tN1\ttrt\n\
             N61_untrt\tN61\tuntrt\nN61_trt\tN61\ttrt\n\
             N3_untrt\tN3\tuntrt\nN3_trt\tN3\ttrt\n\
             N4_untrt\tN4\tuntrt\nN4_trt\tN4\ttrt\n",
        );
        let verdicts = verify_narrative_counts(
            "The experiment used 4 donor cell lines in a paired design.",
            tmp.path(),
            tmp.path(),
            &cfg,
        );
        assert_eq!(verdicts.len(), 1, "{verdicts:?}");
        assert!(
            matches!(verdicts[0].status, ClaimStatus::Verified),
            "{verdicts:?}"
        );
        assert_eq!(
            verdicts[0].claim.source_table.as_deref(),
            Some("cohort_manifest.tsv")
        );
    }

    #[test]
    fn vf16_semantic_stage_summaries_link_operational_counts() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        let write_summary = |task: &str, value: serde_json::Value| {
            let dir = tmp.path().join("runtime").join("outputs").join(task);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("result.json"),
                serde_json::to_vec_pretty(&value).unwrap(),
            )
            .unwrap();
        };
        write_summary(
            "data_acquisition",
            serde_json::json!({"n_genes": 63677, "n_samples": 8, "n_donors": 4}),
        );
        write_summary(
            "differential_expression",
            serde_json::json!({
                "n_genes_input": 63677,
                "n_genes_prefilter_removed": 41308,
                "n_genes_tested": 22369,
                "n_significant": 4030,
                "top_effect_abundance_ratio_basis": {
                    "top_k_missing_significance_count": 0
                }
            }),
        );
        write_summary(
            "pathway_enrichment",
            serde_json::json!({"n_genes_ranked": 17190}),
        );
        write_summary(
            "contextualize_findings_with_literature",
            serde_json::json!({
                "n_entities_assessed": 6,
                "n_entities_not_assessed": 4024
            }),
        );

        let narrative = "\
The input matrix contained 63,677 genes x 8 samples.
The rowSums >=10 pre-filter removed 41,308 of 63,677 input genes, retaining 22,369 for testing.
The 17,190 ranked genes were mapped from 22,369 tested genes.
6 entities were searched.
4,024 of 4,030 significant DE genes were not searched.
0 of those 15 top features carry no usable padj.
The largest positive effect among all 4,030 significant genes was reported separately.";
        let verdicts = verify_narrative_counts(narrative, tmp.path(), tmp.path(), &cfg);
        assert_eq!(verdicts.len(), 11, "{verdicts:#?}");
        assert!(
            verdicts
                .iter()
                .all(|verdict| matches!(verdict.status, ClaimStatus::Verified)),
            "{verdicts:#?}"
        );
        assert!(
            verdicts.iter().all(|verdict| verdict
                .claim
                .source_table
                .as_deref()
                .is_some_and(|source| source.starts_with("runtime/outputs/")
                    && source.ends_with("/result.json"))),
            "{verdicts:#?}"
        );
    }

    #[test]
    fn vf16_himes_multi_population_sentences_bind_each_named_summary_field() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        let write_summary = |task: &str, value: serde_json::Value| {
            let dir = tmp.path().join("runtime").join("outputs").join(task);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("result.json"),
                serde_json::to_vec_pretty(&value).unwrap(),
            )
            .unwrap();
        };
        write_summary(
            "qc_preprocessing",
            serde_json::json!({
                "n_genes_pre_filter": 63677,
                "n_genes_prefilter_removed": 41308,
                "n_genes_retained": 22369
            }),
        );
        write_summary(
            "differential_expression",
            serde_json::json!({
                "n_genes_in_matrix": 22369,
                "tested_feature_count": 22369,
                "n_genes_tested": 17165,
                "n_significant": 4030
            }),
        );
        write_summary(
            "pathway_enrichment",
            serde_json::json!({
                "n_genes_pre_mapping": 22369,
                "n_genes_mapped": 17209,
                "n_genes_unmapped": 5160,
                "n_duplicate_gene_labels_removed": 32,
                "n_genes_ranked": 17177,
                "n_pathways_tested": 6251
            }),
        );
        write_summary(
            "contextualize_findings_with_literature",
            serde_json::json!({"n_entities_not_assessed": 4030}),
        );

        let narrative = "\
A rowSums >= 10 pre-filter was applied; this removed 41,308 of 63,677 genes \
(the original input to the filter step), retaining 22,369 genes for testing.
Of 22,369 genes in the tested matrix, 17,209 were mapped from Ensembl gene IDs \
to gene symbols via org.Hs.eg.db (v3.22.0); 5,160 remained unmapped.
After removing 32 duplicate gene-symbol labels, 17,177 genes were ranked for fgsea.
Of 22,369 genes in the filtered count matrix, 17,165 had estimable adjusted \
p-values after independent filtering.
A total of 6,251 gene sets were tested across four collections.
The complete set of 4,030 significant entities is provided below.
All 4,030 significant entities are `not_assessed`.";
        let verdicts = verify_narrative_counts(narrative, tmp.path(), tmp.path(), &cfg);
        assert_eq!(verdicts.len(), 13, "{verdicts:#?}");
        assert!(
            verdicts
                .iter()
                .all(|verdict| matches!(verdict.status, ClaimStatus::Verified)),
            "{verdicts:#?}"
        );
    }

    #[test]
    fn vf16_himes_duplicate_removal_does_not_alias_ranked_population() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("runtime/outputs/pathway_enrichment");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "n_duplicate_gene_labels_removed": 32,
                "n_genes_ranked": 17177
            }))
            .unwrap(),
        )
        .unwrap();

        let verdicts = verify_narrative_counts(
            "After removing 17177 duplicate gene-symbol labels, 17,177 genes were ranked for fgsea.",
            tmp.path(),
            tmp.path(),
            &cfg,
        );
        assert_eq!(verdicts.len(), 2, "{verdicts:#?}");
        assert!(
            matches!(verdicts[0].status, ClaimStatus::Mismatch { .. }),
            "{verdicts:#?}"
        );
        assert!(
            matches!(verdicts[1].status, ClaimStatus::Verified),
            "{verdicts:#?}"
        );
    }

    #[test]
    fn vf16_semantic_stage_summary_detects_wrong_not_assessed_count() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        let dir = tmp
            .path()
            .join("runtime/outputs/contextualize_findings_with_literature");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "n_entities_not_assessed": 4024
            }))
            .unwrap(),
        )
        .unwrap();

        let verdicts = verify_narrative_counts(
            "4,023 of 4,030 significant DE genes were not searched.",
            tmp.path(),
            tmp.path(),
            &cfg,
        );
        assert_eq!(verdicts.len(), 1, "{verdicts:#?}");
        assert!(
            matches!(verdicts[0].status, ClaimStatus::Mismatch { .. }),
            "{verdicts:#?}"
        );
        assert_eq!(
            verdicts[0].claim.source_table.as_deref(),
            Some("runtime/outputs/contextualize_findings_with_literature/result.json")
        );
    }

    #[test]
    fn vf16_semantic_summary_abstains_on_missing_or_conflicting_fields() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();

        let missing = tempdir().unwrap();
        let missing_verdicts = verify_narrative_counts(
            "0 of those 15 top features carry no usable padj.",
            missing.path(),
            missing.path(),
            &cfg,
        );
        assert_eq!(missing_verdicts.len(), 1, "{missing_verdicts:#?}");
        assert!(
            matches!(missing_verdicts[0].status, ClaimStatus::Unverifiable { .. }),
            "{missing_verdicts:#?}"
        );

        let conflicting = tempdir().unwrap();
        let dir = conflicting
            .path()
            .join("runtime/outputs/contextualize_findings_with_literature");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "n_entities_not_assessed": 4024,
                "nested": {"n_entities_not_assessed": 4023}
            }))
            .unwrap(),
        )
        .unwrap();
        let conflicting_verdicts = verify_narrative_counts(
            "4,024 of 4,030 significant DE genes were not searched.",
            conflicting.path(),
            conflicting.path(),
            &cfg,
        );
        assert_eq!(conflicting_verdicts.len(), 1, "{conflicting_verdicts:#?}");
        assert!(
            matches!(
                conflicting_verdicts[0].status,
                ClaimStatus::Unverifiable { .. }
            ),
            "{conflicting_verdicts:#?}"
        );
    }

    #[test]
    fn thresholded_nonsignificance_claim_verifies_when_gene_is_nonsignificant() {
        // Real himes FP (CXCL1): "GENEA … was not significant (padj > 0.05)" on
        // a gene whose table padj is indeed >= 0.05 must VERIFY. The
        // bare-significance branch previously flipped polarity and flagged it as
        // failing significance (< 0.05).
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_pkg_table(
            tmp.path(),
            "de",
            "de_results.tsv",
            "gene\tlog2FC\tpadj\nGENEA\t0.403\t0.16\n",
        );
        let claims = extract_claims(
            "GENEA (log2FC = +0.403) was not significant in this study (padj > 0.05).",
            &cfg,
        );
        let vd = verify_claims_with_discovery(&claims, tmp.path(), tmp.path(), &cfg)
            .into_iter()
            .find(|x| x.claim.entity == "GENEA")
            .expect("verdict for GENEA");
        assert!(
            !matches!(vd.status, ClaimStatus::Mismatch { .. }),
            "a non-significance claim on a genuinely non-significant gene must not Mismatch; got {:?}",
            vd.status
        );
    }

    #[test]
    fn thresholded_nonsignificance_claim_mismatches_when_gene_is_actually_significant() {
        // Converse guard: a "not significant" narrative for a gene that IS
        // significant (padj < 0.05) is a REAL Mismatch — the narrative
        // contradicts the table. The negation handling must not blanket-verify.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_pkg_table(
            tmp.path(),
            "de",
            "de_results.tsv",
            "gene\tlog2FC\tpadj\nGENEA\t3.0\t0.001\n",
        );
        let claims = extract_claims(
            "GENEA (log2FC = +3.0) was not significant in this study (padj > 0.05).",
            &cfg,
        );
        let vd = verify_claims_with_discovery(&claims, tmp.path(), tmp.path(), &cfg)
            .into_iter()
            .find(|x| x.claim.entity == "GENEA")
            .expect("verdict for GENEA");
        assert!(
            matches!(vd.status, ClaimStatus::Mismatch { .. }),
            "a false non-significance claim on a significant gene must Mismatch; got {:?}",
            vd.status
        );
    }

    // ── Literature-grounded contract (WS-CV) ─────────────────────────────

    /// Dispatch routes `LiteratureGrounded` into `verify_literature_grounded`.
    /// With no matrix and an empty `TableIndex`, the package root cannot be
    /// resolved → Unverifiable. The assertion is that the arm is reachable and
    /// does not panic on an unmatched contract.
    #[test]
    fn literature_grounded_dispatch_is_reachable() {
        use crate::claim_contract::ClaimContract;
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let claim = Claim {
            entity: "TP53".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: None,
            excerpt: "TP53 is concordant with prior work (PMID 12345678)".into(),
            contract: ClaimContract::LiteratureGrounded,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        };
        let index = TableIndex::scan(std::path::Path::new("/nonexistent"));
        let mut cache: BTreeMap<PathBuf, CachedTable> = BTreeMap::new();
        let status = verify_for_contract(&claim, &index, &cfg, &mut cache);
        assert!(matches!(status, ClaimStatus::Unverifiable { .. }));
    }

    #[test]
    fn resolve_evidence_literature_finds_canonical_path() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        let dir = tmp
            .path()
            .join("runtime/outputs/contextualize_findings_with_literature");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("claims_evidence_matrix.csv"),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified\n\
             finding_42,TP53,12345678;23456789,same_direction,pmc_oa_full_text,true\n",
        )
        .unwrap();
        let resolved =
            resolve_evidence_literature(tmp.path(), "finding_42", &[12345678], &cfg).unwrap();
        assert!(resolved.ends_with("claims_evidence_matrix.csv"));
    }

    #[test]
    fn resolve_evidence_literature_none_when_absent() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        assert!(resolve_evidence_literature(tmp.path(), "finding_x", &[1], &cfg).is_none());
    }

    #[test]
    fn load_literature_rows_parses_pmids_and_flags() {
        let tmp = tempdir().unwrap();
        let p = tmp.path().join("claims_evidence_matrix.csv");
        std::fs::write(
            &p,
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified,searched\n\
             finding_42,TP53,12345678;23456789,same_direction,pmc_oa_full_text,true,true\n\
             finding_9,EGFR,,no_prior_finding,none,false,true\n",
        )
        .unwrap();
        let matrix = load_literature_rows(&p).unwrap();
        assert!(matrix.verified_present, "verified column is present here");
        assert!(matrix.source_present, "source_kind column is present here");
        let rows = &matrix.rows;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].finding_id, "finding_42");
        assert_eq!(rows[0].prior_pmids, vec![12345678, 23456789]);
        assert_eq!(rows[0].concordance_flag, "same_direction");
        assert_eq!(rows[0].source_kind, "pmc_oa_full_text");
        assert!(rows[0].verified);
        assert!(rows[1].prior_pmids.is_empty());
        assert!(!rows[1].verified);
        assert!(rows[1].searched);
    }

    fn write_lit_matrix(root: &Path, body: &str) {
        let dir = root.join("runtime/outputs/contextualize_findings_with_literature");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("claims_evidence_matrix.csv"), body).unwrap();
    }

    // ── CV-1 / CV-2: the class-agnostic numeric gate ────────────────────────
    //
    // A literature-grounded claim that also asserts a number must have that
    // number checked against the cited/discovered table before any `Verified`.

    /// Write a DE result table under the differential_expression output dir so
    /// the discovery path (and thus the numeric gate) can find the gene's row.
    fn write_de_output(root: &Path, body: &str) {
        let dir = root.join("runtime/outputs/differential_expression");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("de_results.tsv"), body).unwrap();
    }

    /// A hand-built LiteratureGrounded claim with a chosen excerpt (so the gate
    /// scans exactly the numeric assertion under test), binding by symbol.
    fn lit_numeric_claim(entity: &str, excerpt: &str) -> Claim {
        let mut c = lit_claim_symbol_only(entity, excerpt);
        c.entity = entity.into();
        c
    }

    /// CV-1 flagship: `GENE (log₂FC +2.61, padj ≈ 10⁻⁸⁰)` against a table whose
    /// padj is `7.5e-46` is a **Mismatch** — the log2FC is right but the padj is
    /// wrong by ~34 orders. The verdict tuple records claimed ≈ 1e-80 vs
    /// observed 7.5e-46.
    #[test]
    fn literature_claim_with_wrong_padj_is_mismatch() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // No literature matrix → base is Unverifiable; the numeric gate must
        // still promote to Mismatch on the wrong padj.
        write_de_output(tmp.path(), "gene\tlog2FC\tpadj\nCRISPLD2\t2.61\t7.5e-46\n");
        let claim = lit_numeric_claim(
            "CRISPLD2",
            "CRISPLD2 (log₂FC +2.61, padj ≈ 10⁻⁸⁰) is concordant with prior reports (PMID 12345678)",
        );
        let verdicts = verify_claims_with_discovery(&[claim], tmp.path(), tmp.path(), &cfg);
        let v = &verdicts[0];
        assert!(
            matches!(v.status, ClaimStatus::Mismatch { .. }),
            "wrong padj must be a Mismatch, got {:?}",
            v.status
        );
        let audit = v.audit.as_ref().expect("mismatch carries an audit tuple");
        let claimed = audit.claimed_value.expect("claimed value recorded");
        let observed = audit.observed_value.expect("observed value recorded");
        assert!(
            (claimed.log10() - (-80.0)).abs() < 0.5,
            "claimed padj should be ~1e-80, got {claimed:e}"
        );
        assert!(
            (observed / 7.5e-46 - 1.0).abs() < 1e-6,
            "observed padj should be 7.5e-46, got {observed:e}"
        );
        assert_eq!(audit.measurement_column.as_deref(), Some("padj"));
        assert_eq!(
            audit.comparison_operator.as_deref(),
            Some("order_of_magnitude")
        );
        // Both numeric tokens were compared (parse-coverage 1.0), so the fix
        // caught the wrong value rather than degrading to Unverifiable.
        assert!(
            (audit.parse_coverage - 1.0).abs() < 1e-9,
            "both tokens compared, coverage should be 1.0, got {}",
            audit.parse_coverage
        );
    }

    /// A literature claim whose asserted number matches the table stays Verified
    /// (the number is now checked, and it agrees).
    #[test]
    fn literature_claim_with_correct_values_is_verified() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind\n\
             ENSG_CRISPLD2,CRISPLD2,24926665,same_direction,pmc_oa_full_text\n",
        );
        write_de_output(tmp.path(), "gene\tlog2FC\tpadj\nCRISPLD2\t2.61\t7.5e-46\n");
        // No threshold keyword (`padj`) → routes LiteratureGrounded; carries a
        // correct log2FC that the gate confirms.
        let claim = lit_numeric_claim(
            "CRISPLD2",
            "CRISPLD2 (log₂FC +2.61) is concordant with prior reports for airway remodeling",
        );
        let verdicts = verify_claims_with_discovery(&[claim], tmp.path(), tmp.path(), &cfg);
        let v = &verdicts[0];
        assert!(
            matches!(v.status, ClaimStatus::Verified),
            "correct literature+numeric claim must verify, got {:?}",
            v.status
        );
        let audit = v
            .audit
            .as_ref()
            .expect("verified claim carries an audit tuple");
        assert_eq!(audit.class, VerdictClass::NumericTable);
        assert!((audit.parse_coverage - 1.0).abs() < 1e-9);
    }

    /// A literature claim asserting a number that cannot be parsed at all
    /// (overflow) is Unverifiable — not silently Verified, and not a Mismatch.
    #[test]
    fn literature_claim_with_unparseable_number_is_unverifiable() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind\n\
             ENSG_CRISPLD2,CRISPLD2,24926665,same_direction,pmc_oa_full_text\n",
        );
        write_de_output(tmp.path(), "gene\tlog2FC\tpadj\nCRISPLD2\t2.61\t7.5e-46\n");
        // `1e999` overflows to non-finite → unparseable numeric assertion.
        let claim = lit_numeric_claim(
            "CRISPLD2",
            "CRISPLD2 (log₂FC ≈ 1e999) is concordant with prior reports",
        );
        let verdicts = verify_claims_with_discovery(&[claim], tmp.path(), tmp.path(), &cfg);
        let v = &verdicts[0];
        assert!(
            matches!(v.status, ClaimStatus::Unverifiable { .. }),
            "unparseable numeric assertion must be Unverifiable, got {:?}",
            v.status
        );
        let audit = v.audit.as_ref().expect("audit tuple present");
        assert!(
            audit.parse_coverage < 1.0,
            "an unparseable token must lower parse-coverage below 1.0, got {}",
            audit.parse_coverage
        );
    }

    /// A literature claim with NO numeric assertion is unaffected by the gate:
    /// the literature-direction check still governs and it verifies (no
    /// regression to the pre-gate behavior).
    #[test]
    fn literature_claim_without_numbers_still_direction_checked() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind\n\
             ENSG_CRISPLD2,CRISPLD2,24926665,same_direction,pmc_oa_full_text\n",
        );
        write_de_output(tmp.path(), "gene\tlog2FC\tpadj\nCRISPLD2\t2.61\t7.5e-46\n");
        let claim = lit_numeric_claim(
            "CRISPLD2",
            "CRISPLD2 is concordant with prior reports for airway remodeling",
        );
        let verdicts = verify_claims_with_discovery(&[claim], tmp.path(), tmp.path(), &cfg);
        let v = &verdicts[0];
        assert!(
            matches!(v.status, ClaimStatus::Verified),
            "a number-free literature claim must still verify by direction, got {:?}",
            v.status
        );
        let audit = v.audit.as_ref().expect("audit tuple present");
        // No numeric tokens → class stays literature_quotation, coverage 1.0.
        assert_eq!(audit.class, VerdictClass::LiteratureQuotation);
        assert!((audit.parse_coverage - 1.0).abs() < 1e-9);
    }

    /// A verdict with a full numeric audit tuple round-trips losslessly through
    /// JSON, and every verdict maps to exactly one class (per-class counts sum
    /// to `n_checked`).
    #[test]
    fn verdict_audit_round_trips_and_class_counts_sum() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_de_output(tmp.path(), "gene\tlog2FC\tpadj\nCRISPLD2\t2.61\t7.5e-46\n");
        let claim = lit_numeric_claim(
            "CRISPLD2",
            "CRISPLD2 (log₂FC +2.61, padj ≈ 10⁻⁸⁰) is concordant with prior reports",
        );
        let verdicts = verify_claims_with_discovery(&[claim], tmp.path(), tmp.path(), &cfg);
        let v = &verdicts[0];
        let json = serde_json::to_string(v).expect("serialize verdict");
        let back: ClaimVerdict = serde_json::from_str(&json).expect("deserialize verdict");
        assert_eq!(v, &back, "verdict must round-trip with all audit fields");
        assert!(back.audit.is_some(), "audit tuple survives round-trip");
        assert_eq!(
            back.audit.as_ref().unwrap().verifier_version,
            CLAIM_VERIFIER_VERSION
        );

        // Every verdict maps to EXACTLY one class, so the four per-class
        // counts sum to n_checked (no verdict is uncounted or double-counted).
        let mut report = ClaimVerificationReport::empty();
        for vd in verdicts {
            report.push(vd);
        }
        let class_of = |v: &ClaimVerdict| {
            v.audit
                .as_ref()
                .map(|a| a.class)
                .unwrap_or_else(|| verdict_class_of(&v.claim))
        };
        let mut per_class = [0usize; 4];
        for v in &report.verdicts {
            let idx = match class_of(v) {
                VerdictClass::NumericTable => 0,
                VerdictClass::Directional => 1,
                VerdictClass::EntityPresence => 2,
                VerdictClass::LiteratureQuotation => 3,
            };
            per_class[idx] += 1;
        }
        assert_eq!(
            per_class.iter().sum::<usize>(),
            report.n_checked,
            "per-class counts must sum to n_checked"
        );
    }

    fn lit_claim(finding_id: &str, pmids: Vec<u64>) -> Claim {
        Claim {
            entity: "TP53".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: None,
            excerpt: "TP53 is concordant with prior reports".into(),
            contract: crate::claim_contract::ClaimContract::LiteratureGrounded,
            literature_evidence: Some(crate::claim_extractor::LiteratureEvidence {
                finding_id: finding_id.into(),
                cited_pmids: pmids,
            }),
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        }
    }

    #[test]
    fn literature_grounded_verified_when_matrix_covers_cited_pmids() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified\n\
             finding_42,TP53,12345678;23456789,same_direction,pmc_oa_full_text,true\n",
        );
        let status = verify_literature_grounded_at(
            &lit_claim("finding_42", vec![12345678]),
            tmp.path(),
            &cfg,
        );
        assert!(matches!(status, ClaimStatus::Verified), "{status:?}");
    }

    /// A `lit_claim` variant with NO in-sentence evidence (finding_id / PMID),
    /// only a gene entity — the shape a report produces when it lists concordant
    /// genes on one line and the PMID in a separate section header.
    fn lit_claim_symbol_only(entity: &str, excerpt: &str) -> Claim {
        Claim {
            entity: entity.into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: None,
            excerpt: excerpt.into(),
            contract: crate::claim_contract::ClaimContract::LiteratureGrounded,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        }
    }

    /// Fix A: a LiteratureGrounded claim whose entity is a gene present in
    /// `claims_evidence_matrix.csv` must adjudicate by SYMBOL even when the
    /// narrative carried no in-sentence finding_id/PMID (the report listed the
    /// concordant genes on one line and the PMID in a separate header).
    /// Previously this returned Unverifiable, collapsing the verified-claim count
    /// on report wording alone.
    #[test]
    fn literature_grounded_binds_by_symbol_without_in_sentence_pmid() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified\n\
             finding_1,SPARCL1,24926665,same_direction,pmc_oa_full_text,true\n",
        );
        let claim = lit_claim_symbol_only(
            "SPARCL1",
            "Concordant genes (first 10): SPARCL1, DUSP1, PER1, KLF15",
        );
        let status = verify_literature_grounded_at(&claim, tmp.path(), &cfg);
        assert!(
            matches!(status, ClaimStatus::Verified),
            "symbol-bound same_direction claim must verify without an in-sentence PMID; got {status:?}"
        );
    }

    #[test]
    fn literature_no_prior_finding_verifies_from_retained_search_scope() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified,searched\n\
             ENSG00000157514,TSC22D3,,no_prior_finding,none,false,true\n",
        );
        let claim = lit_claim_symbol_only(
            "TSC22D3",
            "TSC22D3 was searched, but no prior finding was retrieved.",
        );
        let status = verify_literature_grounded_at(&claim, tmp.path(), &cfg);
        assert!(
            matches!(status, ClaimStatus::Verified),
            "searched no-result row is the machine-resolved support for a faithful absence claim; got {status:?}"
        );

        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified,searched\n\
             ENSG00000157514,TSC22D3,,no_prior_finding,none,false,false\n",
        );
        let status = verify_literature_grounded_at(&claim, tmp.path(), &cfg);
        assert!(
            matches!(status, ClaimStatus::Unverifiable { .. }),
            "an unsearched no-result row must not substantiate an absence claim; got {status:?}"
        );
    }

    /// Regression (real-extractor shape). The extractor attaches a
    /// LiteratureGrounded wrapper to EVERY such claim — `Some(LiteratureEvidence
    /// { finding_id: <entity>, cited_pmids: [] })` when the sentence carries no
    /// inline PMID (the concordant-gene list and the "### PMID …" header sit on
    /// separate lines). The symbol-fallback waiver must therefore key on an EMPTY
    /// citation set, NOT on the wrapper being `None` (which real output never is),
    /// so a single-foundational-paper concordance gene still verifies. Before this
    /// fix the claim matched the `Some(..)` arm → `symbol_fallback=false` → hit the
    /// `min_papers >= 2` bar (each airway gene carries exactly one Himes-2014 PMID)
    /// → Unverifiable, collapsing the whole airway package's verified count.
    #[test]
    fn literature_grounded_binds_by_symbol_when_wrapper_present_but_no_cited_pmid() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // No `verified` column and a SINGLE prior PMID — the real airway
        // claims_evidence_matrix.csv shape (grounding rests on the
        // `same_direction` flag; every gene carries exactly one Himes-2014 PMID).
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind\n\
             ENSG00000152583,SPARCL1,24926665,same_direction,pmc_oa_full_text\n",
        );
        // Faithful to production: wrapper present, finding_id = entity, EMPTY
        // cited_pmids (no inline PMID in the enumeration sentence).
        let mut claim = lit_claim("SPARCL1", vec![]);
        claim.entity = "SPARCL1".into();
        claim.excerpt = "Concordant genes (first 10): SPARCL1, DUSP1, SAMHD1, MAOA".into();
        let status = verify_literature_grounded_at(&claim, tmp.path(), &cfg);
        assert!(
            matches!(status, ClaimStatus::Verified),
            "wrapper-present/empty-cited single-PMID same_direction gene must verify by symbol; got {status:?}"
        );
    }

    /// End-to-end regression through the REAL extractor + discovery path (the
    /// production route the hand-built fixtures bypassed by forcing
    /// `literature_evidence: None`). A bare concordance enumeration whose PMID
    /// lives in a separate header must verify each listed gene against the
    /// single-PMID matrix. This is the exact shape that collapsed the airway
    /// package's verified count from 147 to 5.
    #[test]
    fn extracted_concordance_enumeration_verifies_against_single_pmid_matrix() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind\n\
             ENSG00000152583,SPARCL1,24926665,same_direction,pmc_oa_full_text\n",
        );
        let claims = extract_claims(
            "Concordant genes (first 10): SPARCL1, DUSP1, PER1, KLF15",
            &cfg,
        );
        let sparcl1 = claims
            .iter()
            .find(|c| c.entity == "SPARCL1")
            .cloned()
            .expect("extracted SPARCL1 claim");
        assert_eq!(
            sparcl1.contract,
            crate::claim_contract::ClaimContract::LiteratureGrounded
        );
        // Faithful to production: the wrapper is present with an EMPTY citation set.
        let ev = sparcl1
            .literature_evidence
            .as_ref()
            .expect("extractor attaches a LiteratureGrounded wrapper");
        assert!(
            ev.cited_pmids.is_empty(),
            "enumeration sentence carried no inline PMID"
        );
        let verdicts = verify_claims_with_discovery(&[sparcl1], tmp.path(), tmp.path(), &cfg);
        assert!(
            matches!(verdicts[0].status, ClaimStatus::Verified),
            "concordance-enumeration gene must verify by symbol; got {:?}",
            verdicts[0].status
        );
    }

    /// A symbol-bound literature claim that VERIFIES must record the matrix as
    /// its supporting evidence, so the projected `supported_by` points at
    /// claims_evidence_matrix.csv. Before this, the discovery path pushed the
    /// original claim (`source_table: None`) for LiteratureGrounded verdicts, so
    /// a Verified literature claim carried an EMPTY supported_by — degrading
    /// audit-proof evidence_coverage and diverging from the provenance the
    /// baseline recorded for every verified gene.
    #[test]
    fn literature_verified_verdict_records_matrix_as_supported_evidence() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind\n\
             ENSG00000152583,SPARCL1,24926665,same_direction,pmc_oa_full_text\n",
        );
        let claims = extract_claims(
            "Concordant genes (first 10): SPARCL1, DUSP1, PER1, KLF15",
            &cfg,
        );
        let sparcl1 = claims
            .iter()
            .find(|c| c.entity == "SPARCL1")
            .cloned()
            .expect("extracted SPARCL1 claim");
        let verdicts = verify_claims_with_discovery(&[sparcl1], tmp.path(), tmp.path(), &cfg);
        assert!(
            matches!(verdicts[0].status, ClaimStatus::Verified),
            "{:?}",
            verdicts[0].status
        );
        let src = verdicts[0].claim.source_table.as_deref().unwrap_or("");
        assert!(
            src.contains("claims_evidence_matrix.csv"),
            "verified literature claim must record the matrix as source_table (→ supported_by); got {src:?}"
        );
    }

    /// The symbol fallback must NOT ground a gene ABSENT from the matrix — a
    /// literature claim about an un-adjudicated gene stays Unverifiable (no
    /// fabricated grounding, no over-binding).
    #[test]
    fn literature_grounded_symbol_fallback_unverifiable_when_gene_absent() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified\n\
             finding_1,SPARCL1,24926665,same_direction,pmc_oa_full_text,true\n",
        );
        let claim = lit_claim_symbol_only("NOTAGENE", "NOTAGENE is concordant with prior work");
        let status = verify_literature_grounded_at(&claim, tmp.path(), &cfg);
        assert!(
            matches!(status, ClaimStatus::Unverifiable { .. }),
            "{status:?}"
        );
    }

    #[test]
    fn literature_grounded_mismatch_on_uncited_pmid() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified\n\
             finding_42,TP53,12345678;23456789,same_direction,pmc_oa_full_text,true\n",
        );
        // Narrative cites a PMID the matrix does not contain → fabricated cite.
        let status = verify_literature_grounded_at(
            &lit_claim("finding_42", vec![99999999]),
            tmp.path(),
            &cfg,
        );
        assert!(matches!(status, ClaimStatus::Mismatch { .. }), "{status:?}");
    }

    #[test]
    fn literature_grounded_mismatch_on_opposite_direction_prior() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified\n\
             finding_42,TP53,12345678;23456789,opposite_direction,pmc_oa_full_text,true\n",
        );
        let status = verify_literature_grounded_at(
            &lit_claim("finding_42", vec![12345678]),
            tmp.path(),
            &cfg,
        );
        assert!(matches!(status, ClaimStatus::Mismatch { .. }), "{status:?}");
    }

    #[test]
    fn vf15a_no_prior_finding_asserted_concordant_is_mismatch() {
        // VF-15a — the matrix records `no_prior_finding`, but the narrative
        // POSITIVELY asserts concordance ("...is concordant with prior
        // reports") → fabricated concordance → Mismatch.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified\n\
             finding_42,TP53,,no_prior_finding,pmc_oa_full_text,true\n",
        );
        // lit_claim's excerpt = "TP53 is concordant with prior reports".
        let status =
            verify_literature_grounded_at(&lit_claim("finding_42", vec![]), tmp.path(), &cfg);
        assert!(
            matches!(status, ClaimStatus::Mismatch { .. }),
            "asserted-concordance vs no_prior_finding must be a Mismatch, got {status:?}"
        );

        // Faithful twin: a NEUTRAL excerpt (no agreement cue) over the same
        // no_prior_finding row must NOT be a fabricated-concordance Mismatch.
        let mut neutral = lit_claim("finding_42", vec![]);
        neutral.excerpt = "TP53 was differentially expressed in this cohort".into();
        let status2 = verify_literature_grounded_at(&neutral, tmp.path(), &cfg);
        assert!(
            !matches!(status2, ClaimStatus::Mismatch { .. }),
            "neutral no_prior_finding mention must not be flagged a fabricated concordance, got {status2:?}"
        );
    }

    #[test]
    fn literature_grounded_unverifiable_below_min_papers() {
        let mut p = policy_json();
        p["verifiableEntities"]["literatureGrounding"] = json!({"minPapers": 2, "minSources": 1});
        let cfg = ExtractorConfig::from_policy(&p).unwrap();
        let tmp = tempdir().unwrap();
        // Only ONE supporting PMID — below minPapers=2.
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified\n\
             finding_42,TP53,12345678,same_direction,pmc_oa_full_text,true\n",
        );
        let status = verify_literature_grounded_at(
            &lit_claim("finding_42", vec![12345678]),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(status, ClaimStatus::Unverifiable { .. }),
            "{status:?}"
        );
    }

    #[test]
    fn literature_grounded_binds_by_symbol_when_evidence_block_cleared() {
        // Fix A: clearing the in-sentence evidence block no longer strands a
        // literature claim — the entity (TP53) is present in the matrix, so it
        // binds by SYMBOL and adjudicates from the row → Verified. (Before Fix A
        // this returned Unverifiable "carries no finding_id / cited PMIDs". The
        // absent-gene boundary is covered by
        // `literature_grounded_symbol_fallback_unverifiable_when_gene_absent`.)
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified\n\
             finding_42,TP53,12345678;23456789,same_direction,pmc_oa_full_text,true\n",
        );
        let mut claim = lit_claim("finding_42", vec![12345678]);
        claim.literature_evidence = None;
        let status = verify_literature_grounded_at(&claim, tmp.path(), &cfg);
        assert!(matches!(status, ClaimStatus::Verified), "{status:?}");
    }

    #[test]
    fn discovery_routes_literature_grounded_to_matrix() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified\n\
             TP53,TP53,12345678;23456789,same_direction,pmc_oa_full_text,true\n",
        );
        // No result table at all — a table-discovery path would return
        // Unverifiable("not found in any result table"); the literature path
        // must Verify against the matrix instead.
        let claims = extract_claims(
            "TP53 dysregulation is concordant with prior reports (PMID 12345678, PMID 23456789).",
            &cfg,
        );
        let tp53 = claims
            .iter()
            .find(|c| c.entity == "TP53")
            .cloned()
            .expect("extracted TP53 claim");
        assert_eq!(
            tp53.contract,
            crate::claim_contract::ClaimContract::LiteratureGrounded
        );
        let verdicts = verify_claims_with_discovery(&[tp53], tmp.path(), tmp.path(), &cfg);
        assert!(
            matches!(verdicts[0].status, ClaimStatus::Verified),
            "{:?}",
            verdicts[0].status
        );
    }

    // ── §3.6 narrative↔domain cross-check ────────────────────────────────

    #[test]
    fn aggregate_n_within_domain_range_verifies() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_pkg_table(
            tmp.path(),
            "de",
            "de.tsv",
            "gene\tlog2FC\tpadj\nA\t2.0\t0.001\nB\t-1.0\t0.02\nC\t1.0\t0.049\nD\t0.1\t0.5\n",
        );
        let table = resolve_evidence_table(tmp.path(), "de.tsv").unwrap();
        // Narrative says 3 DEGs (padj<0.05); domain plausible 1..=1000.
        let status = verify_aggregate_n_in_range(
            "3 genes are differentially expressed (padj < 0.05)",
            &table,
            &cfg,
            1.0,
            1000.0,
        );
        assert!(matches!(status, ClaimStatus::Verified), "{status:?}");
    }

    #[test]
    fn aggregate_n_outside_domain_range_mismatches() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_pkg_table(
            tmp.path(),
            "de",
            "de.tsv",
            "gene\tlog2FC\tpadj\nA\t2.0\t0.001\nB\t-1.0\t0.02\nC\t1.0\t0.049\nD\t0.1\t0.5\n",
        );
        let table = resolve_evidence_table(tmp.path(), "de.tsv").unwrap();
        // The TABLE recompute (3) agrees with the narrative, but a domain
        // floor of 5000 makes 3 implausible for the declared cohort.
        let status = verify_aggregate_n_in_range(
            "3 genes are differentially expressed (padj < 0.05)",
            &table,
            &cfg,
            5000.0,
            20000.0,
        );
        assert!(matches!(status, ClaimStatus::Mismatch { .. }), "{status:?}");
    }

    #[test]
    fn aggregate_n_unverifiable_when_not_count_shaped() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_pkg_table(
            tmp.path(),
            "de",
            "de.tsv",
            "gene\tlog2FC\tpadj\nA\t2.0\t0.001\n",
        );
        let table = resolve_evidence_table(tmp.path(), "de.tsv").unwrap();
        let status =
            verify_aggregate_n_in_range("the analysis was performed", &table, &cfg, 1.0, 100.0);
        assert!(
            matches!(status, ClaimStatus::Unverifiable { .. }),
            "{status:?}"
        );
    }

    #[test]
    fn narrative_must_state_threshold() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        write_pkg_table(
            tmp.path(),
            "de",
            "de.tsv",
            "gene\tlog2FC\tpadj\nA\t2.0\t0.001\n",
        );
        let table = resolve_evidence_table(tmp.path(), "de.tsv").unwrap();
        // No "padj < X" stated → Unverifiable (the SME never declared the cut).
        let status =
            verify_narrative_threshold_honored("we report the significant genes", &table, &cfg);
        assert!(
            matches!(status, ClaimStatus::Unverifiable { .. }),
            "{status:?}"
        );
    }

    #[test]
    fn artifact_honors_stated_threshold_verifies() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // Every row satisfies padj < 0.05 (the "filtered DE table" contract).
        write_pkg_table(
            tmp.path(),
            "de",
            "de.tsv",
            "gene\tlog2FC\tpadj\nA\t2.0\t0.001\nB\t-1.0\t0.02\nC\t1.0\t0.049\n",
        );
        let table = resolve_evidence_table(tmp.path(), "de.tsv").unwrap();
        let status = verify_narrative_threshold_honored(
            "the filtered DE table reports genes at padj < 0.05",
            &table,
            &cfg,
        );
        assert!(matches!(status, ClaimStatus::Verified), "{status:?}");
    }

    #[test]
    fn artifact_violates_stated_threshold_mismatches() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tmp = tempdir().unwrap();
        // Row D leaks past the stated cut (padj 0.5 >= 0.05).
        write_pkg_table(
            tmp.path(),
            "de",
            "de.tsv",
            "gene\tlog2FC\tpadj\nA\t2.0\t0.001\nB\t-1.0\t0.02\nD\t0.1\t0.5\n",
        );
        let table = resolve_evidence_table(tmp.path(), "de.tsv").unwrap();
        let status = verify_narrative_threshold_honored(
            "the filtered DE table reports genes at padj < 0.05",
            &table,
            &cfg,
        );
        assert!(matches!(status, ClaimStatus::Mismatch { .. }), "{status:?}");
    }

    // ── Emitted contextualize-header literature grounding ────────────────────
    //
    // The contextualize atom writes `claims_evidence_matrix.csv` with header
    //   finding_id,entity,entity_kind,pmid,evidence_quote,concordance_flag
    // — note the singular `pmid` column and the ABSENCE of `verified` /
    // `source_kind`. Before the fix, the PMID column resolved only from
    // `prior_pmids`/`prior_pmid`, so every row's PMID set parsed empty, the
    // supporting set was empty, and a narrative that CORRECTLY cited a prior
    // PMID was falsely flagged "cites PMID X but no supporting row" (Mismatch)
    // for 24 of 25 mismatches in the executed package (KLF15/CRISPLD2/…).

    /// A literature claim with a caller-chosen entity / finding_id / excerpt,
    /// so a single helper drives the symbol- and Ensembl-keyed cases.
    fn lit_claim_for(entity: &str, finding_id: &str, pmids: Vec<u64>, excerpt: &str) -> Claim {
        Claim {
            entity: entity.into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: None,
            excerpt: excerpt.into(),
            contract: crate::claim_contract::ClaimContract::LiteratureGrounded,
            literature_evidence: Some(crate::claim_extractor::LiteratureEvidence {
                finding_id: finding_id.into(),
                cited_pmids: pmids,
            }),
            matched_pvalue_keyword: None,
            linear_fold: None,
            aggregate_kind: None,
            aggregate_column: None,
            aggregate_rowset: None,
            aggregate_value: None,
            collection: None,
            term: None,
            keyed_column: None,
            keyed_value: None,
        }
    }

    /// minPapers=1 so a single-PMID emitted row can reach Verified; this
    /// isolates the column-resolution / verification-record logic from the
    /// (separately tested) paper-count threshold.
    fn lit_policy_min1() -> serde_json::Value {
        let mut p = policy_json();
        p["verifiableEntities"]["literatureGrounding"] = json!({"minPapers": 1, "minSources": 1});
        p
    }

    /// (A) The real emitted header, symbol-entity claim citing the row's PMID →
    /// Verified (was a false Mismatch). Exercised for both KLF15 and CRISPLD2.
    #[test]
    fn emitted_header_supported_citation_verifies() {
        let cfg = ExtractorConfig::from_policy(&lit_policy_min1()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,entity_kind,pmid,evidence_quote,concordance_flag\n\
             ENSG00000163884,KLF15,gene,28375666,\"KLF15 induced by dex\",same_direction\n\
             ENSG00000103196,CRISPLD2,gene,24926665,\"CRISPLD2 glucocorticoid target\",same_direction\n",
        );

        let klf15 = verify_literature_grounded_at(
            &lit_claim_for(
                "KLF15",
                "ENSG00000163884",
                vec![28375666],
                "KLF15 is a known dex target",
            ),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(klf15, ClaimStatus::Verified),
            "emitted-header KLF15 citing its supporting PMID must Verify, got {klf15:?}"
        );

        let crispld2 = verify_literature_grounded_at(
            &lit_claim_for(
                "CRISPLD2",
                "ENSG00000103196",
                vec![24926665],
                "CRISPLD2 is a glucocorticoid-responsive gene",
            ),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(crispld2, ClaimStatus::Verified),
            "emitted-header CRISPLD2 citing its supporting PMID must Verify, got {crispld2:?}"
        );
    }

    /// (B) Same emitted header + KLF15 row, but the narrative cites a PMID the
    /// matrix does NOT carry → still a Mismatch (a genuine fabricated-cite of
    /// the SAME class must keep being caught).
    #[test]
    fn emitted_header_uncited_pmid_still_mismatches() {
        let cfg = ExtractorConfig::from_policy(&lit_policy_min1()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,entity_kind,pmid,evidence_quote,concordance_flag\n\
             ENSG00000163884,KLF15,gene,28375666,\"KLF15 induced by dex\",same_direction\n",
        );
        let status = verify_literature_grounded_at(
            &lit_claim_for(
                "KLF15",
                "ENSG00000163884",
                vec![99999999],
                "KLF15 is a known dex target",
            ),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(status, ClaimStatus::Mismatch { .. }),
            "emitted-header claim citing a PMID absent from the matrix must Mismatch, got {status:?}"
        );
    }

    /// (C) Emitted header carrying `opposite_direction` → still a Mismatch
    /// (the contradiction is keyed on the flag, not the PMID column).
    #[test]
    fn emitted_header_opposite_direction_still_mismatches() {
        let cfg = ExtractorConfig::from_policy(&lit_policy_min1()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,entity_kind,pmid,evidence_quote,concordance_flag\n\
             ENSG00000163884,KLF15,gene,28375666,\"prior work shows the opposite\",opposite_direction\n",
        );
        let status = verify_literature_grounded_at(
            &lit_claim_for(
                "KLF15",
                "ENSG00000163884",
                vec![28375666],
                "KLF15 is concordant with prior work",
            ),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(status, ClaimStatus::Mismatch { .. }),
            "emitted-header opposite_direction prior must Mismatch, got {status:?}"
        );
    }

    /// (C-twin) The real false positive from the executed Himes package: the
    /// reporting narrative FAITHFULLY describes an opposite-direction finding
    /// ("...showing small, non-significant fold change in a discordant direction
    /// in these data") and AGREES with the matrix's `opposite_direction` flag.
    /// A faithful discordance description carries no agreement cue → it is a
    /// genuine adjudication record → Verified, NOT a Mismatch.
    #[test]
    fn opposite_direction_faithful_discordance_description_verifies() {
        let cfg = ExtractorConfig::from_policy(&lit_policy_min1()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,entity_kind,pmid,evidence_quote,concordance_flag\n\
             RAMP1,RAMP1,gene,28375666,\"putative anti-inflammatory GR-occupancy target\",opposite_direction\n",
        );
        let status = verify_literature_grounded_at(
            &lit_claim_for(
                "RAMP1",
                "RAMP1",
                vec![28375666],
                "RAMP1 (PMID 28375666) - listed as a putative anti-inflammatory \
                 GR-occupancy target in that publication but showing small, \
                 non-significant fold change in a discordant direction in these data",
            ),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(status, ClaimStatus::Verified),
            "a faithful opposite-direction discordance description (no concordance \
             assertion) agrees with the matrix and must Verify, got {status:?}"
        );
    }

    /// (C-twin) A neutral citation that merely reports the prior finding, with
    /// no agreement cue, over an `opposite_direction` row must NOT be a Mismatch
    /// — it carries no fabricated concordance, so it is a genuine adjudication
    /// record → Verified.
    #[test]
    fn opposite_direction_neutral_mention_verifies() {
        let cfg = ExtractorConfig::from_policy(&lit_policy_min1()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,entity_kind,pmid,evidence_quote,concordance_flag\n\
             RAMP1,RAMP1,gene,28375666,\"GR-occupancy target\",opposite_direction\n",
        );
        let status = verify_literature_grounded_at(
            &lit_claim_for(
                "RAMP1",
                "RAMP1",
                vec![28375666],
                "RAMP1 was reported as a GR-occupancy target (PMID 28375666)",
            ),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(status, ClaimStatus::Verified),
            "a neutral opposite_direction citation with no agreement cue must Verify, got {status:?}"
        );
    }

    /// (C-twin) Explicit guard for the contradiction the gate must still catch:
    /// the narrative POSITIVELY asserts concordance ("...is concordant with...")
    /// while the matrix records `opposite_direction` → fabricated concordance →
    /// Mismatch. Mirrors `emitted_header_opposite_direction_still_mismatches`
    /// for clarity now that the branch is gated.
    #[test]
    fn opposite_direction_asserted_concordance_still_mismatches() {
        let cfg = ExtractorConfig::from_policy(&lit_policy_min1()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,entity_kind,pmid,evidence_quote,concordance_flag\n\
             RAMP1,RAMP1,gene,28375666,\"GR-occupancy target\",opposite_direction\n",
        );
        let status = verify_literature_grounded_at(
            &lit_claim_for(
                "RAMP1",
                "RAMP1",
                vec![28375666],
                "RAMP1 is concordant with the prior reports",
            ),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(status, ClaimStatus::Mismatch { .. }),
            "asserted concordance vs an opposite_direction row must Mismatch, got {status:?}"
        );
    }

    /// (D) Emitted header + `no_prior_finding` + an agreement cue in the
    /// excerpt → fabricated concordance → still a Mismatch (VF-15a survives the
    /// new column shape).
    #[test]
    fn emitted_header_fabricated_concordance_still_mismatches() {
        let cfg = ExtractorConfig::from_policy(&lit_policy_min1()).unwrap();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,entity_kind,pmid,evidence_quote,concordance_flag\n\
             ENSG00000163884,KLF15,gene,,\"novel finding\",no_prior_finding\n",
        );
        // VF-15a: positive concordance assertion against a no_prior_finding row.
        let status = verify_literature_grounded_at(
            &lit_claim_for(
                "KLF15",
                "ENSG00000163884",
                vec![],
                "KLF15 is consistent with prior reports",
            ),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(status, ClaimStatus::Mismatch { .. }),
            "emitted-header fabricated concordance must Mismatch, got {status:?}"
        );

        // Faithful twin: a NEUTRAL excerpt over the same no_prior_finding row
        // must NOT be flagged a fabricated concordance.
        let neutral = verify_literature_grounded_at(
            &lit_claim_for(
                "KLF15",
                "ENSG00000163884",
                vec![],
                "KLF15 was differentially expressed",
            ),
            tmp.path(),
            &cfg,
        );
        assert!(
            !matches!(neutral, ClaimStatus::Mismatch { .. }),
            "neutral no_prior_finding mention must not Mismatch, got {neutral:?}"
        );
    }

    /// (Fix 3) A symbol-keyed claim must match an Ensembl-keyed finding_id when
    /// the row's `entity` does NOT carry the symbol, using the independent
    /// annotation map — and the same machinery must NOT rescue a genuinely
    /// uncited PMID.
    #[test]
    fn emitted_header_symbol_resolves_ensembl_finding_via_map() {
        let mut map = BTreeMap::new();
        map.insert("klf15".to_string(), "ENSG00000163884".to_string());
        let mut cfg = ExtractorConfig::from_policy(&lit_policy_min1()).unwrap();
        cfg.gene_annotation_map = Some(map);

        let tmp = tempdir().unwrap();
        // The matrix row is keyed by the Ensembl id in BOTH finding_id and
        // entity columns (no symbol present), so only the map can bridge it.
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,entity_kind,pmid,evidence_quote,concordance_flag\n\
             ENSG00000163884,ENSG00000163884,gene,28375666,\"KLF15 induced by dex\",same_direction\n",
        );
        // Claim keyed by SYMBOL with an unrelated finding_id label.
        let verified = verify_literature_grounded_at(
            &lit_claim_for(
                "KLF15",
                "klf15_finding",
                vec![28375666],
                "KLF15 is a dex target",
            ),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(verified, ClaimStatus::Verified),
            "symbol claim must resolve the Ensembl-keyed row via the map, got {verified:?}"
        );

        // Same alias path, but an uncited PMID must STILL Mismatch.
        let mismatch = verify_literature_grounded_at(
            &lit_claim_for(
                "KLF15",
                "klf15_finding",
                vec![99999999],
                "KLF15 is a dex target",
            ),
            tmp.path(),
            &cfg,
        );
        assert!(
            matches!(mismatch, ClaimStatus::Mismatch { .. }),
            "alias-resolved row with an uncited PMID must still Mismatch, got {mismatch:?}"
        );
    }

    /// (E) Report-level self-consistency guard. When a (entity, PMID) pair is
    /// Verified, a contradicting "no supporting row" Mismatch on the SAME pair
    /// is demoted — but an INDEPENDENT genuinely-absent (gene, PMID) Mismatch is
    /// preserved.
    #[test]
    fn missing_row_mismatch_contradicted_by_verified_twin_is_demoted() {
        let verified = ClaimVerdict {
            claim: lit_claim_for(
                "KLF15",
                "ENSG00000163884",
                vec![28375666],
                "KLF15 prior work",
            ),
            status: ClaimStatus::Verified,
            strength: ClaimStrength::Exploratory,
            audit: None,
        };
        // Contradictory missing-row Mismatch on the SAME (KLF15, 28375666).
        let contradicted = ClaimVerdict {
            claim: lit_claim_for("KLF15", "ENSG00000163884", vec![28375666], "KLF15 again"),
            status: ClaimStatus::Mismatch {
                detail: "literature: narrative cites PMID 28375666 but the matrix has no such supporting row for `KLF15`".into(),
            },
            strength: ClaimStrength::Exploratory,
            audit: None,
        };
        // INDEPENDENT genuinely-absent Mismatch — different gene, no Verified
        // twin — must survive.
        let independent = ClaimVerdict {
            claim: lit_claim_for("GHOSTGENE", "ENSG00000000000", vec![55555555], "GHOSTGENE prior work"),
            status: ClaimStatus::Mismatch {
                detail: "literature: narrative cites PMID 55555555 but the matrix has no such supporting row for `GHOSTGENE`".into(),
            },
            strength: ClaimStrength::Exploratory,
            audit: None,
        };
        // An opposite_direction Mismatch on a verified pair must NOT be demoted
        // (it keys on a flag, not a missing row).
        let opposite = ClaimVerdict {
            claim: lit_claim_for("KLF15", "ENSG00000163884", vec![28375666], "KLF15 opposite"),
            status: ClaimStatus::Mismatch {
                detail: "literature: matrix records opposite-direction prior finding for `KLF15`"
                    .into(),
            },
            strength: ClaimStrength::Exploratory,
            audit: None,
        };

        let mut verdicts = vec![verified, contradicted, independent, opposite];
        demote_contradicted_missing_row_mismatches(&mut verdicts);

        assert!(
            matches!(verdicts[0].status, ClaimStatus::Verified),
            "the Verified verdict is untouched, got {:?}",
            verdicts[0].status
        );
        assert!(
            matches!(verdicts[1].status, ClaimStatus::Unverifiable { .. }),
            "the contradicted missing-row Mismatch must be demoted, got {:?}",
            verdicts[1].status
        );
        assert!(
            matches!(verdicts[2].status, ClaimStatus::Mismatch { .. }),
            "the independent genuinely-absent Mismatch must be preserved, got {:?}",
            verdicts[2].status
        );
        assert!(
            matches!(verdicts[3].status, ClaimStatus::Mismatch { .. }),
            "an opposite-direction Mismatch on a verified pair must NOT be demoted, got {:?}",
            verdicts[3].status
        );
    }

    // ── Multi-gene per-gene PMID binding (extractor cross-product fix) ────────
    //
    // The audited package's final_report.md sentence lists four concordant
    // genes, each with its OWN parenthetical PMID:
    //   "Same-direction concordant genes (4): KLF15 (PMID 28375666, …),
    //    CRISPLD2 (PMID 24926665, …), IRS2 and MFGE8 (PMID 28375666)."
    // The matrix is CORRECT (KLF15→28375666, CRISPLD2→24926665, IRS2→28375666,
    // MFGE8→28375666). The narrative is CORRECT. But the prior extractor bound
    // EVERY PMID in the sentence to EVERY gene (the cross-product), fabricating
    // KLF15↔24926665 and CRISPLD2↔28375666, which then failed the matrix as 12
    // false "narrative cites PMID X but the matrix has no such supporting row"
    // Mismatches. The fix binds each gene only to the PMID(s) in its own
    // proximate citation span.

    /// Build a config from the REAL `interpretation-policy.json` (so `PMID` is
    /// excluded as an entity and "induced" is an up-word, exactly as the
    /// executed package), with literatureGrounding minPapers/minSources=1 so a
    /// single supporting PMID can reach Verified.
    fn real_lit_cfg() -> ExtractorConfig {
        let config_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config");
        let policy_path = config_dir
            .join("downstream-policy")
            .join("interpretation-policy.json");
        let mut policy: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&policy_path).unwrap()).unwrap();
        policy["verifiableEntities"]["literatureGrounding"] =
            json!({"minPapers": 1, "minSources": 1});
        ExtractorConfig::from_policy(&policy).unwrap()
    }

    /// (A) The exact four-gene concordance sentence, end-to-end: each gene binds
    /// ONLY its own parenthetical PMID — KLF15→28375666 (not 24926665),
    /// CRISPLD2→24926665, IRS2 & MFGE8→the shared trailing 28375666 — and all
    /// four VERIFY against the matrix with NO cross-pairing Mismatch emitted.
    #[test]
    fn multi_gene_sentence_binds_each_gene_to_its_own_pmid_and_all_verify() {
        let cfg = real_lit_cfg();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,entity_kind,pmid,evidence_quote,concordance_flag\n\
             ENSG00000163884,KLF15,gene,28375666,\"directly induced by glucocorticoids\",same_direction\n\
             ENSG00000103196,CRISPLD2,gene,24926665,\"dexamethasone increased CRISPLD2 mRNA\",same_direction\n\
             ENSG00000185950,IRS2,gene,28375666,\"glucocorticoid target\",same_direction\n\
             ENSG00000140545,MFGE8,gene,28375666,\"glucocorticoid target\",same_direction\n",
        );

        let sentence = "Same-direction concordant genes (4): KLF15 (PMID 28375666, \
            \"directly induced by glucocorticoids\"), CRISPLD2 (PMID 24926665, \
            \"dexamethasone treatment significantly increased CRISPLD2 mRNA\"), \
            IRS2 and MFGE8 (PMID 28375666).";
        let claims = extract_claims(sentence, &cfg);

        // Per-gene binding: each gene carries ONLY its own proximate PMID, never
        // the cross-product.
        let pmids_of = |gene: &str| -> Vec<u64> {
            let c = claims
                .iter()
                .find(|c| c.entity == gene)
                .unwrap_or_else(|| panic!("no claim extracted for {gene}; got {claims:?}"));
            c.literature_evidence
                .as_ref()
                .unwrap_or_else(|| panic!("{gene} carries no literature_evidence"))
                .cited_pmids
                .clone()
        };
        assert_eq!(
            pmids_of("KLF15"),
            vec![28375666],
            "KLF15 must bind ONLY its own PMID"
        );
        assert_eq!(
            pmids_of("CRISPLD2"),
            vec![24926665],
            "CRISPLD2 must bind ONLY its own PMID"
        );
        assert_eq!(
            pmids_of("IRS2"),
            vec![28375666],
            "IRS2 inherits the shared trailing PMID"
        );
        assert_eq!(
            pmids_of("MFGE8"),
            vec![28375666],
            "MFGE8 binds its trailing-parenthetical PMID"
        );
        // The cross-association is gone: no gene carries another gene's PMID.
        assert!(
            !pmids_of("KLF15").contains(&24926665),
            "KLF15 must NOT carry CRISPLD2's PMID"
        );
        assert!(
            !pmids_of("CRISPLD2").contains(&28375666),
            "CRISPLD2 must NOT carry the others' PMID"
        );

        // End-to-end: all four VERIFY, zero Mismatch.
        for gene in ["KLF15", "CRISPLD2", "IRS2", "MFGE8"] {
            let claim = claims.iter().find(|c| c.entity == gene).unwrap();
            let status = verify_literature_grounded_at(claim, tmp.path(), &cfg);
            assert!(
                matches!(status, ClaimStatus::Verified),
                "{gene} must VERIFY (no cross-pairing Mismatch), got {status:?}"
            );
        }
    }

    /// (B) Real-error-still-caught: the narrowing must not blanket-pass. A
    /// single-gene sentence "FOO (PMID 11111111)" against a matrix that backs
    /// FOO with a DIFFERENT PMID (22222222) is a genuinely wrong cite → Mismatch.
    #[test]
    fn genuinely_wrong_cite_still_mismatches() {
        let cfg = real_lit_cfg();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,entity_kind,pmid,evidence_quote,concordance_flag\n\
             ENSGFOO,FOO,gene,22222222,\"prior work\",same_direction\n",
        );
        let claims = extract_claims("FOO is concordant with prior work (PMID 11111111).", &cfg);
        let foo = claims
            .iter()
            .find(|c| c.entity == "FOO")
            .unwrap_or_else(|| panic!("no FOO claim; got {claims:?}"));
        assert_eq!(
            foo.literature_evidence.as_ref().unwrap().cited_pmids,
            vec![11111111],
            "FOO binds its own (wrong) cited PMID"
        );
        let status = verify_literature_grounded_at(foo, tmp.path(), &cfg);
        assert!(
            matches!(status, ClaimStatus::Mismatch { .. }),
            "a genuinely wrong cite must still Mismatch, got {status:?}"
        );
    }

    /// (C) Single-gene single-PMID regression: the common faithful shape still
    /// Verifies. "BAR was induced (PMID 33333333)" with matrix BAR→33333333.
    #[test]
    fn single_gene_single_pmid_still_verifies() {
        let cfg = real_lit_cfg();
        let tmp = tempdir().unwrap();
        write_lit_matrix(
            tmp.path(),
            "finding_id,entity,entity_kind,pmid,evidence_quote,concordance_flag\n\
             ENSGBAR,BAR,gene,33333333,\"BAR was induced\",same_direction\n",
        );
        let claims = extract_claims(
            "BAR was induced as previously reported (PMID 33333333).",
            &cfg,
        );
        let bar = claims
            .iter()
            .find(|c| c.entity == "BAR")
            .unwrap_or_else(|| panic!("no BAR claim; got {claims:?}"));
        assert_eq!(
            bar.literature_evidence.as_ref().unwrap().cited_pmids,
            vec![33333333],
            "single-gene single-PMID binding unchanged"
        );
        let status = verify_literature_grounded_at(bar, tmp.path(), &cfg);
        assert!(
            matches!(status, ClaimStatus::Verified),
            "single-gene single-PMID faithful claim must Verify, got {status:?}"
        );
    }

    // ── Phase C: KeyedTableCell (composite-key enrichment cell) ──────────────

    /// Faithful twin for [`ClaimContract::KeyedTableCell`]: an enrichment table
    /// whose `Autophagy` term recurs across collections (KEGG vs Reactome) with
    /// DIFFERENT adjusted p-values. The single-entity path collapses the
    /// duplicate "Autophagy" rows to the first; the keyed verifier must
    /// LINEAR-SCAN on BOTH collection AND term so the right row is checked.
    ///
    ///   * WRONG value (2.86e-04) for KEGG/Autophagy → Mismatch (recall: the
    ///     previously-missed subtle padj discrepancy now flips to the right
    ///     verdict).
    ///   * RIGHT value (2.98e-04) → Verified (a genuinely-correct claim passes).
    ///   * The same WRONG value would VERIFY against the decoy Reactome row
    ///     (different padj) if the term key were ignored — proving the composite
    ///     key, not a lone term match, is doing the work.
    #[test]
    fn keyed_cell_autophagy_padj_flips_on_collection_and_term() {
        use crate::claim_extractor::extract_claims;
        let mut cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        // The enrichment table is keyed on `term` (not a gene column), so it
        // must be a configured entity column for the table to load.
        cfg.entity_columns = vec!["term".into(), "gene".into()];
        // Enrichment padj agreement is judged at a tighter relative band than
        // the lenient DE default, so the 2.86e-04-vs-2.98e-04 gap (~4%) is a
        // real disagreement rather than rounding.
        cfg.pvalue_relative_tolerance = 0.01;
        let tmp = tempdir().unwrap();
        // KEGG/Autophagy padj=2.98e-04; a DECOY Reactome/Autophagy row with a
        // DIFFERENT padj (1.20e-02) that the WRONG value must NOT match either.
        write_table(
            tmp.path(),
            "enrichment.tsv",
            "collection\tterm\tNES\tadj_p_value\n\
             KEGG\tAutophagy\t2.10\t2.98e-04\n\
             Reactome\tAutophagy\t1.40\t1.20e-02\n\
             KEGG\tApoptosis\t1.90\t3.30e-03\n",
        );

        // Wrong padj for the KEGG/Autophagy cell → Mismatch.
        let wrong = extract_claims("KEGG Autophagy GSEA padj = 2.86e-04.", &cfg);
        let kw = wrong
            .iter()
            .find(|c| c.contract == ClaimContract::KeyedTableCell)
            .expect("keyed-cell claim extracted");
        assert_eq!(kw.collection.as_deref(), Some("KEGG"), "{kw:?}");
        assert_eq!(kw.term.as_deref(), Some("Autophagy"), "{kw:?}");
        let r_wrong = verify_claims(std::slice::from_ref(kw), tmp.path(), &cfg);
        assert!(
            r_wrong
                .verdicts
                .iter()
                .any(|v| matches!(v.status, ClaimStatus::Mismatch { .. })),
            "wrong KEGG/Autophagy padj must be Mismatch, got {:?}",
            r_wrong
                .verdicts
                .iter()
                .map(|v| &v.status)
                .collect::<Vec<_>>()
        );

        // Right padj → Verified.
        let right = extract_claims("KEGG Autophagy GSEA padj = 2.98e-04.", &cfg);
        let kr = right
            .iter()
            .find(|c| c.contract == ClaimContract::KeyedTableCell)
            .expect("keyed-cell claim extracted");
        let r_right = verify_claims(std::slice::from_ref(kr), tmp.path(), &cfg);
        assert!(
            r_right
                .verdicts
                .iter()
                .any(|v| matches!(v.status, ClaimStatus::Verified)),
            "correct KEGG/Autophagy padj must Verify, got {:?}",
            r_right
                .verdicts
                .iter()
                .map(|v| &v.status)
                .collect::<Vec<_>>()
        );
    }

    /// FP guard for KeyedTableCell: a claim about a collection/term the table
    /// does NOT carry must ABSTAIN (`Unverifiable`), never fire a false
    /// Mismatch. Here the term `Mitophagy` is absent from `enrichment.tsv`.
    #[test]
    fn keyed_cell_absent_term_abstains() {
        use crate::claim_extractor::extract_claims;
        let mut cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        cfg.entity_columns = vec!["term".into(), "gene".into()];
        cfg.pvalue_relative_tolerance = 0.01;
        let tmp = tempdir().unwrap();
        write_table(
            tmp.path(),
            "enrichment.tsv",
            "collection\tterm\tNES\tadj_p_value\nKEGG\tAutophagy\t2.10\t2.98e-04\n",
        );
        let claims = extract_claims("KEGG Mitophagy GSEA padj = 5.00e-03.", &cfg);
        let kw = claims
            .iter()
            .find(|c| c.contract == ClaimContract::KeyedTableCell)
            .expect("keyed-cell claim extracted");
        let report = verify_claims(std::slice::from_ref(kw), tmp.path(), &cfg);
        assert!(
            report
                .verdicts
                .iter()
                .all(|v| matches!(v.status, ClaimStatus::Unverifiable { .. })),
            "absent keyed row must abstain (Unverifiable), got {:?}",
            report
                .verdicts
                .iter()
                .map(|v| &v.status)
                .collect::<Vec<_>>()
        );
    }

    // ── Phase C: QuantileOfColumn (median/mean of a column over a row set) ───

    /// Faithful twin for [`ClaimContract::QuantileOfColumn`]: a DE table whose
    /// `baseMean` median over TESTED genes (non-NA padj rows) is 263.14 while
    /// the ALL-ROWS median is 100.94. The verifier must recompute over the
    /// CORRECT row set.
    ///
    ///   * Claim "median baseMean (tested genes) = 100.94" → Mismatch (the
    ///     all-rows median mislabelled as the tested-genes one — the recall gap
    ///     this closes).
    ///   * Claim "median baseMean (tested genes) = 263.14" → Verified.
    #[test]
    fn quantile_basemean_tested_genes_flips_on_rowset() {
        use crate::claim_extractor::extract_claims;
        let cfg = cfg_with_entity_cols(&["gene", "gene_id"]);
        let tmp = tempdir().unwrap();
        // Tested (non-NA padj) baseMean values: 50, 263.14, 700 → median 263.14.
        // The two NA-padj rows (baseMean 1.0, 2.0) drag the ALL-ROWS median to
        // the midpoint of the sorted {1,2,50,263.14,700} = 50? No — include all
        // five: sorted [1,2,50,263.14,700], median = 50. Add a sixth NA row so
        // the all-rows median lands at 100.94 (mean of 50 and 151.88).
        write_table(
            tmp.path(),
            "de_results.tsv",
            "gene\tbaseMean\tlog2FC\tpadj\n\
             AAA\t50.0\t1.0\t0.01\n\
             BBB\t263.14\t-1.0\t0.001\n\
             CCC\t700.0\t2.0\t0.04\n\
             DDD\t1.0\t0.1\tNA\n\
             EEE\t2.0\t0.2\tNA\n\
             FFF\t151.88\t0.3\tNA\n",
        );
        // Sanity on the fixture: tested {50, 263.14, 700} median = 263.14;
        // all-rows {1,2,50,151.88,263.14,700} median = (50+151.88)/2 = 100.94.

        // Wrong: all-rows median quoted as the tested-genes median → Mismatch.
        let wrong = extract_claims("The median baseMean of tested genes is 100.94.", &cfg);
        let qw = wrong
            .iter()
            .find(|c| c.contract == ClaimContract::QuantileOfColumn)
            .expect("quantile claim extracted");
        assert_eq!(
            qw.aggregate_rowset,
            Some(crate::claim_extractor::QuantileRowSet::TestedGenes),
            "{qw:?}"
        );
        let r_wrong = verify_claims(std::slice::from_ref(qw), tmp.path(), &cfg);
        assert!(
            r_wrong
                .verdicts
                .iter()
                .any(|v| matches!(v.status, ClaimStatus::Mismatch { .. })),
            "all-rows median mislabelled as tested-genes must be Mismatch, got {:?}",
            r_wrong
                .verdicts
                .iter()
                .map(|v| &v.status)
                .collect::<Vec<_>>()
        );

        // Right: the true tested-genes median → Verified.
        let right = extract_claims("The median baseMean of tested genes is 263.14.", &cfg);
        let qr = right
            .iter()
            .find(|c| c.contract == ClaimContract::QuantileOfColumn)
            .expect("quantile claim extracted");
        let r_right = verify_claims(std::slice::from_ref(qr), tmp.path(), &cfg);
        assert!(
            r_right
                .verdicts
                .iter()
                .any(|v| matches!(v.status, ClaimStatus::Verified)),
            "correct tested-genes median must Verify, got {:?}",
            r_right
                .verdicts
                .iter()
                .map(|v| &v.status)
                .collect::<Vec<_>>()
        );
    }

    /// FP guard for QuantileOfColumn: a claim quoting the median of a column the
    /// table does NOT carry must ABSTAIN (`Unverifiable`), never false-Mismatch.
    #[test]
    fn quantile_absent_column_abstains() {
        use crate::claim_extractor::extract_claims;
        let cfg = cfg_with_entity_cols(&["gene", "gene_id"]);
        let tmp = tempdir().unwrap();
        // No `baseMean` column at all.
        write_table(
            tmp.path(),
            "de_results.tsv",
            "gene\tlog2FC\tpadj\nAAA\t1.0\t0.01\nBBB\t-1.0\t0.001\n",
        );
        let claims = extract_claims("The median baseMean of tested genes is 263.14.", &cfg);
        let qc = claims
            .iter()
            .find(|c| c.contract == ClaimContract::QuantileOfColumn)
            .expect("quantile claim extracted");
        let report = verify_claims(std::slice::from_ref(qc), tmp.path(), &cfg);
        assert!(
            report
                .verdicts
                .iter()
                .all(|v| matches!(v.status, ClaimStatus::Unverifiable { .. })),
            "absent column must abstain (Unverifiable), got {:?}",
            report
                .verdicts
                .iter()
                .map(|v| &v.status)
                .collect::<Vec<_>>()
        );
    }
}
