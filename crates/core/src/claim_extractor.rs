//! Regex-first narrative-claim extractor.
//!
//! Takes a free-text report (narrative output from a reporting /
//! interpretation stage) and the `verifiableEntities` block from
//! `interpretation-policy.json`, and returns a list of structured
//! [`Claim`]s. Each claim pairs an *entity* (gene symbol, protein
//! identifier, endpoint code, etc.) with as many of a *direction*, an
//! *effect size*, a *p-value*, and a *source-table reference* as the
//! narrative mentions. The [`claim_verifier`](super::claim_verifier)
//! module consumes the resulting vector and cross-checks each row
//! against the cited table.
//!
//! The extractor is deterministic and policy-driven — no LLM calls, no
//! randomness, no stateful parsing. Brittle to phrasing that does not
//! match the configured patterns, but the audit trail stays
//! reproducible.
//!
//! Design notes:
//!
//! * Sentences are split on `.`, `!`, `?`, and newlines. This is crude
//!   enough to mishandle abbreviations in a formal paper but is good
//!   enough for the short narrative reports the agent emits today.
//! * Entities are collected once per sentence with the policy's
//!   `entityNamePatterns`. A common bioinformatics default is
//!   `[A-Z][A-Z0-9]{1,}`, which matches gene symbols but not
//!   lowercase words — so `cells` stays out of the claim set.
//! * Direction is resolved by nearest-wins match on the policy vocab.
//!   A sentence with both "upregulated" and "downregulated" records
//!   each one against the closest entity rather than assigning the
//!   same direction to every entity in the sentence.
//! * Numeric slots (`log2FC`, `p`, `padj`) are captured with a
//!   key-value regex applied to the whole sentence, then attached to
//!   every claim from that sentence. The verifier can decide whether
//!   to attribute a per-entity value or treat the sentence as an
//!   aggregate.

use anyhow::{anyhow, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;
use ts_rs::TS;

use crate::claim_contract::ClaimContract;

/// Resolve a downstream-policy file by name under `config_dir`, returning the
/// FIRST existing of:
///
/// 1. `config_dir/downstream-policy/<filename>` — the canonical repo layout
///    (`config/downstream-policy/`), so a repo-rooted `config_dir` (the server)
///    resolves exactly as before.
/// 2. `config_dir/<filename>` (flat) — the layout an EMITTED package carries:
///    the emitter copies downstream-policy `.json` files FLAT into
///    `<root>/policies/` with NO `downstream-policy/` subdir. Pointing
///    `config_dir` at the package's own `policies/` resolves here.
///
/// Because the `downstream-policy/` subdir is tried FIRST, this is purely
/// ADDITIVE: any caller passing a repo `config/` (server, tests) is unaffected.
/// Returns `None` when neither location holds the file.
pub fn resolve_policy_file(
    config_dir: &std::path::Path,
    filename: &str,
) -> Option<std::path::PathBuf> {
    let nested = config_dir.join("downstream-policy").join(filename);
    if nested.is_file() {
        return Some(nested);
    }
    let flat = config_dir.join(filename);
    if flat.is_file() {
        return Some(flat);
    }
    None
}

/// Static regex for `classify_contract`'s rank/top-N detector. Hoisted
/// out of the per-call hot path so the pattern is compiled once on
/// first use rather than per sentence.
static RANK_CLASSIFIER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(top[\s-]\d+|top \d+|rank\s*\d+|ranked\b|rank-order|ranking)\b")
        .expect("static regex")
});

/// Static regex for `classify_contract`'s time-series detector. Hoisted
/// for the same reason as `RANK_CLASSIFIER_RE`.
static TIME_SERIES_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\b(day\s*\d+|week\s*\d+|month\s*\d+|hour\s*\d+|timepoint|time[\s-]point|enrolled|n\s*=\s*\d+|peak at|baseline|follow[\s-]up)\b"
    )
    .expect("static regex")
});

/// Static regex for `classify_contract`'s literature-grounding PMID
/// detector. Matches a `PMID 12345678` / `PMID: 12345678` / `pmid12345678`
/// citation token. Hoisted for the same reason as `RANK_CLASSIFIER_RE`.
static PMID_CITATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bpmid\s*:?\s*\d{4,9}\b").expect("static regex"));

/// Static regex for `extract_claims`'s sentence splitter. Hoisted so
/// every narrative parse reuses the same compiled DFA.
static SENTENCE_SPLITTER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[.!?\u{FF01}\u{FF0E}\u{FF1F}\u{2026}]+\s+|\n+").expect("static regex compiles")
});

/// Static regex for `scan_table_reference`. Hoisted so every sentence
/// scan reuses one compiled pattern instead of recompiling per call.
static TABLE_REF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)Table\s+S?[0-9A-Za-z_\-]+").expect("static regex"));

/// Pre-built regex set for the dynamic per-keyword scanners in
/// `extract_claims`. Built once from `ExtractorConfig` (plus the
/// baked-in default keywords) so the per-sentence scan loop reuses
/// compiled regexes instead of recompiling per-(sentence × keyword).
///
/// Keyword iteration order matches the prior `Vec` + `.iter().any()`
/// dedup: configured columns first (in `ExtractorConfig` order), then
/// the baked-in defaults. The scanners short-circuit on the first
/// match, so preserving this ordering keeps the returned f64 value
/// byte-identical to the original implementation.
pub(crate) struct ExtractorRegexCache {
    pub effect_size: Vec<(String, Regex)>,
    pub pvalue: Vec<(String, Regex)>,
}

impl ExtractorRegexCache {
    pub(crate) fn build(cfg: &ExtractorConfig) -> Self {
        // Replicate the original dedup semantics: lower-case the
        // configured columns, then append each baked-in default unless
        // already present. Preserves first-occurrence-wins order.
        let mut effect_keywords: Vec<String> = cfg
            .effect_size_columns
            .iter()
            .map(|c| c.to_lowercase())
            .collect();
        // Baked-in defaults + the SPACED/full-word log2 prose forms (VF-3) so
        // "log2 fold change of -4.2" parses the same as "log2FC=-4.2". Only
        // EXPLICITLY-log2 phrases are added — bare "fold change" is linear and
        // is handled separately, so a linear magnitude is never mis-read as log2.
        for extra in [
            "log2fc",
            "logfc",
            "log2 fold change",
            "log2-fold change",
            "log2 foldchange",
        ] {
            if !effect_keywords.iter().any(|k| k == extra) {
                effect_keywords.push(extra.into());
            }
        }
        let mut pvalue_keywords: Vec<String> = cfg
            .pvalue_columns
            .iter()
            .map(|c| c.to_lowercase())
            .collect();
        for extra in ["pvalue", "p_value", "padj", "fdr", "p"] {
            if !pvalue_keywords.iter().any(|k| k == extra) {
                pvalue_keywords.push(extra.into());
            }
        }
        let effect_size = effect_keywords
            .into_iter()
            .map(|kw| {
                // VF-3 — accept the prose separator `of` in addition to `:`/`=`,
                // so "log2FC of 3.5" / "log2 fold change of -4.2" parse. The
                // `of` arm requires a digit (optionally signed) IMMEDIATELY
                // after, so cutoff phrasing like "log2FC of at least 1" does NOT
                // capture a value (no false effect-size slot → no false flag).
                let pat = format!(
                    r"(?i){}(?:\s*[:=]\s*|\s+of\s+)(-?\d+(?:\.\d+)?(?:[eE]-?\d+)?)",
                    regex::escape(&kw)
                );
                let re = Regex::new(&pat).expect("static-shape regex");
                (kw, re)
            })
            .collect();
        let pvalue = pvalue_keywords
            .into_iter()
            .map(|kw| {
                let pat = format!(
                    r"(?i)(?:\b|,|\s){}\s*[:=]\s*(\d+(?:\.\d+)?(?:[eE]-?\d+)?)",
                    regex::escape(&kw)
                );
                let re = Regex::new(&pat).expect("static-shape regex");
                (kw, re)
            })
            .collect();
        Self {
            effect_size,
            pvalue,
        }
    }
}

/// Direction a claim asserts.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    TS,
    schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Up variant.
    Up,
    /// Down variant.
    Down,
}

/// Literature-support evidence attached to a [`ClaimContract::LiteratureGrounded`]
/// claim: the upstream finding the narrative is grounding and the PMIDs the
/// narrative cites for it. The verifier cross-checks these against the
/// `claims_evidence_matrix.csv` row for `finding_id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct LiteratureEvidence {
    /// Upstream finding id the claim grounds (matches the `finding_id`
    /// column of `claims_evidence_matrix.csv`).
    pub finding_id: String,
    /// PMIDs the narrative cites in support, as parsed integers.
    pub cited_pmids: Vec<u64>,
}

