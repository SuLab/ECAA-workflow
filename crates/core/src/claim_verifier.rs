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
use std::collections::BTreeMap;
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
/// Distinguishes Ensembl-family stable ids (`ENSG…`, `ENSMUSG…`, `ENST…`) from
/// everything else (gene symbols, etc.). The guard only flags an absent entity
/// Suspicious when its class MATCHES the cited table's entity-column class — so
/// a symbol claim looked up in an Ensembl-keyed table (a benign cross-namespace
/// miss) stays Unverifiable rather than being wrongly flagged.
fn id_namespace(token: &str) -> &'static str {
    let t = token.trim();
    let upper = t.to_ascii_uppercase();
    if upper.starts_with("ENS") {
        // ENS + optional species (up to 4 letters) + G/T/P + ≥6 digits.
        let rest = &upper[3..];
        let letters: String = rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
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

/// True when the claim entity's id-namespace matches the cited table's
/// entity-column namespace (sampled from the first row). Used by VF-0 so an
/// absent entity is only flagged Suspicious when its absence is a real
/// negative in the SAME namespace, not a symbol-vs-Ensembl lookup artifact.
fn namespace_matches_table(claim_entity: &str, cached: &CachedTable) -> bool {
    match cached.rows.first() {
        Some(first) => id_namespace(claim_entity) == id_namespace(&first.entity),
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
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
    /// N unverifiable.
    pub n_unverifiable: usize,
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

    for claim in claims {
        let status = verify_for_contract(claim, &index, cfg, &mut cache);
        report.push(ClaimVerdict {
            claim: claim.clone(),
            status,
            strength: ClaimStrength::Exploratory,
        });
    }
    report
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
fn verify_for_contract(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    match claim.contract {
        ClaimContract::NumericTableLookup => verify_numeric_lookup(claim, index, cfg, cache),
        ClaimContract::ThresholdedDeOrEnrichment => verify_thresholded(claim, index, cfg, cache),
        ClaimContract::RankTopN => verify_rank_top_n(claim, index, cfg, cache),
        ClaimContract::GroupComparison => verify_group_comparison(claim, index, cfg, cache),
        ClaimContract::Categorical => verify_categorical(claim, index, cfg, cache),
        ClaimContract::TimeSeriesSummary => verify_time_series(claim, index, cfg, cache),
        ClaimContract::LiteratureGrounded => verify_literature_grounded(claim, index, cfg, cache),
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
                if let Some(row) = cached
                    .rows
                    .iter()
                    .find(|r| r.entity.eq_ignore_ascii_case(&claim.entity))
                {
                    // VF-7 — judge "significant at FDR/padj < 0.05" on the
                    // ADJUSTED column, not the raw `pvalue` that `lookup_numeric`
                    // (column order) returns first. A gene with raw p=0.042 but
                    // padj=0.16 is NOT FDR-significant; the raw-first probe
                    // silently passed that overclaim. Prefer the first adjusted
                    // p-column present; fall back to a raw column only when the
                    // table carries no adjusted column at all.
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
                    let obs_p = lookup_numeric(&row.values, &adjusted)
                        .or_else(|| lookup_numeric(&row.values, &raw));
                    if let Some(obs_p) = obs_p {
                        if obs_p >= 0.05 {
                            return ClaimStatus::Mismatch {
                                detail: format!(
                                    "thresholded claim: observed adjusted p-value {:.4e} does not meet FDR < 0.05",
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
/// Checks whether the entity appears in the top-N rows of the source table
/// when ranked by absolute effect size descending — recomputed here rather
/// than trusting the table's physical row order, which may be sorted by
/// p-value, gene name, or anything else. When the claim excerpt doesn't name
/// an explicit N, uses a generous default of 10.
fn verify_rank_top_n(
    claim: &Claim,
    index: &TableIndex,
    cfg: &ExtractorConfig,
    cache: &mut BTreeMap<PathBuf, CachedTable>,
) -> ClaimStatus {
    let Some(source_ref) = claim.source_table.as_deref() else {
        return ClaimStatus::Unverifiable {
            reason: "no source table cited — cannot check rank membership".into(),
        };
    };
    let (path, cached) = match cached_table_for(cache, index, source_ref, cfg) {
        Ok(t) => t,
        Err(status) => return status,
    };

    // Parse an explicit N from the excerpt ("top-10", "top 5", etc.).
    let n: usize = {
        let re = &*RANK_TOP_N_RE;
        re.captures(&claim.excerpt.to_lowercase())
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(10)
    };

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

    // The claimed entity must itself carry a numeric effect size; without one
    // it cannot be ranked, so its top-N membership is unverifiable.
    let Some(claimed_eff) = lookup_numeric(&claimed_row.values, &cfg.effect_size_columns) else {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "entity `{}` has no numeric effect size in `{}` — cannot rank",
                claim.entity,
                table_label(&path)
            ),
        };
    };

    // VF-6 — a ranked claim that ALSO asserts a direction ("the top-3 most
    // UP-regulated genes include X") must have X's own sign agree. Ranking is
    // by |effect size|, so a large-NEGATIVE gene can legitimately sit in the
    // top-N by magnitude while flatly contradicting an "upregulated" claim;
    // that signed-vs-magnitude confusion is a fabrication, not a pass. Only
    // fires when the sign positively contradicts (obs nonzero, opposite sign),
    // so a faithful "top-N upregulated" naming a positive gene still Verifies.
    if let Some(direction) = claim.direction {
        let observed_direction = if claimed_eff > 0.0 {
            Some(Direction::Up)
        } else if claimed_eff < 0.0 {
            Some(Direction::Down)
        } else {
            None
        };
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

    // Rank by |effect size| descending, recomputed from the configured
    // effect-size columns. Rows that lack a numeric effect size are dropped
    // (they cannot be ranked) rather than silently kept in row order. The
    // tie-break on entity name keeps the ordering stable + deterministic.
    let mut ranked: Vec<(&str, f64)> = cached
        .rows
        .iter()
        .filter_map(|r| {
            lookup_numeric(&r.values, &cfg.effect_size_columns)
                // Drop non-finite effect sizes (NaN/±inf from "NA"/blank cells):
                // they cannot be ranked and would poison the sort comparator.
                .filter(|eff| eff.is_finite())
                .map(|eff| (r.entity.as_str(), eff.abs()))
        })
        .collect();

    // If no row in the table carries an effect size, there is nothing to rank.
    if ranked.is_empty() {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "table `{}` has no configured effect-size column — cannot rank",
                table_label(&path)
            ),
        };
    }

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
                "entity `{}` is not in the top-{} rows of `{}` ranked by |effect size|",
                claim.entity,
                n,
                table_label(&path)
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
    let (_path, cached) = match cached_table_for(cache, index, source_ref, cfg) {
        Ok(t) => t,
        Err(status) => return status,
    };
    let Some(row) = cached
        .rows
        .iter()
        .find(|r| r.entity.eq_ignore_ascii_case(&claim.entity))
    else {
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
    // Scope the immutable cache borrow so the trailing `verify_one`
    // can reacquire it mutably without aliasing.
    let early = {
        let (_path, cached) = match cached_table_for(cache, index, source_ref, cfg) {
            Ok(t) => t,
            Err(status) => return status,
        };
        let Some(row) = cached
            .rows
            .iter()
            .find(|r| r.entity.eq_ignore_ascii_case(&claim.entity))
        else {
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

    // Direction word cross-check: if narrative says "upregulated" but the
    // observed effect size is negative (or vice versa), flag it. This is
    // the highest-signal check and catches the lotz v1-style fabrication
    // pattern even when the numeric effect size was omitted.
    if let Some(direction) = claim.direction {
        let observed = lookup_numeric(&row.values, &cfg.effect_size_columns);
        if let Some(obs) = observed {
            // Near-zero / non-significance policy: a *bare* direction claim
            // (no stated effect-size value) on a gene that is both near-zero
            // (|log2FC| < EPS) and non-significant has no mechanically
            // determinable direction — neither confirmable nor refutable — so
            // it is `Unverifiable` rather than verified or flagged. Significance
            // is judged on the *adjusted* p (the largest reported p-value-family
            // value, so e.g. padj=0.16 reads non-significant even when raw
            // p<0.05); a claim that itself states an effect size is exempt
            // because it makes a checkable quantitative assertion.
            const NEAR_ZERO_LOG2FC: f64 = 0.5;
            if claim.effect_size.is_none() && obs.abs() < NEAR_ZERO_LOG2FC {
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
            // A zero observed effect size agrees with NEITHER direction — an
            // "upregulated"/"downregulated" claim on a no-change row is a
            // fabrication, not a confirm. Mirrors the strict `> 0.0` / `< 0.0`
            // direction guards on the count-recompute path.
            let observed_direction = if obs > 0.0 {
                Some(Direction::Up)
            } else if obs < 0.0 {
                Some(Direction::Down)
            } else {
                None
            };
            if observed_direction != Some(direction) {
                return ClaimStatus::Mismatch {
                    detail: format!(
                        "direction: narrative says {:?}, table effect size is {:+.4}",
                        direction, obs
                    ),
                };
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
/// Exact equality (incl. both-zero underflow) and the log-ratio band are
/// both accepted; non-positive values only match on exact equality.
fn pvalue_within_tolerance(claimed: f64, obs: f64, rel_tol: f64) -> bool {
    if claimed == obs {
        return true;
    }
    if claimed <= 0.0 || obs <= 0.0 {
        return false;
    }
    (claimed / obs).ln().abs() <= (1.0 + rel_tol).ln()
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
                if key.contains(tok) {
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
    /// `normalize(entity) -> row index` map. On duplicate entity keys
    /// the first occurrence wins (matches the prior `iter().find(...)`
    /// semantics, which returned the earliest matching row).
    fn from_rows(rows: Vec<TableRow>) -> Self {
        let mut by_entity: BTreeMap<String, usize> = BTreeMap::new();
        for (i, row) in rows.iter().enumerate() {
            by_entity.entry(normalize(&row.entity)).or_insert(i);
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

/// Get-or-load helper. Resolves `source_ref` against `index`, then
/// returns the cached `CachedTable` for the resolved path, loading
/// it from disk on first miss. Returns `Err(ClaimStatus::Unverifiable)`
/// when the table cannot be located or read so callers can short-circuit
/// without duplicating the diagnostic strings.
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
    Ok(CachedTable::from_rows(rows))
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
        r"(?i)\b(\d[\d,]*)\s+(?:[A-Za-z][\w-]*\s+){0,5}?(gene[\s-]?sets?|cell[\s-]?types?|sub[\s-]?types?|genes?|features?|transcripts?|proteins?|peaks?|sites?|probes?|pathways?|terms?|cpgs?|loci|locus|snps?|variants?|regions?|clusters?|cells?|samples?|modules?|components?|domains?|communities|community|programs?|taxa|taxon|otus?|asvs?|species|genera|genus|families|family|phyla|phylum|lineages?)\b",
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
        r"(?i)(p[\s._-]?adj|adj[\s._-]?p(?:[\s._-]?val(?:ue)?)?|fdr|q[\s._-]?val(?:ue)?|adjusted\s+p[\s-]?val(?:ue)?|p[\s-]?val(?:ue)?|p)\s*[<≤]\s*(\d*\.?\d+(?:[eE][+-]?\d+)?)",
    )
    .expect("static regex")
});

/// True when a p-value-family keyword (or column name) denotes a
/// *multiple-testing-adjusted* quantity rather than a raw p-value.
fn is_adjusted_pvalue_keyword(kw: &str) -> bool {
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
    let Some(base) = Path::new(evidence.trim()).file_name().and_then(|s| s.to_str()) else {
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
}

/// Load `claims_evidence_matrix.csv` into typed rows. `prior_pmids` is a
/// `;`-joined list per the schema; empty / non-numeric tokens are dropped.
/// The parse is pure CSV (comma-delimited, headers required) and tolerant of
/// missing optional columns — only `finding_id` and `entity` are required to
/// keep a row.
fn load_literature_rows(path: &Path) -> Result<Vec<LiteratureRow>> {
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
    // Accept both the plural `prior_pmids` and the singular `prior_pmid` the
    // contextualize step actually emits. Without the singular alias every row's
    // PMID list parsed empty, so a narrative that correctly cited a prior PMID
    // was falsely flagged "cites PMID X but no supporting row" (Mismatch).
    let pmids_idx = col("prior_pmids").or_else(|| col("prior_pmid"));
    let flag_idx = col("concordance_flag");
    let source_idx = col("source_kind");
    let verified_idx = col("verified");

    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record?;
        let get = |i: Option<usize>| -> String {
            i.and_then(|k| record.get(k))
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let prior_pmids = pmids_idx
            .and_then(|k| record.get(k))
            .map(|raw| {
                raw.split(';')
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
        });
    }
    Ok(rows)
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
    let Some(evidence) = claim.literature_evidence.as_ref() else {
        return ClaimStatus::Unverifiable {
            reason: "literature-grounded claim carries no finding_id / cited PMIDs".into(),
        };
    };
    let Some(matrix_path) = resolve_evidence_literature(
        package_root,
        &evidence.finding_id,
        &evidence.cited_pmids,
        cfg,
    ) else {
        return ClaimStatus::Unverifiable {
            reason: "claims_evidence_matrix.csv not found in package".into(),
        };
    };
    let rows = match load_literature_rows(&matrix_path) {
        Ok(r) => r,
        Err(e) => {
            return ClaimStatus::Unverifiable {
                reason: format!("claims_evidence_matrix.csv unreadable: {:#}", e),
            }
        }
    };

    // Rows backing this finding: prefer an exact finding_id match; fall back to
    // entity match (older matrices keyed only by entity).
    let entity_norm = normalize(&claim.entity);
    let matched: Vec<&LiteratureRow> = rows
        .iter()
        .filter(|r| {
            r.finding_id.eq_ignore_ascii_case(&evidence.finding_id)
                || normalize(&r.entity) == entity_norm
        })
        .collect();
    if matched.is_empty() {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "no claims_evidence_matrix row for finding `{}` / entity `{}`",
                evidence.finding_id, claim.entity
            ),
        };
    }

    // Any matched row asserting opposite-direction prior literature contradicts
    // a concordance claim → Mismatch.
    if matched
        .iter()
        .any(|r| r.concordance_flag == "opposite_direction")
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
    if matched.iter().any(|r| r.concordance_flag == "no_prior_finding") {
        let lower = claim.excerpt.to_lowercase();
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
        if AGREEMENT_CUES.iter().any(|c| lower.contains(c)) {
            return ClaimStatus::Mismatch {
                detail: format!(
                    "literature: narrative asserts prior-work concordance for `{}` but the matrix records no_prior_finding",
                    claim.entity
                ),
            };
        }
    }

    // Every narrative-cited PMID must appear in the matrix's supporting set.
    let mut supporting: std::collections::BTreeSet<u64> = std::collections::BTreeSet::new();
    let mut sources: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut any_verified = false;
    for r in &matched {
        if r.verified {
            any_verified = true;
        }
        for p in &r.prior_pmids {
            supporting.insert(*p);
        }
        if !r.source_kind.is_empty() && r.source_kind != "none" {
            sources.insert(r.source_kind.clone());
        }
    }
    for cited in &evidence.cited_pmids {
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
                evidence.finding_id
            ),
        };
    }
    if supporting.len() < cfg.literature_min_papers {
        return ClaimStatus::Unverifiable {
            reason: format!(
                "literature: {} supporting paper(s) for `{}`, policy requires >= {}",
                supporting.len(),
                claim.entity,
                cfg.literature_min_papers
            ),
        };
    }
    if sources.len() < cfg.literature_min_sources {
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

/// Attempt to verify `text` as an aggregate count claim against
/// `table_path`. Returns `None` when the text is not count-shaped (no
/// "N <noun>" + threshold), so the caller can fall back to per-entity
/// verification. Recomputes the count from the table rather than trusting
/// the agent's figure: counts rows whose configured p-value column is
/// below the claimed threshold and (when present) whose effect size
/// satisfies the claimed direction / magnitude constraint.
fn verify_count_claim(text: &str, table_path: &Path, cfg: &ExtractorConfig) -> Option<ClaimStatus> {
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

    let cached = load_table_rows(table_path, &cfg.entity_columns).ok()?;

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
    let lower = text.to_lowercase();
    let has_up = cfg
        .up_words
        .iter()
        .any(|w| lower.contains(&w.to_lowercase()));
    let has_down = cfg
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

/// Compare a claimed count against the recomputed `observed`, allowing a
/// small relative band (counts vary with NA / tie handling) while still
/// catching fabricated figures.
fn compare_count(claimed_n: f64, observed: usize, table_path: &Path, what: &str) -> ClaimStatus {
    let tol = (claimed_n * 0.02).max(2.0);
    if (observed as f64 - claimed_n).abs() <= tol {
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

/// True for nouns that denote a *grouping* whose count is the number of
/// distinct labels (cluster ids, cell types, modules, taxa), as opposed
/// to a per-row entity (gene, peak) whose count needs a threshold.
fn is_grouping_noun(noun: &str) -> bool {
    let n = noun.replace(['-', '_'], " ");
    let n = n.trim();
    matches!(
        n,
        "cluster"
            | "clusters"
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
        "celltype",
        "cell_type",
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
        },
        status,
        strength: ClaimStrength::Exploratory,
    };

    let Some(evidence) = sc.evidence.as_deref().filter(|e| !e.trim().is_empty()) else {
        return make(
            summarize_claim_subject(&sc.claim),
            ClaimStatus::Unverifiable {
                reason: "claim cites no evidence file".into(),
            },
            None,
        );
    };
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
            ClaimStatus::Unverifiable {
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
        let status = verify_for_contract(&claim, &index, cfg, &mut cache);
        return ClaimVerdict {
            claim,
            status,
            strength: ClaimStrength::Exploratory,
        };
    }

    // 3. Nothing numeric/countable to check (e.g. a methodological note).
    make(
        summarize_claim_subject(&sc.claim),
        ClaimStatus::Unverifiable {
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
    let mut verdicts = Vec::new();
    for claim in claims {
        // Literature-grounded claims are verified against the package's
        // claims_evidence_matrix.csv, never a numeric result table — route
        // them before the table-discovery branch.
        if claim.contract == ClaimContract::LiteratureGrounded {
            let status = verify_literature_grounded_at(claim, package_root, cfg);
            verdicts.push(ClaimVerdict {
                claim: claim.clone(),
                status,
                strength: ClaimStrength::Exploratory,
            });
            continue;
        }
        if claim.source_table.is_some() {
            let status = verify_for_contract(claim, &cited_index, cfg, &mut cache);
            verdicts.push(ClaimVerdict {
                claim: claim.clone(),
                status,
                strength: ClaimStrength::Exploratory,
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
                if !cache.contains_key(*cand) {
                    match load_table_rows(cand, &cfg.entity_columns) {
                        Ok(t) => {
                            cache.insert((*cand).clone(), t);
                        }
                        Err(e) => {
                            tracing::warn!(
                                target: "ecaa::claim_verifier",
                                table = %cand.display(),
                                error = %e,
                                "result table failed to load during claim discovery; excluding it"
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

        let (claim_out, status) = if containing.is_empty() {
            (
                claim.clone(),
                ClaimStatus::Unverifiable {
                    reason: format!("entity `{}` not found in any result table", claim.entity),
                },
            )
        } else {
            let mut best: Option<ClaimStatus> = None;
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
                let status = verify_for_contract(&c, &idx, cfg, &mut cache);
                let verified = matches!(status, ClaimStatus::Verified);
                let prefer = match &best {
                    None => true,
                    // Verified beats everything; Mismatch beats Unverifiable.
                    Some(ClaimStatus::Verified) => false,
                    Some(ClaimStatus::Mismatch { .. }) => verified,
                    Some(ClaimStatus::Unverifiable { .. }) => {
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
                }
                if matches!(best, Some(ClaimStatus::Verified)) {
                    break;
                }
            }
            (chosen, best.expect("non-empty containing set"))
        };
        verdicts.push(ClaimVerdict {
            claim: claim_out,
            status,
            strength: ClaimStrength::Exploratory,
        });
    }
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
            },
            status: ClaimStatus::Verified,
            strength: ClaimStrength::Exploratory,
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
            },
            status: ClaimStatus::Verified,
            strength: ClaimStrength::Exploratory,
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
            },
            status: ClaimStatus::Verified,
            strength: ClaimStrength::Exploratory,
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
                "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}"],
                "directionVocab": {
                    "up": ["upregulated", "increased", "elevated"],
                    "down": ["downregulated", "decreased", "reduced"]
                },
                "effectSizeColumns": ["log2FC", "logFC"],
                "entityColumns": ["gene", "symbol"],
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
        let acan = report.verdicts.iter().find(|v| v.claim.entity == "ACAN").unwrap();
        assert!(
            matches!(acan.status, ClaimStatus::Suspicious { .. }),
            "absent entity + specific effect size + same namespace must be Suspicious, got {:?}",
            acan.status
        );
        assert_eq!(report.n_suspicious, 1);
        assert_eq!(report.n_mismatch, 0, "Suspicious must not count as a mismatch");
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
        let acan = report.verdicts.iter().find(|v| v.claim.entity == "ACAN").unwrap();
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
        use crate::claim_extractor::{extract_claims, ExtractorConfig};
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
        let v = report.verdicts.iter().find(|v| v.claim.entity == "CASP3").unwrap();
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
        let v2 = report2.verdicts.iter().find(|v| v.claim.entity == "CASP3").unwrap();
        assert!(
            matches!(v2.status, ClaimStatus::Verified),
            "faithful FDR-significant claim must Verify, got {:?}",
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
        let v = report.verdicts.iter().find(|v| v.claim.entity == "ACAN").unwrap();
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
        let v2 = report2.verdicts.iter().find(|v| v.claim.entity == "ACAN").unwrap();
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
        let v3 = report3.verdicts.iter().find(|v| v.claim.entity == "ACAN").unwrap();
        assert!(
            matches!(v3.status, ClaimStatus::Verified),
            "raw value under a raw label is not laundering and must Verify, got {:?}",
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
        let fab = extract_claims("DOWN is among the top-2 upregulated genes (Table S1).", &cfg);
        let dc = fab.iter().find(|c| c.entity == "DOWN").unwrap();
        assert_eq!(dc.contract, ClaimContract::RankTopN);
        let report = verify_claims(&fab, tmp.path(), &cfg);
        let v = report.verdicts.iter().find(|v| v.claim.entity == "DOWN").unwrap();
        assert!(
            matches!(v.status, ClaimStatus::Mismatch { .. }),
            "top-N claim with wrong direction must be caught, got {:?}",
            v.status
        );

        // Faithful twin: UP is positive and in the top-2 → Verified.
        let faithful = extract_claims("UP is among the top-2 upregulated genes (Table S1).", &cfg);
        let report2 = verify_claims(&faithful, tmp.path(), &cfg);
        let v2 = report2.verdicts.iter().find(|v| v.claim.entity == "UP").unwrap();
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
        write_table(tmp.path(), "de_s1.tsv", "gene\tlog2FC\tpadj\nACAN\t-2.8\t0.001\n");

        // Fabrication: claims UP, table is DOWN → must be caught.
        let fab = extract_claims(
            "ACAN was upregulated as a marker of healthy phenotype (Table S1).",
            &cfg,
        );
        let acan = fab.iter().find(|c| c.entity == "ACAN").unwrap();
        assert_eq!(acan.contract, ClaimContract::Categorical);
        let report = verify_claims(&fab, tmp.path(), &cfg);
        let v = report.verdicts.iter().find(|v| v.claim.entity == "ACAN").unwrap();
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
        let v2 = report2.verdicts.iter().find(|v| v.claim.entity == "ACAN").unwrap();
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
        std::fs::write(nested.join("aux.tsv"), "gene\tlog2FC\tpadj\nA\t2.0\t0.001\n").unwrap();
        let present = StructuredClaim {
            claim: "5 genes are differentially expressed (padj < 0.05)".into(),
            evidence: Some("aux.tsv".into()),
        };
        let v2 = verify_structured_claims(&[present], tmp.path(), &cfg);
        assert!(
            matches!(v2[0].status, ClaimStatus::Unverifiable { .. }),
            "present-but-unresolved evidence must stay Unverifiable, got {:?}",
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
            "finding_id,entity,prior_pmids,concordance_flag,source_kind,verified\n\
             finding_42,TP53,12345678;23456789,same_direction,pmc_oa_full_text,true\n\
             finding_9,EGFR,,no_prior_finding,none,false\n",
        )
        .unwrap();
        let rows = load_literature_rows(&p).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].finding_id, "finding_42");
        assert_eq!(rows[0].prior_pmids, vec![12345678, 23456789]);
        assert_eq!(rows[0].concordance_flag, "same_direction");
        assert_eq!(rows[0].source_kind, "pmc_oa_full_text");
        assert!(rows[0].verified);
        assert!(rows[1].prior_pmids.is_empty());
        assert!(!rows[1].verified);
    }

    fn write_lit_matrix(root: &Path, body: &str) {
        let dir = root.join("runtime/outputs/contextualize_findings_with_literature");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("claims_evidence_matrix.csv"), body).unwrap();
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
    fn literature_grounded_unverifiable_when_no_evidence_block() {
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
        assert!(
            matches!(status, ClaimStatus::Unverifiable { .. }),
            "{status:?}"
        );
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
}
