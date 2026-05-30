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
        for extra in ["log2fc", "logfc"] {
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
                let pat = format!(
                    r"(?i){}\s*[:=]\s*(-?\d+(?:\.\d+)?(?:[eE]-?\d+)?)",
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
}

impl ExtractorConfig {
    /// Pick the `verifiableEntities` block that matches the session's
    /// `ProjectClass`. The overlay file
    /// (`interpretation-policy.<class>.json`, when present) wins on
    /// every field it specifies; fields absent from the overlay fall
    /// through to the base `interpretation-policy.json`.
    ///
    /// `policy_dir` is typically `config/downstream-policy/`. When no
    /// overlay exists for the class, or the class is `Bioinformatics`,
    /// the base policy is used unchanged.
    pub fn from_policy_for_class(
        base_policy: &Value,
        policy_dir: &std::path::Path,
        class: crate::project_class::ProjectClass,
    ) -> Result<Self> {
        let overlay_name = format!("interpretation-policy.{}.json", class.as_str());
        let overlay_path = policy_dir.join(&overlay_name);
        let merged = if overlay_path.exists() {
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
/// 2. `RankTopN` — "top N" / "rank" constructs.
/// 3. `GroupComparison` — directional group comparisons ("vs", "higher than", "lower than").
/// 4. `Categorical` — cluster / label / category assignments.
/// 5. `TimeSeriesSummary` — day / week / month / enrolled / timepoint patterns.
/// 6. `NumericTableLookup` — fallback.
pub fn classify_contract(sentence: &str) -> ClaimContract {
    let lower = sentence.to_lowercase();

    // Thresholded DE / enrichment: presence of a threshold keyword.
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
    if threshold_keywords.iter().any(|kw| lower.contains(kw)) {
        return ClaimContract::ThresholdedDeOrEnrichment;
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
        let mut s = text.to_string();
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

        let mut seen: std::collections::BTreeSet<(String, Option<Direction>)> =
            std::collections::BTreeSet::new();
        for (ent_pos, ent_name) in entity_hits {
            let direction = nearest_direction(ent_pos, &direction_hits);
            let effect_size = value_for_entity(ent_pos, &effect_size_hits);
            let pvalue = value_for_entity(ent_pos, &pvalue_hits);
            let key = (ent_name.clone(), direction);
            if seen.contains(&key) {
                continue;
            }
            seen.insert(key);
            out.push(Claim {
                entity: ent_name,
                direction,
                effect_size,
                pvalue,
                source_table: source_table.clone(),
                excerpt: trimmed.to_string(),
                contract,
            });
        }
    }

    out
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

/// Every p-value match in the sentence as `(keyword_anchor_pos, value)`,
/// sorted by position. Same per-entity-nearest rationale as
/// [`scan_effect_size_positions`].
fn scan_pvalue_positions(sentence: &str, cache: &ExtractorRegexCache) -> Vec<(usize, f64)> {
    let mut hits: Vec<(usize, f64)> = Vec::new();
    for (_kw, re) in &cache.pvalue {
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

fn scan_table_reference(sentence: &str) -> Option<String> {
    TABLE_REF_RE.find(sentence).map(|m| m.as_str().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        let policy_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/downstream-policy");
        let cfg = ExtractorConfig::from_policy_for_class(
            &base,
            &policy_dir,
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
        let policy_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/downstream-policy");
        let cfg = ExtractorConfig::from_policy_for_class(
            &base,
            &policy_dir,
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
}