/// One extracted narrative claim. Fields beyond `entity` and `excerpt`
/// are optional — if the narrative omits an effect size, the claim
/// still records the direction and the source table reference, and
/// the verifier falls back to verifying only what was provided.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct Claim {
    /// The entity name matched by one of the policy's entity patterns.
    pub entity: String,
    /// Direction word captured in the same sentence, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub direction: Option<Direction>,
    /// Numeric effect size captured from a log2FC / effect_size mention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effect_size: Option<f64>,
    /// Numeric p-value / FDR captured from the sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pvalue: Option<f64>,
    /// Source-table reference captured from a "Table S1" / "(Table 2)"
    /// style mention in the sentence. Populated as a free string so
    /// the verifier can fuzzy-match against the package's
    /// `results/tables/*` index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_table: Option<String>,
    /// The sentence the claim originates from, for operator review
    /// of mismatches.
    pub excerpt: String,
    /// Contract class assigned by heuristic during extraction. Defaults
    /// to `NumericTableLookup` for backwards compatibility with claims
    /// serialized before this field was introduced.
    #[serde(default = "ClaimContract::default_numeric")]
    pub contract: ClaimContract,
    /// Literature-support evidence for a `LiteratureGrounded` claim. `None`
    /// for every other contract class and for claims serialized before this
    /// field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub literature_evidence: Option<LiteratureEvidence>,
    /// The p-value-family keyword the narrative used for the parsed `pvalue`
    /// ("padj"/"fdr"/"q…" → adjusted; "pvalue"/"p" → raw). Lets the verifier
    /// reject a value quoted under the wrong label (a raw value labelled
    /// "padj"). `None` when no p-value was parsed. (VF-8)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub matched_pvalue_keyword: Option<String>,
    /// A LINEAR fold-change magnitude parsed from prose ("induced 8-fold",
    /// "2.3-fold higher"), distinct from the log2 `effect_size`. The verifier
    /// converts it (log2 of the ratio, signed by direction) before comparing,
    /// so a linear claim is reconciled against a log2 table. `None` when no
    /// linear-fold phrase was found. (VF-4)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub linear_fold: Option<f64>,
}

/// Policy-driven extractor configuration. Parsed once from the
/// `verifiableEntities` block of `interpretation-policy.json`.
#[derive(Debug, Clone)]
pub struct ExtractorConfig {
    /// Entity patterns.
    pub entity_patterns: Vec<Regex>,
    /// Anchored deny-list patterns compiled from `entityNameExcludePatterns`.
    /// Any entity match whose full text matches one of these patterns is
    /// dropped before the claim is recorded. Missing field → empty vec
    /// (backward-compatible: no exclusions applied).
    pub entity_exclude_patterns: Vec<Regex>,
    /// Up words.
    pub up_words: Vec<String>,
    /// Down words.
    pub down_words: Vec<String>,
    /// Effect size columns.
    pub effect_size_columns: Vec<String>,
    /// Pvalue columns.
    pub pvalue_columns: Vec<String>,
    /// Entity columns.
    pub entity_columns: Vec<String>,
    /// Log2fc tolerance.
    pub log2fc_tolerance: f64,
    /// Pvalue relative tolerance.
    pub pvalue_relative_tolerance: f64,
    /// Minimum number of distinct supporting papers (PMIDs) a
    /// literature-grounded claim must cite to be Verified. Default 2.
    pub literature_min_papers: usize,
    /// Minimum number of distinct evidence `source_kind`s backing a
    /// literature-grounded claim. Default 1.
    pub literature_min_sources: usize,
}

impl ExtractorConfig {
    /// Pick the `verifiableEntities` block that matches the session's
    /// `ProjectClass`. The overlay file
    /// (`interpretation-policy.<class>.json`, when present) wins on
    /// every field it specifies; fields absent from the overlay fall
    /// through to the base `interpretation-policy.json`.
    ///
    /// `config_dir` is the policy root: either a repo `config/` (the overlay
    /// resolves under `config/downstream-policy/`) or an emitted package's own
    /// `policies/` (the overlay resolves flat) — [`resolve_policy_file`]
    /// encodes the downstream-policy-first precedence. When no overlay exists
    /// for the class, or the class is `Bioinformatics`, the base policy is used
    /// unchanged.
    pub fn from_policy_for_class(
        base_policy: &Value,
        config_dir: &std::path::Path,
        class: crate::project_class::ProjectClass,
    ) -> Result<Self> {
        let overlay_name = format!("interpretation-policy.{}.json", class.as_str());
        let merged = if let Some(overlay_path) = resolve_policy_file(config_dir, &overlay_name) {
            let overlay_bytes = std::fs::read(&overlay_path)
                .with_context(|| format!("reading overlay policy '{}'", overlay_path.display()))?;
            let overlay: Value = serde_json::from_slice(&overlay_bytes)
                .with_context(|| format!("parsing overlay policy '{}'", overlay_path.display()))?;
            merge_overlay(base_policy.clone(), &overlay)
        } else {
            base_policy.clone()
        };
        Self::from_policy(&merged)
    }

    /// From policy.
    pub fn from_policy(policy: &Value) -> Result<Self> {
        let ve = policy
            .get("verifiableEntities")
            .ok_or_else(|| anyhow!("policy missing `verifiableEntities`"))?;
        let enabled = ve.get("enabled").and_then(Value::as_bool).unwrap_or(false);
        if !enabled {
            return Err(anyhow!("`verifiableEntities.enabled` is false"));
        }

        let entity_patterns = read_string_list(ve, "entityNamePatterns")?
            .into_iter()
            .map(|p| Regex::new(&p).map_err(|e| anyhow!("bad entity pattern `{}`: {}", p, e)))
            .collect::<Result<Vec<_>>>()?;

        // Optional deny-list of anchored regex patterns. An entity match
        // whose full text is matched by any of these patterns is dropped
        // from the extracted claim set. Missing field is treated as an
        // empty denylist (backward-compatible).
        let entity_exclude_patterns = ve
            .get("entityNameExcludePatterns")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|p| {
                        Regex::new(p).map_err(|e| anyhow!("bad exclude pattern `{}`: {}", p, e))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .transpose()?
            .unwrap_or_default();

        let vocab = ve
            .get("directionVocab")
            .ok_or_else(|| anyhow!("missing directionVocab"))?;
        let up_words = read_string_list(vocab, "up")?;
        let down_words = read_string_list(vocab, "down")?;

        let effect_size_columns = read_string_list(ve, "effectSizeColumns")?;
        let entity_columns = read_string_list(ve, "entityColumns")?;
        let pvalue_columns = ve
            .get("pvalueColumns")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let tolerance = ve.get("tolerance");
        let log2fc_tolerance = tolerance
            .and_then(|t| t.get("log2FcAbsoluteDelta"))
            .and_then(Value::as_f64)
            .unwrap_or(0.05);
        let pvalue_relative_tolerance = tolerance
            .and_then(|t| t.get("pvalueRelativeDelta"))
            .and_then(Value::as_f64)
            .unwrap_or(0.1);

        // Literature-grounding thresholds for `LiteratureGrounded` claims.
        // Missing block falls back to the defaults (2 papers, 1 source) so
        // policies authored before the block existed keep working.
        let lit = ve.get("literatureGrounding");
        let literature_min_papers = lit
            .and_then(|l| l.get("minPapers"))
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(2);
        let literature_min_sources = lit
            .and_then(|l| l.get("minSources"))
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(1);

        Ok(Self {
            entity_patterns,
            entity_exclude_patterns,
            up_words,
            down_words,
            effect_size_columns,
            pvalue_columns,
            entity_columns,
            log2fc_tolerance,
            pvalue_relative_tolerance,
            literature_min_papers,
            literature_min_sources,
        })
    }
}

/// Shallow-merge `overlay` on top of `base`. Objects recurse one
/// level; arrays and scalars on the overlay *replace* the corresponding
/// base value wholesale. This is exactly what the verifiableEntities
/// block needs — the overlay lists the effect-size columns and entity
/// patterns for the class, and those
/// must replace bio's defaults rather than append to them.
fn merge_overlay(mut base: Value, overlay: &Value) -> Value {
    match (base.as_object_mut(), overlay.as_object()) {
        (Some(base_obj), Some(overlay_obj)) => {
            for (k, v) in overlay_obj {
                match (base_obj.get_mut(k), v) {
                    (Some(existing @ Value::Object(_)), Value::Object(_)) => {
                        let merged = merge_overlay(existing.clone(), v);
                        *existing = merged;
                    }
                    _ => {
                        base_obj.insert(k.clone(), v.clone());
                    }
                }
            }
            base
        }
        _ => overlay.clone(),
    }
}

fn read_string_list(v: &Value, key: &str) -> Result<Vec<String>> {
    v.get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("policy missing `{}` array", key))?
        .iter()
        .map(|item| {
            item.as_str()
                .map(String::from)
                .ok_or_else(|| anyhow!("`{}` contains a non-string element", key))
        })
        .collect()
}

/// Classify a sentence into a [`ClaimContract`] by heuristic keyword scan.
///
/// The rules are applied in priority order — the first match wins. The
/// default when no keyword fires is `NumericTableLookup` (direct table
/// cell lookup), which is the broadest and lowest-specificity class.
///
/// Priority:
/// 1. `ThresholdedDeOrEnrichment` — FDR / padj / p< threshold patterns.
/// 2. `LiteratureGrounded` — prior-literature / previous-finding / concordance
///    cues, or a `PMID` citation token.
/// 3. `RankTopN` — "top N" / "rank" constructs.
/// 4. `GroupComparison` — directional group comparisons ("vs", "higher than", "lower than").
/// 5. `Categorical` — cluster / label / category assignments.
/// 6. `TimeSeriesSummary` — day / week / month / enrolled / timepoint patterns.
/// 7. `NumericTableLookup` — fallback.
pub fn classify_contract(sentence: &str) -> ClaimContract {
    let lower = sentence.to_lowercase();

    // Thresholded DE / enrichment: an explicit threshold comparison or a
    // significance ASSERTION.
    let threshold_keywords = [
        "fdr",
        "padj",
        "p<",
        "p <",
        "p-value <",
        "p-value<",
        "adjusted p",
        "q-value",
        "fdr<",
        "fdr <",
        "significance threshold",
        "bonferroni",
    ];
    // A proximity *hedge* ("near/approaching the significance threshold",
    // "marginally significant") asserts the result is NOT (yet) significant, so
    // it must not be checked against FDR < 0.05 — doing so flags an honestly
    // near-threshold gene as a fabrication. Phrases are significance-anchored so
    // unrelated words ("linear", "nuclear") don't match a bare "near".
    let proximity_hedges = [
        "near the significance",
        "near significance",
        "near the threshold",
        "near the fdr",
        "approaching significance",
        "approaching the significance",
        "close to significance",
        "close to the significance",
        "short of significance",
        "just below significance",
        "just shy of significance",
        "trend toward significance",
        "trending toward significance",
        "marginally significant",
        "marginal significance",
        "borderline significant",
        "borderline significance",
        "nominally significant",
    ];
    let has_threshold_kw = threshold_keywords.iter().any(|kw| lower.contains(kw));
    let is_proximity_hedge = proximity_hedges.iter().any(|kw| lower.contains(kw));
    // An explicit numeric comparator ("FDR < 0.05") is a real, checkable
    // assertion even alongside hedging language, so it stays thresholded.
    let has_explicit_comparator = lower.contains('<') || lower.contains('≤');
    if has_threshold_kw && !(is_proximity_hedge && !has_explicit_comparator) {
        return ClaimContract::ThresholdedDeOrEnrichment;
    }

    // Literature-grounded support: a PMID citation token, or prose grounding
    // the result in prior literature / a previous finding / concordance with
    // earlier work. These are checked against the PMID-anchored
    // `claims_evidence_matrix.csv` rather than a numeric result table.
    let literature_keywords = [
        "prior literature",
        "prior reports",
        "prior report",
        "prior studies",
        "prior study",
        "prior work",
        "previous literature",
        "previous finding",
        "previous findings",
        "previous report",
        "previous reports",
        "previous studies",
        "previous study",
        "previous work",
        "earlier work",
        "earlier studies",
        "earlier study",
        "published literature",
        "published reports",
        "literature support",
        "supported by the literature",
        "consistent with prior",
        "consistent with previous",
        "concordant with",
        "concordance with",
        "in concordance",
        "concordant",
        "in agreement with prior",
        "in agreement with previous",
        "as previously reported",
        "as previously described",
        "previously reported",
        "previously described",
    ];
    let has_literature_kw = literature_keywords.iter().any(|kw| lower.contains(kw));
    if PMID_CITATION_RE.is_match(&lower) || has_literature_kw {
        return ClaimContract::LiteratureGrounded;
    }

    // Rank / top-N membership.
    if RANK_CLASSIFIER_RE.is_match(&lower) {
        return ClaimContract::RankTopN;
    }

    // Group comparison: directional comparison language.
    let group_keywords = [
        " vs ",
        " vs. ",
        "higher than",
        "lower than",
        "greater than",
        "less than",
        "compared to",
        "compared with",
        "-fold",
        "fold-change",
        "fold change",
        "enriched in",
        "depleted in",
        "between groups",
        "treatment vs",
        "control vs",
    ];
    if group_keywords.iter().any(|kw| lower.contains(kw)) {
        return ClaimContract::GroupComparison;
    }

    // Categorical label / cluster assignment.
    let cat_keywords = [
        "cluster",
        "cell type",
        "cell-type",
        "label",
        "annotated as",
        "identified as",
        "classified as",
        "category",
        "subtype",
        "phenotype",
        "signature",
    ];
    if cat_keywords.iter().any(|kw| lower.contains(kw)) {
        return ClaimContract::Categorical;
    }

    // Time-series / clinical summary.
    if TIME_SERIES_RE.is_match(&lower) {
        return ClaimContract::TimeSeriesSummary;
    }

    ClaimContract::NumericTableLookup
}

/// Extract every claim the configured patterns can identify in `text`.
///
/// Ordering is preserved — callers that want to stream the results in
/// document order can iterate the returned Vec directly. Duplicate
/// `(entity, direction)` pairs within the same sentence collapse to one
/// claim; across sentences they do not (a report that says "ACAN was
/// upregulated" in two places yields two claims so that both occurrences
/// are verifiable).
pub fn extract_claims(text: &str, cfg: &ExtractorConfig) -> Vec<Claim> {
    // ECAA_ABLATE_CLAIM_CONSISTENCY suppression deliberately lives at
    // the emit-write site (crates/conversation/src/emit/sidecars.rs
    // ::write_claim_verification) so the runtime /verify endpoint can
    // still extract claims under the ablation flag. Do not re-add a
    // short-circuit here.
    //
    // Split on terminal punctuation followed by whitespace (or on bare
    // newlines) so we never chop a decimal number in half. Common
    // abbreviations ("et al.", "Fig.", "Dr.", "vs.", "e.g.", "i.e.")
    // are guarded with a negative-lookahead-style preprocessor: we
    // temporarily substitute their trailing period with a sentinel
    // before splitting, then restore them inside each fragment. Also
    // treats the Unicode sentence terminators (full-width period
    // U+FF0E, ellipsis U+2026, full-width !? U+FF01/FF1F) the same as
    // ASCII.
    let sentence_splitter = &*SENTENCE_SPLITTER_RE;
    // Build the per-keyword regex cache once per `extract_claims` call
    // so the hot per-sentence scanners reuse compiled regexes instead
    // of rebuilding them for each (sentence × keyword) pair.
    let regex_cache = ExtractorRegexCache::build(cfg);
    // Sentinel chosen so it can't appear in legitimate input — the BEL
    // control character (U+0007).
    const ABBREV_SENTINEL: char = '\u{0007}';
    let preprocessed = {
        let mut s = canonicalize_scientific(text);
        for abbrev in &[
            "et al.", "Fig.", "fig.", "Tab.", "tab.", "Dr.", "Mr.", "Mrs.", "Ms.", "Prof.", "e.g.",
            "i.e.", "vs.", "cf.", "approx.", "ca.", "No.", "no.",
        ] {
            let replacement = format!("{}{}", &abbrev[..abbrev.len() - 1], ABBREV_SENTINEL);
            s = s.replace(abbrev, &replacement);
        }
        s
    };
    let mut out: Vec<Claim> = Vec::new();

    for raw_sentence in sentence_splitter.split(&preprocessed) {
        // Restore the period after splitting so downstream regexes see
        // the original surface form (claim entity-patterns may rely on
        // it).
        let restored: String = raw_sentence.replace(ABBREV_SENTINEL, ".");
        let sentence = restored.as_str();
        let trimmed = sentence.trim();
        if trimmed.is_empty() {
            continue;
        }
        // Skip markdown table rows (`| FBgn… | -4.6 | 1e-159 |`) and
        // separator rows. These are parsed structurally by
        // `extract_markdown_table_claims`, which maps cells to
        // entity/effect/pvalue by header; letting the prose scanner also
        // mine them produces duplicate, value-less, or mis-parsed claims.
        if trimmed.starts_with('|') || trimmed.matches('|').count() >= 2 {
            continue;
        }

        // Every entity-pattern match in the sentence, with their byte
        // offsets so "nearest direction" is deterministic. Matches whose
        // full text satisfies any `entity_exclude_patterns` pattern are
        // dropped before the claim set is built — this filters common-noun
        // acronyms (RNA, PCR, DNA, WHO) that the broad gene-symbol regex
        // otherwise captures.
        let table_ref_spans = table_reference_spans(trimmed);
        let mut raw_entity_hits: Vec<EntityHit> = Vec::new();
        for pat in &cfg.entity_patterns {
            for m in pat.find_iter(trimmed) {
                let token = m.as_str();
                let excluded = cfg
                    .entity_exclude_patterns
                    .iter()
                    .any(|excl| excl.is_match(token));
                if excluded
                    || is_embedded_in_alnum_token(trimmed, m.start(), m.end())
                    || table_ref_spans
                        .iter()
                        .any(|(start, end)| *start <= m.start() && m.end() <= *end)
                {
                    continue;
                }
                raw_entity_hits.push(EntityHit {
                    start: m.start(),
                    end: m.end(),
                    token: token.to_string(),
                });
            }
        }
        let mut entity_hits = select_longest_non_overlapping_entity_hits(raw_entity_hits);
        if entity_hits.is_empty() {
            continue;
        }
        entity_hits.sort_by_key(|(start, _)| *start);

        // All direction-word positions.
        let lowered = trimmed.to_lowercase();
        let mut direction_hits: Vec<(usize, Direction)> = Vec::new();
        for w in &cfg.up_words {
            for (pos, _) in lowered.match_indices(&w.to_lowercase()) {
                direction_hits.push((pos, Direction::Up));
            }
        }
        for w in &cfg.down_words {
            for (pos, _) in lowered.match_indices(&w.to_lowercase()) {
                direction_hits.push((pos, Direction::Down));
            }
        }

        let effect_size_hits = scan_effect_size_positions(trimmed, &regex_cache);
        let pvalue_hits = scan_pvalue_positions(trimmed, &regex_cache);
        let source_table = scan_table_reference(trimmed);
        let contract = classify_contract(trimmed);

        // For a literature-grounded sentence, capture the cited PMIDs once; each
        // entity then becomes a finding_id the verifier resolves against
        // claims_evidence_matrix.csv. Non-literature contracts carry no PMIDs.
        let cited_pmids: Vec<u64> = if contract == ClaimContract::LiteratureGrounded {
            PMID_CITATION_RE
                .find_iter(trimmed)
                .filter_map(|m| {
                    m.as_str()
                        .chars()
                        .filter(|c| c.is_ascii_digit())
                        .collect::<String>()
                        .parse::<u64>()
                        .ok()
                })
                .collect()
        } else {
            Vec::new()
        };

        let mut seen: std::collections::BTreeSet<(String, Option<Direction>)> =
            std::collections::BTreeSet::new();
        for (ent_pos, ent_name) in entity_hits {
            let direction = nearest_direction(ent_pos, &direction_hits);
            let effect_size = value_for_entity(ent_pos, &effect_size_hits);
            let (pvalue, matched_pvalue_keyword) = pvalue_for_entity(ent_pos, &pvalue_hits);
            let key = (ent_name.clone(), direction);
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            // Note: report-control ALLCAPS noise (PASS/FAIL/GATING/…) is removed
            // precisely by the policy `entityNameExcludePatterns` deny-list, not
            // by a slot heuristic. A real gene symbol mentioned in prose with no
            // inline number is deliberately KEPT as a candidate claim: the
            // discovery verifier resolves it against the result tables by entity
            // membership (now that the DE table loads), so dropping it here would
            // reduce verifiable-claim recall — the opposite of the intent.
            let literature_evidence = if contract == ClaimContract::LiteratureGrounded {
                Some(LiteratureEvidence {
                    finding_id: ent_name.clone(),
                    cited_pmids: cited_pmids.clone(),
                })
            } else {
                None
            };
            out.push(Claim {
                entity: ent_name,
                direction,
                effect_size,
                pvalue,
                source_table: source_table.clone(),
                excerpt: trimmed.to_string(),
                contract,
                literature_evidence,
                matched_pvalue_keyword,
                linear_fold: None,
            });
        }
    }

    out
}

/// Extract claims from GitHub-flavored markdown tables embedded in `text`.
///
/// Real agent reports present per-entity results as a markdown table
/// ("| Gene | log2FC | padj |") rather than as "GENEX (log2FC=…)" prose,
/// so the sentence scanner misses them entirely. This recognizes a
/// header row immediately followed by a `|---|---|` separator, maps the
/// header cells to entity / effect-size / p-value roles by name, and
/// emits one [`Claim`] per data row whose entity cell matches a
/// configured entity pattern. The rows carry no "Table S1" citation, so
/// `source_table` is left `None` for the verifier's table-discovery step
/// to resolve against the file the agent actually wrote.
pub fn extract_markdown_table_claims(text: &str, cfg: &ExtractorConfig) -> Vec<Claim> {
    let regex_cache = ExtractorRegexCache::build(cfg);
    let text = canonicalize_scientific(text);
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Claim> = Vec::new();

    let is_separator = |cells: &[String]| -> bool {
        !cells.is_empty()
            && cells
                .iter()
                .all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':' || ch == ' '))
    };

    let mut i = 0;
    while i + 1 < lines.len() {
        let header_line = lines[i];
        if !header_line.contains('|') {
            i += 1;
            continue;
        }
        let headers = split_md_row(header_line);
        let sep = split_md_row(lines[i + 1]);
        if headers.len() < 2 || sep.len() != headers.len() || !is_separator(&sep) {
            i += 1;
            continue;
        }
        let Some(roles) = table_column_roles(&headers, cfg) else {
            i += 2;
            continue;
        };

        // Consume data rows.
        let mut j = i + 2;
        while j < lines.len() && lines[j].contains('|') {
            let cells = split_md_row(lines[j]);
            if cells.len() != headers.len() {
                break;
            }
            // Re-scan the raw row text for key=value numerics too, in case
            // the agent wrote "log2FC=…" inside a cell; `None` source_table
            // because markdown rows carry no "Table S1" citation (the
            // verifier's discovery step resolves the backing file).
            if let Some(claim) =
                claim_from_table_row(&cells, &roles, lines[j], None, cfg, &regex_cache)
            {
                out.push(claim);
            }
            j += 1;
        }
        i = j.max(i + 2);
    }
    out
}

/// Column-role indices for a result table: which header column carries the
/// entity, the effect size, and the p-value. Produced by
/// [`table_column_roles`] and consumed by [`claim_from_table_row`] so the
/// markdown-table and delimited-file (TSV/CSV) extractors share one mapping
/// and one per-row emission path.
struct TableColumnRoles {
    entity_idx: usize,
    effect_idx: Option<usize>,
    pvalue_idx: Option<usize>,
}

/// Map header cells to entity / effect-size / p-value roles by EXACT
/// cleaned-header match. A substring test wrongly maps count / annotation
/// columns: e.g. "Top up-gene" contains "gene" and "N sig (FDR<0.05)"
/// contains "fdr", which would mis-read a cluster-summary table as per-gene
/// DE rows and emit false claims. `clean_header` lowercases, drops any
/// "(…)" qualifier, and normalizes separators so a header is matched only
/// when it *names* the role. Returns `None` when no entity column is found
/// (a table with no recognizable entity yields no claims).
fn table_column_roles(headers: &[String], cfg: &ExtractorConfig) -> Option<TableColumnRoles> {
    let clean: Vec<String> = headers.iter().map(|h| clean_header(h)).collect();
    let find_col = |variants: &[&str], cfg_cols: &[String]| -> Option<usize> {
        clean.iter().position(|h| {
            variants.iter().any(|v| h == v) || cfg_cols.iter().any(|c| clean_header(c) == *h)
        })
    };
    let entity_idx = find_col(
        &[
            "gene",
            "gene id",
            "gene name",
            "gene symbol",
            "feature",
            "feature id",
            "symbol",
            "term",
            "id",
            "name",
            "protein",
            "protein id",
            "peak",
            "peak id",
            "region",
            "taxon",
            "taxon id",
            "taxon name",
            "otu",
            "otu id",
            "asv",
            "cpg",
            "cpg id",
            "site",
            "probe",
            "probe id",
            "variant",
            "snp",
            "rsid",
            "transcript",
            "transcript id",
            "uniprot",
            "accession",
            "entity",
        ],
        &cfg.entity_columns,
    )?;
    let effect_idx = find_col(
        &[
            "log2fc",
            "log2 fc",
            "logfc",
            "log fc",
            "logfoldchange",
            "log2foldchange",
            "log2 fold change",
            "log fold change",
            "lfc",
            "fold change",
            "nes",
            "effect size",
            "effect",
            "estimate",
            "beta",
            "es",
        ],
        &cfg.effect_size_columns,
    );
    let pvalue_idx = find_col(
        &[
            "padj",
            "p adj",
            "p adjust",
            "adj p",
            "adj p val",
            "adj p value",
            "adjusted p value",
            "fdr",
            "q value",
            "qvalue",
            "q val",
            "pvalue",
            "p value",
            "pval",
            "fdr q value",
        ],
        &cfg.pvalue_columns,
    );
    Some(TableColumnRoles {
        entity_idx,
        effect_idx,
        pvalue_idx,
    })
}

/// Build one `NumericTableLookup` [`Claim`] from a single result-table row,
/// or `None` if the row's entity cell does not match a configured entity
/// pattern or the row carries no checkable numeric slot. `row_text` is the
/// raw line, re-scanned for in-cell `log2FC=…` key/value numerics.
/// `source_table` is the file basename for a delimited file (so the verifier
/// reads the cited table directly) or `None` for a markdown row (so the
/// verifier's discovery step resolves the backing file). Applies the same
/// all-`None` guard as the prose path: a row with a recognized entity but no
/// direction / effect size / p-value is unverifiable noise and is dropped.
fn claim_from_table_row(
    cells: &[String],
    roles: &TableColumnRoles,
    row_text: &str,
    source_table: Option<String>,
    cfg: &ExtractorConfig,
    regex_cache: &ExtractorRegexCache,
) -> Option<Claim> {
    let raw_entity = cells.get(roles.entity_idx)?;
    let entity = matched_entity_token(raw_entity, cfg)?;
    let effect_size = roles
        .effect_idx
        .and_then(|k| cells.get(k))
        .and_then(|c| parse_leading_number(c));
    let pvalue = roles
        .pvalue_idx
        .and_then(|k| cells.get(k))
        .and_then(|c| parse_leading_number(c));
    let direction = effect_size.map(|e| {
        if e >= 0.0 {
            Direction::Up
        } else {
            Direction::Down
        }
    });
    // Re-scan the row text for key=value numerics too, in case the agent
    // wrote "log2FC=…" inside a cell.
    let effect_size = effect_size.or_else(|| {
        scan_effect_size_positions(row_text, regex_cache)
            .first()
            .map(|(_, v)| *v)
    });
    let pvalue = pvalue.or_else(|| {
        scan_pvalue_positions(row_text, regex_cache)
            .first()
            .map(|(_, v, _)| *v)
    });
    // C2 all-None guard: a row whose entity matched but that has no
    // direction, effect size, or p-value carries nothing for the verifier
    // to check (markdown rows always have source_table=None, so it is not
    // part of the test). Drop it as unverifiable noise rather than emitting
    // a bare claim.
    if direction.is_none() && effect_size.is_none() && pvalue.is_none() {
        return None;
    }
    Some(Claim {
        entity,
        direction,
        effect_size,
        pvalue,
        source_table,
        excerpt: row_text.trim().to_string(),
        contract: ClaimContract::NumericTableLookup,
        literature_evidence: None,
        matched_pvalue_keyword: None,
        linear_fold: None,
    })
}

/// Mine a delimited (TSV/CSV) result table for per-row claims.
///
/// Reads `reader` as a delimited table (header row + data rows, split on
/// `delimiter`), maps the header columns to entity / effect-size / p-value
/// roles by name (shared with [`extract_markdown_table_claims`] via
/// [`table_column_roles`]), and emits one [`ClaimContract::NumericTableLookup`]
/// [`Claim`] per data row whose entity cell matches a configured entity
/// pattern AND that carries at least one numeric slot (the C2 all-`None`
/// guard, applied in [`claim_from_table_row`]). Each emitted claim's
/// `source_table` is left `None` here; the caller (`finalize.rs`) sets it to
/// the file's basename so the verifier reads the cited table directly. Reuses
/// the same `matched_entity_token` / `scan_effect_size_positions` /
/// `scan_pvalue_positions` helpers as the prose and markdown paths.
pub fn extract_delimited_table_claims<R: std::io::Read>(
    reader: R,
    delimiter: u8,
    cfg: &ExtractorConfig,
) -> Vec<Claim> {
    let regex_cache = ExtractorRegexCache::build(cfg);
    let mut csv_reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(true)
        .from_reader(reader);
    let Ok(header_record) = csv_reader.headers() else {
        return Vec::new();
    };
    let headers: Vec<String> = header_record.iter().map(|h| h.to_string()).collect();
    let Some(roles) = table_column_roles(&headers, cfg) else {
        return Vec::new();
    };
    let mut out: Vec<Claim> = Vec::new();
    for record in csv_reader.records().flatten() {
        let cells: Vec<String> = record.iter().map(|c| c.to_string()).collect();
        // Canonicalize the row so Unicode scientific notation / minus signs
        // parse the same way the prose and markdown paths do.
        let row_text = canonicalize_scientific(&cells.join(" "));
        let canon_cells: Vec<String> = cells.iter().map(|c| canonicalize_scientific(c)).collect();
        if let Some(claim) =
            claim_from_table_row(&canon_cells, &roles, &row_text, None, cfg, &regex_cache)
        {
            out.push(claim);
        }
    }
    out
}

/// Normalize a table header for exact role matching: lowercase, drop any
/// parenthetical qualifier ("N sig (FDR<0.05)" → "n sig"), map `._-` to
/// spaces, and collapse whitespace. So a column is matched to a role only
/// when its name *is* that role, never when it merely mentions it.
fn clean_header(h: &str) -> String {
    let mut s = h.to_lowercase();
    if let Some(idx) = s.find('(') {
        s.truncate(idx);
    }
    s = s.replace(['.', '_', '-', '`'], " ");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split a markdown table row into trimmed cell strings, dropping the
/// empty leading/trailing cells produced by the outer pipes.
fn split_md_row(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    let inner = trimmed
        .strip_prefix('|')
        .unwrap_or(trimmed)
        .strip_suffix('|')
        .unwrap_or_else(|| trimmed.strip_prefix('|').unwrap_or(trimmed));
    inner
        .split('|')
        .map(|c| c.trim().trim_matches('`').trim().to_string())
        .collect()
}

/// Return the entity token in `cell` if it matches a configured entity
/// pattern and is not excluded (mirrors the per-sentence entity filter).
fn matched_entity_token(cell: &str, cfg: &ExtractorConfig) -> Option<String> {
    let token = cell.trim();
    if token.is_empty() {
        return None;
    }
    for pat in &cfg.entity_patterns {
        if let Some(m) = pat.find(token) {
            // Require the match to span the whole cell (a result-table
            // entity cell is the identifier itself, not prose).
            if m.start() == 0 && m.end() == token.len() {
                let excluded = cfg
                    .entity_exclude_patterns
                    .iter()
                    .any(|excl| excl.is_match(token));
                if !excluded {
                    return Some(token.to_string());
                }
            }
        }
    }
    None
}

/// Parse a leading number from a markdown cell ("1.49e-159", "-4.606",
/// "2,345"). Tolerates a trailing unit / annotation.
fn parse_leading_number(cell: &str) -> Option<f64> {
    let c = cell.trim().replace(',', "");
    static NUM_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^[+-]?\d*\.?\d+(?:[eE][+-]?\d+)?").expect("static regex"));
    NUM_RE.find(&c).and_then(|m| m.as_str().parse::<f64>().ok())
}

#[derive(Debug, Clone)]
struct EntityHit {
    start: usize,
    end: usize,
    token: String,
}

fn table_reference_spans(sentence: &str) -> Vec<(usize, usize)> {
    TABLE_REF_RE
        .find_iter(sentence)
        .map(|m| (m.start(), m.end()))
        .collect()
}

fn is_embedded_in_alnum_token(sentence: &str, start: usize, end: usize) -> bool {
    let prev_blocks = sentence[..start]
        .chars()
        .next_back()
        .is_some_and(is_alnum_token_char);
    let next_blocks = sentence[end..]
        .chars()
        .next()
        .is_some_and(is_alnum_token_char);
    prev_blocks || next_blocks
}

fn is_alnum_token_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_'
}

fn select_longest_non_overlapping_entity_hits(mut hits: Vec<EntityHit>) -> Vec<(usize, String)> {
    hits.sort_by(|a, b| {
        let a_len = a.end - a.start;
        let b_len = b.end - b.start;
        b_len
            .cmp(&a_len)
            .then_with(|| a.start.cmp(&b.start))
            .then_with(|| a.token.cmp(&b.token))
    });

    let mut selected: Vec<EntityHit> = Vec::new();
    for hit in hits {
        if selected
            .iter()
            .any(|existing| spans_overlap(hit.start, hit.end, existing.start, existing.end))
        {
            continue;
        }
        selected.push(hit);
    }
    selected.sort_by_key(|hit| hit.start);
    selected
        .into_iter()
        .map(|hit| (hit.start, hit.token))
        .collect()
}

fn spans_overlap(a_start: usize, a_end: usize, b_start: usize, b_end: usize) -> bool {
    a_start < b_end && b_start < a_end
}

fn nearest_direction(
    entity_pos: usize,
    direction_hits: &[(usize, Direction)],
) -> Option<Direction> {
    direction_hits
        .iter()
        .min_by_key(|(pos, _)| pos.abs_diff(entity_pos))
        .map(|(_, d)| *d)
}

/// Bind a numeric value to one entity in a sentence.
///
/// Reporting prose writes the number *after* the entity it describes
/// ("ACAN was upregulated (log2FC=2.1) and COL2A1 was downregulated
/// (log2FC=-1.5)"), so a value belongs to the entity that most recently
/// precedes it. Rules, given `value_hits` sorted ascending by position:
///
/// * **No values** → `None`.
/// * **Exactly one value** → that value, for every entity. Preserves the
///   prior aggregate behavior for a single shared number
///   ("A and B were both up (log2FC=2.0)").
/// * **Multiple values** → the first value at or after `entity_pos` (the
///   number written next to this entity); if the entity follows every
///   value, the last value. This stops the sentence's first number being
///   force-attributed onto every entity, which surfaced correct
///   multi-entity narratives as false mismatches and wrongly blocked
///   the session.
fn value_for_entity(entity_pos: usize, value_hits: &[(usize, f64)]) -> Option<f64> {
    match value_hits.len() {
        0 => None,
        1 => Some(value_hits[0].1),
        _ => value_hits
            .iter()
            .find(|(pos, _)| *pos >= entity_pos)
            .or_else(|| value_hits.last())
            .map(|(_, v)| *v),
    }
}

/// Like [`value_for_entity`], but for p-value hits that carry their matched
/// keyword. Returns the value AND the keyword of the SAME bound hit, so the
/// recorded `matched_pvalue_keyword` always describes the value actually
/// attributed to this entity (VF-8). The binding rule is identical to
/// `value_for_entity` (single value → shared; multiple → first at/after the
/// entity, else the last), guaranteeing the value the verifier compares and
/// the keyword it classifies come from one and the same prose mention.
fn pvalue_for_entity(
    entity_pos: usize,
    value_hits: &[(usize, f64, String)],
) -> (Option<f64>, Option<String>) {
    let chosen = match value_hits.len() {
        0 => None,
        1 => value_hits.first(),
        _ => value_hits
            .iter()
            .find(|(pos, _, _)| *pos >= entity_pos)
            .or_else(|| value_hits.last()),
    };
    match chosen {
        Some((_, v, kw)) => (Some(*v), Some(kw.clone())),
        None => (None, None),
    }
}

/// Every effect-size match in the sentence as `(keyword_anchor_pos, value)`,
/// sorted by position. Keyword priority (configured columns first, then the
/// baked-in `log2fc`/`logfc` defaults) breaks ties so a number matched by two
/// keyword regexes at the same offset is recorded once. Returns every
/// occurrence so the caller can bind each entity to its nearest number; the
/// previous single-value scanner forced the first match onto the whole
/// sentence. The regex set is prebuilt by `ExtractorRegexCache::build` so the
/// hot loop does N capture-scans instead of N compile-and-scans.
fn scan_effect_size_positions(sentence: &str, cache: &ExtractorRegexCache) -> Vec<(usize, f64)> {
    let mut hits: Vec<(usize, f64)> = Vec::new();
    for (_kw, re) in &cache.effect_size {
        for caps in re.captures_iter(sentence) {
            let Some(whole) = caps.get(0) else { continue };
            if let Some(m) = caps.get(1) {
                if let Ok(v) = m.as_str().parse::<f64>() {
                    let pos = whole.start();
                    if !hits.iter().any(|(p, _)| *p == pos) {
                        hits.push((pos, v));
                    }
                }
            }
        }
    }
    hits.sort_by_key(|(p, _)| *p);
    hits
}

/// Every p-value match in the sentence as `(keyword_anchor_pos, value,
/// matched_keyword)`, sorted by position. Same per-entity-nearest rationale as
/// [`scan_effect_size_positions`]. The matched keyword (the literal p-value
/// term the prose used — "padj"/"fdr"/"pvalue"/"p"/…) is carried so the
/// verifier can tell which column CLASS the narrative attributed the value to
/// (VF-8 p-laundering). The set of positions/values is unchanged from the
/// prior 2-tuple form; only the keyword string is added, so extraction
/// behaviour (and the faithful-claim path) is untouched.
fn scan_pvalue_positions(sentence: &str, cache: &ExtractorRegexCache) -> Vec<(usize, f64, String)> {
    let mut hits: Vec<(usize, f64, String)> = Vec::new();
    for (kw, re) in &cache.pvalue {
        for caps in re.captures_iter(sentence) {
            let Some(whole) = caps.get(0) else { continue };
            if let Some(m) = caps.get(1) {
                if let Ok(v) = m.as_str().parse::<f64>() {
                    let pos = whole.start();
                    if !hits.iter().any(|(p, _, _)| *p == pos) {
                        hits.push((pos, v, kw.clone()));
                    }
                }
            }
        }
    }
    hits.sort_by_key(|(p, _, _)| *p);
    hits
}

fn scan_table_reference(sentence: &str) -> Option<String> {
    TABLE_REF_RE.find(sentence).map(|m| m.as_str().to_string())
}

/// Static regex collapsing `<num> × 10<exp>` (after Unicode→ASCII mapping)
/// into `<num>e<exp>` so the numeric scanners see standard exponent form.
static SCI_NOTATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d(?:\.\d+)?)\s*\*\s*10\s*\^?\s*(-?\d+)").expect("static regex")
});

/// Canonicalize the Unicode scientific-notation polish that real reports
/// use ("1.49 × 10⁻¹⁵⁹", "log2FC = −4.61") into ASCII the regex scanners
/// understand ("1.49e-159", "log2FC = -4.61"). Without this, a *correct*
/// claim like `padj = 1.49 × 10⁻¹⁵⁹` parses as `1.49`, producing a false
/// mismatch that would wrongly block the session. Applied before any
/// offset computation so entity/direction positions stay self-consistent.
pub(crate) fn canonicalize_scientific(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    for ch in text.chars() {
        let mapped = match ch {
            // Unicode minus / figure-en-em dashes / horizontal bar → '-'.
            '\u{2212}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}' => '-',
            // Superscript digits → ASCII digits.
            '\u{2070}' => '0',
            '\u{00B9}' => '1',
            '\u{00B2}' => '2',
            '\u{00B3}' => '3',
            '\u{2074}' => '4',
            '\u{2075}' => '5',
            '\u{2076}' => '6',
            '\u{2077}' => '7',
            '\u{2078}' => '8',
            '\u{2079}' => '9',
            // Superscript minus → '-'.
            '\u{207B}' => '-',
            // Multiplication sign / middle dot → '*' marker for the
            // scientific-notation collapse below.
            '\u{00D7}' | '\u{22C5}' | '\u{00B7}' => '*',
            other => {
                s.push(other);
                continue;
            }
        };
        s.push(mapped);
    }
    SCI_NOTATION_RE.replace_all(&s, "${1}e${2}").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_policy_file_prefers_downstream_policy_then_flat() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        // Neither location → None.
        assert!(resolve_policy_file(dir, "interpretation-policy.json").is_none());
        // Flat only → flat path.
        let flat = dir.join("interpretation-policy.json");
        std::fs::write(&flat, "{}").unwrap();
        assert_eq!(
            resolve_policy_file(dir, "interpretation-policy.json"),
            Some(flat.clone())
        );
        // Both present → downstream-policy/ wins (server precedence unchanged).
        let nested = dir
            .join("downstream-policy")
            .join("interpretation-policy.json");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "{}").unwrap();
        assert_eq!(
            resolve_policy_file(dir, "interpretation-policy.json"),
            Some(nested)
        );
    }

    fn policy_json() -> Value {
        json!({
            "verifiableEntities": {
                "enabled": true,
                "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}"],
                "directionVocab": {
                    "up": ["upregulated", "increased", "elevated"],
                    "down": ["downregulated", "decreased", "reduced"]
                },
                "effectSizeColumns": ["log2FC", "logFC"],
                "entityColumns": ["gene", "symbol"],
                "pvalueColumns": ["pvalue", "padj"]
            }
        })
    }

    #[test]
    fn merge_overlay_replaces_scalar_and_array_fields() {
        // Overlay arrays replace wholesale (they must not append to
        // bio's defaults).
        let base = json!({
            "verifiableEntities": {
                "enabled": true,
                "effectSizeColumns": ["log2FC"],
                "directionVocab": {
                    "up": ["upregulated"],
                    "down": ["downregulated"]
                }
            }
        });
        let overlay = json!({
            "verifiableEntities": {
                "effectSizeColumns": ["hazard_ratio", "odds_ratio"],
                "directionVocab": {
                    "up": ["improved", "superior"]
                }
            }
        });
        let merged = merge_overlay(base, &overlay);
        let cols = merged
            .pointer("/verifiableEntities/effectSizeColumns")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].as_str().unwrap(), "hazard_ratio");
        let up = merged
            .pointer("/verifiableEntities/directionVocab/up")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(up.len(), 2);
        // The overlay didn't specify `down` → base value carries through.
        let down = merged
            .pointer("/verifiableEntities/directionVocab/down")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(down.len(), 1);
        assert_eq!(down[0].as_str().unwrap(), "downregulated");
        // `enabled: true` survives the merge.
        assert!(merged
            .pointer("/verifiableEntities/enabled")
            .unwrap()
            .as_bool()
            .unwrap());
    }

    #[test]
    fn from_policy_for_class_loads_clinical_trial_overlay() {
        // The real overlay file lives at config/downstream-policy/
        // interpretation-policy.clinical_trial.json. Resolve it from
        // the crate manifest root.
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
        .expect("clinical_trial overlay merges cleanly");
        assert!(
            cfg.effect_size_columns
                .iter()
                .any(|c| c.eq_ignore_ascii_case("hazard_ratio")),
            "overlay must contribute hazard_ratio to effect-size columns: {:?}",
            cfg.effect_size_columns
        );
        assert!(
            cfg.up_words.iter().any(|w| w == "improved"),
            "overlay must contribute clinical direction words: {:?}",
            cfg.up_words
        );
        // Overlay replaces bio's gene-symbol entity patterns entirely.
        assert!(
            !cfg.entity_patterns
                .iter()
                .any(|p| p.as_str().contains("[A-Z][A-Z0-9]")),
            "clinical overlay should not keep bio's gene-symbol pattern"
        );
    }

    #[test]
    fn from_policy_for_class_bio_is_identity() {
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
            crate::project_class::ProjectClass::Bioinformatics,
        )
        .expect("bio class does not require an overlay");
        // There's no interpretation-policy.bioinformatics.json, so the
        // base policy carries through.
        assert_eq!(cfg.effect_size_columns, vec!["log2FC", "logFC"]);
    }

    #[test]
    fn extracts_simple_entity_direction_claim() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let text = "ACAN was upregulated in NP cells (log2FC=2.1, padj=0.001, Table S1).";
        let claims = extract_claims(text, &cfg);
        // ACAN + NP are both caught by the [A-Z][A-Z0-9]+ pattern. NP
        // shares the sentence with a direction word, so both get it.
        let acan = claims.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(acan.direction, Some(Direction::Up));
        assert!(
            (acan.effect_size.unwrap() - 2.1).abs() < 1e-9,
            "got {:?}",
            acan.effect_size
        );
        assert!((acan.pvalue.unwrap() - 0.001).abs() < 1e-9);
        assert!(acan.source_table.as_deref().unwrap().starts_with("Table"));
    }

    #[test]
    fn vf3_prose_format_log2fc_magnitude_parses() {
        // VF-3 — "log2FC of 3.5" and spaced "log2 fold change of -4.2" must
        // populate effect_size (the old `[:=]`-only regex left them None, so a
        // fabricated magnitude rode through on a true direction).
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let a = extract_claims("STAT1 was upregulated, log2FC of 3.5 (Table S1).", &cfg);
        let stat1 = a.iter().find(|c| c.entity == "STAT1").unwrap();
        assert!(
            stat1.effect_size.map(|e| (e - 3.5).abs() < 1e-9).unwrap_or(false),
            "prose 'log2FC of 3.5' must parse, got {:?}",
            stat1.effect_size
        );
        let b = extract_claims(
            "IFIT1 showed a marked log2 fold change of -4.2 (Table S1).",
            &cfg,
        );
        let ifit1 = b.iter().find(|c| c.entity == "IFIT1").unwrap();
        assert!(
            ifit1.effect_size.map(|e| (e + 4.2).abs() < 1e-9).unwrap_or(false),
            "prose 'log2 fold change of -4.2' must parse, got {:?}",
            ifit1.effect_size
        );
    }

    #[test]
    fn vf3_cutoff_phrasing_does_not_capture_a_value() {
        // FP guard: "log2 fold change of at least 1" is a CUTOFF, not a
        // per-gene effect — the `of\s+<digit>` form must not capture "1".
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let claims = extract_claims(
            "ACAN passed the log2 fold change of at least 1 cutoff (Table S1).",
            &cfg,
        );
        let acan = claims.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(
            acan.effect_size, None,
            "cutoff phrasing must not capture a value, got {:?}",
            acan.effect_size
        );
    }

    #[test]
    fn downregulated_direction_is_captured() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let text = "COL2A1 showed decreased expression (log2FC=-1.5, padj=0.003).";
        let claims = extract_claims(text, &cfg);
        let col = claims.iter().find(|c| c.entity == "COL2A1").unwrap();
        assert_eq!(col.direction, Some(Direction::Down));
        assert!((col.effect_size.unwrap() + 1.5).abs() < 1e-9);
    }

    #[test]
    fn entity_without_direction_still_records() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let text = "TNF is listed among the top hits (see Table 2).";
        let claims = extract_claims(text, &cfg);
        let tnf = claims.iter().find(|c| c.entity == "TNF").unwrap();
        assert_eq!(tnf.direction, None);
        assert!(tnf
            .source_table
            .as_deref()
            .unwrap()
            .to_lowercase()
            .contains("table"));
    }

    #[test]
    fn multiple_sentences_yield_separate_claims() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let text = "ACAN was upregulated (log2FC=2.1). COL2A1 was reduced (log2FC=-1.5).";
        let claims = extract_claims(text, &cfg);
        assert!(claims.iter().any(|c| c.entity == "ACAN"
            && c.direction == Some(Direction::Up)
            && (c.effect_size.unwrap() - 2.1).abs() < 1e-9));
        assert!(claims.iter().any(|c| c.entity == "COL2A1"
            && c.direction == Some(Direction::Down)
            && (c.effect_size.unwrap() + 1.5).abs() < 1e-9));
    }

    #[test]
    fn multiple_entities_one_sentence_bind_nearest_numbers() {
        // Two entities + two effect sizes + two p-values in a single
        // sentence must each bind to their *nearest* number, not have the
        // first number force-attributed onto every entity (which would
        // surface a correct narrative as a false mismatch and wrongly
        // block the session).
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let text = "ACAN was upregulated (log2FC=2.1, padj=0.001) and COL2A1 \
                    was downregulated (log2FC=-1.5, padj=0.04).";
        let claims = extract_claims(text, &cfg);
        let acan = claims.iter().find(|c| c.entity == "ACAN").unwrap();
        let col = claims.iter().find(|c| c.entity == "COL2A1").unwrap();
        assert!((acan.effect_size.unwrap() - 2.1).abs() < 1e-9, "{:?}", acan);
        assert!((acan.pvalue.unwrap() - 0.001).abs() < 1e-9, "{:?}", acan);
        assert!((col.effect_size.unwrap() + 1.5).abs() < 1e-9, "{:?}", col);
        assert!((col.pvalue.unwrap() - 0.04).abs() < 1e-9, "{:?}", col);
    }

    #[test]
    fn canonicalize_unicode_scientific_notation() {
        // Unicode minus, superscript exponent, × 10ⁿ → ASCII e-notation.
        let s = canonicalize_scientific("padj = 1.49 × 10⁻¹⁵⁹, log2FC = −4.61");
        assert!(s.contains("1.49e-159"), "got {s}");
        assert!(s.contains("-4.61"), "got {s}");
    }

    #[test]
    fn markdown_de_table_yields_per_gene_claims() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let md = "| Gene | log2FC | padj |\n|---|---|---|\n\
                  | ACAN | -4.606 | 1.49e-159 |\n| COL2A1 | 2.889 | 7.48e-110 |\n";
        let claims = extract_markdown_table_claims(md, &cfg);
        let acan = claims.iter().find(|c| c.entity == "ACAN").unwrap();
        assert!(
            (acan.effect_size.unwrap() + 4.606).abs() < 1e-6,
            "{:?}",
            acan
        );
        assert!(
            (acan.pvalue.unwrap() - 1.49e-159).abs() < 1e-165,
            "{:?}",
            acan
        );
        assert_eq!(acan.direction, Some(Direction::Down));
    }

    #[test]
    fn markdown_summary_table_yields_no_false_claims() {
        // A cluster/domain summary ("N sig (FDR<0.05)", "Top up-gene")
        // must NOT be mis-read as per-gene DE rows — strict cleaned-header
        // matching rejects count/annotation columns.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let md = "| Cluster | Label | N spots | N sig (FDR<0.05) | Top up-gene |\n\
                  |---|---|---|---|---|\n\
                  | 0 | Domain_0 | 13 | 1 | ACAN |\n";
        let claims = extract_markdown_table_claims(md, &cfg);
        assert!(
            claims.is_empty(),
            "summary table must yield no claims, got {:?}",
            claims.iter().map(|c| &c.entity).collect::<Vec<_>>()
        );
    }

    #[test]
    fn single_number_still_attaches_to_all_entities() {
        // Regression guard for the nearest-number change: when a sentence
        // carries exactly one effect size, every entity in it still binds
        // to that one value (the prior aggregate behavior).
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let text = "ACAN and COL2A1 were both upregulated (log2FC=2.0).";
        let claims = extract_claims(text, &cfg);
        for ent in ["ACAN", "COL2A1"] {
            let c = claims.iter().find(|c| c.entity == ent).unwrap();
            assert!(
                (c.effect_size.unwrap() - 2.0).abs() < 1e-9,
                "{}: {:?}",
                ent,
                c
            );
        }
    }

    #[test]
    fn disabled_policy_rejects_config() {
        let disabled = json!({ "verifiableEntities": { "enabled": false } });
        let err = ExtractorConfig::from_policy(&disabled).unwrap_err();
        assert!(err.to_string().contains("enabled"), "{}", err);
    }

    #[test]
    fn same_entity_twice_in_one_sentence_dedupes() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let text = "ACAN and ACAN-positive cells were upregulated.";
        let claims = extract_claims(text, &cfg);
        let acan_hits = claims.iter().filter(|c| c.entity == "ACAN").count();
        assert_eq!(acan_hits, 1);
    }

    #[test]
    fn scientific_notation_pvalue_parses() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let text = "TNF was elevated (log2FC=3.0, padj=1.2e-7).";
        let claims = extract_claims(text, &cfg);
        let tnf = claims.iter().find(|c| c.entity == "TNF").unwrap();
        assert!(
            (tnf.pvalue.unwrap() - 1.2e-7).abs() < 1e-12,
            "got {:?}",
            tnf.pvalue
        );
    }

    #[test]
    fn metric_suffixes_and_table_labels_are_not_entities() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let text = [
            "ACAN was upregulated (log2FC=2.1, padj=0.001, Table S1).",
            "COL2A1 was downregulated (log2FC=-2.0, padj=0.001, Table S1).",
            "TNF was upregulated (log2FC=0.5, padj=0.9, Table S1).",
        ]
        .join(" ");
        let claims = extract_claims(&text, &cfg);
        let entities = claims.iter().map(|c| c.entity.as_str()).collect::<Vec<_>>();
        assert_eq!(entities, vec!["ACAN", "COL2A1", "TNF"]);
    }

    #[test]
    fn overlapping_identifier_matches_prefer_longest_span() {
        let policy = json!({
            "verifiableEntities": {
                "enabled": true,
                "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}", "GO:\\d{7}"],
                "directionVocab": {
                    "up": ["enriched"],
                    "down": ["depleted"]
                },
                "effectSizeColumns": ["score"],
                "entityColumns": ["term"],
                "pvalueColumns": ["padj"]
            }
        });
        let cfg = ExtractorConfig::from_policy(&policy).unwrap();
        let claims = extract_claims("GO:0008150 was enriched (score=2.0, Table S1).", &cfg);
        let entities = claims.iter().map(|c| c.entity.as_str()).collect::<Vec<_>>();
        assert_eq!(entities, vec!["GO:0008150"]);
    }

    #[test]
    fn classify_hedged_near_significance_not_thresholded() {
        // "near the significance threshold" is a proximity HEDGE (the result is
        // explicitly NOT significant), not an assertion that it passed FDR, so
        // it must NOT be classified as a thresholded claim (which would enforce
        // FDR < 0.05 against the table p-value). Regression for the tier-4-1
        // clean-scenario false positive: HMOX1 (log2FC +0.3, p 0.54) flagged as
        // a fabrication purely because the sentence named the threshold.
        assert_eq!(
            classify_contract(
                "HMOX1 was modestly upregulated near the significance threshold (Table 2)"
            ),
            ClaimContract::NumericTableLookup,
        );
    }

    #[test]
    fn classify_explicit_fdr_comparator_stays_thresholded_when_hedged() {
        // A real, checkable significance assertion with an explicit comparator
        // stays thresholded even when hedging words are present.
        assert_eq!(
            classify_contract("GENE1 was marginally significant at FDR < 0.05 (Table 1)"),
            ClaimContract::ThresholdedDeOrEnrichment,
        );
    }

    #[test]
    fn classify_unhedged_significance_assertion_stays_thresholded() {
        // No proximity hedge → an assertion that the gene meets the threshold
        // remains a thresholded claim.
        assert_eq!(
            classify_contract("GENE1 passed the significance threshold (Table 1)"),
            ClaimContract::ThresholdedDeOrEnrichment,
        );
    }

    #[test]
    fn classify_pmid_citation_is_literature_grounded() {
        // A PMID citation token marks the sentence as a literature-support
        // claim, verified against the PMID-anchored evidence matrix.
        assert_eq!(
            classify_contract(
                "TP53 dysregulation is concordant with prior reports (PMID 12345678)"
            ),
            ClaimContract::LiteratureGrounded,
        );
        assert_eq!(
            classify_contract("This matches earlier work (PMID: 23456789)"),
            ClaimContract::LiteratureGrounded,
        );
    }

    #[test]
    fn classify_prior_literature_prose_is_literature_grounded() {
        // Prior-literature / previous-finding / concordance prose, with no
        // PMID, still routes to the literature-grounded contract.
        for sentence in [
            "TP53 is consistent with prior work in this disease",
            "ACAN expression is concordant with previous findings",
            "This result is supported by the literature",
            "As previously reported, BRCA1 is downregulated here",
        ] {
            assert_eq!(
                classify_contract(sentence),
                ClaimContract::LiteratureGrounded,
                "sentence: {sentence}",
            );
        }
    }

    #[test]
    fn classify_explicit_threshold_wins_over_literature_cue() {
        // An explicit checkable threshold assertion stays thresholded even
        // when literature-grounding prose is present — the numeric assertion
        // is verifiable against the table.
        assert_eq!(
            classify_contract("GENE1 passed FDR < 0.05, consistent with prior work"),
            ClaimContract::ThresholdedDeOrEnrichment,
        );
    }

    #[test]
    fn literature_evidence_round_trips_and_is_omitted_when_none() {
        // Present: serializes nested, deserializes back.
        let claim = Claim {
            entity: "TP53".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: None,
            excerpt: "TP53 concordant (PMID 12345678)".into(),
            contract: ClaimContract::LiteratureGrounded,
            literature_evidence: Some(LiteratureEvidence {
                finding_id: "finding_42".into(),
                cited_pmids: vec![12345678, 23456789],
            }),
            matched_pvalue_keyword: None,
            linear_fold: None,
        };
        let json = serde_json::to_string(&claim).unwrap();
        assert!(json.contains("\"literature_evidence\""), "{json}");
        assert!(json.contains("\"finding_id\":\"finding_42\""), "{json}");
        let back: Claim = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.literature_evidence.as_ref().unwrap().cited_pmids,
            vec![12345678, 23456789]
        );

        // Absent: field is skipped when None.
        let bare = Claim {
            entity: "X".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: None,
            excerpt: "X".into(),
            contract: ClaimContract::NumericTableLookup,
            literature_evidence: None,
            matched_pvalue_keyword: None,
            linear_fold: None,
        };
        let bare_json = serde_json::to_string(&bare).unwrap();
        assert!(!bare_json.contains("literature_evidence"), "{bare_json}");
    }

    #[test]
    fn old_claim_json_without_literature_evidence_defaults_none() {
        let old = r#"{"entity":"ACAN","excerpt":"ACAN was upregulated"}"#;
        let claim: Claim = serde_json::from_str(old).unwrap();
        assert!(claim.literature_evidence.is_none());
    }

    #[test]
    fn literature_thresholds_default_when_absent() {
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        assert_eq!(cfg.literature_min_papers, 2);
        assert_eq!(cfg.literature_min_sources, 1);
    }

    #[test]
    fn literature_thresholds_parse_from_policy() {
        let mut p = policy_json();
        p["verifiableEntities"]["literatureGrounding"] = json!({
            "minPapers": 3,
            "minSources": 2
        });
        let cfg = ExtractorConfig::from_policy(&p).unwrap();
        assert_eq!(cfg.literature_min_papers, 3);
        assert_eq!(cfg.literature_min_sources, 2);
    }

    #[test]
    fn report_control_token_denylisted_but_bare_gene_mention_kept() {
        // Report-control noise ("GATING") is removed by the policy
        // `entityNameExcludePatterns` deny-list — the precise mechanism, not a
        // slot heuristic. A real gene mentioned in prose with NO inline number
        // ("ACTB") must still be KEPT: the discovery verifier resolves it
        // against the result tables by entity membership, so dropping bare
        // mentions would reduce verifiable-claim recall. CRISPLD2, which also
        // carries an effect size, survives with that slot populated.
        let mut p = policy_json();
        p["verifiableEntities"]["entityNameExcludePatterns"] = json!(["^GATING$"]);
        let cfg = ExtractorConfig::from_policy(&p).unwrap();
        let claims = extract_claims(
            "The GATING step ran. ACTB is a housekeeping gene. \
             CRISPLD2 was upregulated (log2FC=2.6, Table S1).",
            &cfg,
        );
        let entities: Vec<&str> = claims.iter().map(|c| c.entity.as_str()).collect();
        assert!(
            !entities.contains(&"GATING"),
            "deny-listed report token must be excluded: {entities:?}"
        );
        assert!(
            entities.contains(&"ACTB"),
            "a real gene mentioned with no inline number must still be extracted \
             (discovery verifies it): {entities:?}"
        );
        assert!(
            claims
                .iter()
                .any(|c| c.entity == "CRISPLD2" && c.effect_size.is_some()),
            "CRISPLD2 with an effect size must survive: {:?}",
            claims
        );
    }

    #[test]
    fn enrichment_score_acronym_nes_not_mistaken_for_nestin_gene() {
        // "NES = 1.92" in a gene-set enrichment narrative is a Normalized
        // Enrichment Score, NOT the Nestin gene (HGNC symbol NES). Without the
        // deny-list the broad gene-symbol regex captures "NES" as an entity and
        // the discovery verifier binds it to the Nestin row in an unrelated DE
        // table (log2FC -0.78), emitting a guaranteed false Mismatch. The
        // `^NES$` exclude pattern (sibling of the existing `^ES$`) drops it; the
        // real entity — the gene-set name — is not a gene symbol and is verified
        // structurally elsewhere. A genuine gene in the same prose still survives.
        let mut p = policy_json();
        p["verifiableEntities"]["entityNameExcludePatterns"] = json!(["^NES$", "^FWER$"]);
        let cfg = ExtractorConfig::from_policy(&p).unwrap();
        let claims = extract_claims(
            "Top enriched terms (by NES) include Adipogenesis (NES = 1.92, FDR = 0.001). \
             CRISPLD2 was upregulated (log2FC=2.6, Table S1).",
            &cfg,
        );
        let entities: Vec<&str> = claims.iter().map(|c| c.entity.as_str()).collect();
        assert!(
            !entities.contains(&"NES"),
            "enrichment-score acronym NES must be deny-listed: {entities:?}"
        );
        assert!(
            claims
                .iter()
                .any(|c| c.entity == "CRISPLD2" && c.effect_size.is_some()),
            "a real gene with an effect size must still survive: {entities:?}"
        );
    }

    #[test]
    fn delimited_tsv_yields_per_row_claims() {
        // A bare TSV result table (no markdown pipes) must produce one
        // NumericTableLookup claim per row that has a recognized entity and a
        // numeric slot.
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let tsv = "gene\tlog2FC\tpadj\nACAN\t-4.606\t1.49e-159\nCOL2A1\t2.889\t7.48e-110\n";
        let claims = extract_delimited_table_claims(tsv.as_bytes(), b'\t', &cfg);
        let acan = claims.iter().find(|c| c.entity == "ACAN").unwrap();
        assert!(
            (acan.effect_size.unwrap() + 4.606).abs() < 1e-6,
            "{:?}",
            acan
        );
        assert!(
            (acan.pvalue.unwrap() - 1.49e-159).abs() < 1e-165,
            "{:?}",
            acan
        );
        assert_eq!(acan.direction, Some(Direction::Down));
        assert_eq!(acan.contract, ClaimContract::NumericTableLookup);
        assert!(claims.iter().any(|c| c.entity == "COL2A1"));
    }

    #[test]
    fn delimited_csv_drops_rows_with_no_numeric_slot() {
        // A CSV with an entity column but no recognized numeric column must
        // emit nothing (the C2 all-None guard).
        let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
        let csv = "gene,note\nACAN,present\nCOL2A1,absent\n";
        let claims = extract_delimited_table_claims(csv.as_bytes(), b',', &cfg);
        assert!(
            claims.is_empty(),
            "rows with no numeric slot must yield no claims: {:?}",
            claims.iter().map(|c| &c.entity).collect::<Vec<_>>()
        );
    }
}
