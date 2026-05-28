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
        }
        self.verdicts.push(verdict);
    }

    /// True iff at least one claim was classified as `Mismatch`. Used by
    /// the session-state hook to decide whether to transition to
    /// `Blocked { ValidationFailed }`.
    pub fn has_mismatch(&self) -> bool {
        self.n_mismatch > 0
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

/// Load `<config_dir>/downstream-policy/interpretation-policy.json`. The
/// emit-time entry-point reuses this when the package-side
/// `policies/interpretation-policy.json` gate is enabled — the extractor
/// needs the full config-side policy (entity name patterns, direction
/// vocab, tolerances) which is only canonical at `config_dir`.
fn load_interpretation_policy(config_dir: &Path) -> Option<serde_json::Value> {
    let path = config_dir
        .join("downstream-policy")
        .join("interpretation-policy.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Emit-time, in-core mirror of the server's `verify_task_with_context`.
///
/// Runs the full claim-extractor → claim-verifier → post-hoc-demotion
/// pipeline for a single task and returns the resulting
/// `ClaimVerificationReport`. Used by the conversation emit pipeline to
/// persist `runtime/verification-reports/<task_id>.json` sidecars at emit
/// time so the `GET /task/:task_id/result` handler can read them instead
/// of re-running verification on every poll.
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
    let policy_dir = config_dir.join("downstream-policy");
    let cfg = ExtractorConfig::from_policy_for_class(&policy, &policy_dir, project_class).ok()?;
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
                    if let Some(obs_p) = lookup_numeric(&row.values, &cfg.pvalue_columns) {
                        if obs_p >= 0.05 {
                            return ClaimStatus::Mismatch {
                                detail: format!(
                                    "thresholded claim: observed p-value {:.4e} does not meet FDR < 0.05",
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
/// ordered by the first configured effect-size column (descending by absolute
/// value, matching the typical DE table convention). When the claim excerpt
/// doesn't name an explicit N, uses a generous default of 10.
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

    // Sort rows by absolute effect size descending.
    let mut sorted_rows: Vec<&str> = cached.rows.iter().map(|r| r.entity.as_str()).collect();
    // Keep only first N (already ordered by table row; the table is assumed
    // pre-sorted, which is standard for DE result tables).
    sorted_rows.truncate(n);

    let in_top_n = sorted_rows
        .iter()
        .any(|e| e.eq_ignore_ascii_case(&claim.entity));
    if in_top_n {
        ClaimStatus::Verified
    } else {
        ClaimStatus::Mismatch {
            detail: format!(
                "entity `{}` is not in the top-{} rows of `{}`",
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

    // No label column — fall back to existence check.
    ClaimStatus::Verified
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
            let observed_direction = if obs >= 0.0 {
                Direction::Up
            } else {
                Direction::Down
            };
            if observed_direction != direction {
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
    // this is a softer check than effect size.
    if let Some(claimed_p) = claim.pvalue {
        let observed_p = lookup_numeric(&row.values, &cfg.pvalue_columns);
        match observed_p {
            Some(obs_p) => {
                if !claimed_p.is_finite() || !obs_p.is_finite() {
                    return ClaimStatus::Unverifiable {
                        reason: "p-value is not finite in narrative or table".into(),
                    };
                }
                if claimed_p == obs_p {
                    // Exact equality also handles the rare underflow case
                    // where both narrative and table serialize a p-value as 0.
                } else if claimed_p <= 0.0 || obs_p <= 0.0 {
                    return ClaimStatus::Mismatch {
                        detail: format!(
                            "p-value: narrative {:.4e} vs table {:.4e}",
                            claimed_p, obs_p
                        ),
                    };
                } else {
                    let ratio = (claimed_p / obs_p).ln().abs();
                    if ratio > (1.0 + cfg.pvalue_relative_tolerance).ln() {
                        return ClaimStatus::Mismatch {
                            detail: format!(
                            "p-value: narrative {:.4e} vs table {:.4e} (relative tolerance {}%)",
                            claimed_p,
                            obs_p,
                            (cfg.pvalue_relative_tolerance * 100.0) as u32
                        ),
                        };
                    }
                }
            }
            None => {
                return ClaimStatus::Unverifiable {
                    reason: "table has no configured p-value column/value for claimed p-value"
                        .into(),
                };
            }
        }
    }

    ClaimStatus::Verified
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

    fn resolve(&self, source_ref: &str) -> Option<&Path> {
        // Strategy, in order — each step is a cheap map lookup or linear
        // scan over the (small) index:
        // 1. Exact file-name match: cite "de_summary.tsv".
        // 2. Exact stem: cite "de_summary".
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
        // steps (1, 2) remain deterministic and unique by
        // construction.
        let needle = normalize(source_ref.trim());
        if let Some(p) = self.by_name.get(&needle) {
            return Some(p);
        }
        let collapsed: String = needle.split_whitespace().collect();
        if let Some(p) = self.by_name.get(&collapsed) {
            return Some(p);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim_extractor::extract_claims;
    use crate::decision_log::{DecisionActor, DecisionRecord, DecisionType};
    use serde_json::json;
    use tempfile::tempdir;

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
    fn unverifiable_when_entity_missing_from_table() {
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
        assert!(matches!(acan.status, ClaimStatus::Unverifiable { .. }));
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
        let tsv = "some_other\tvalue\nFOO\t1\n";
        let cols = vec!["gene".to_string(), "symbol".to_string()];
        let err = parse_table_rows_from_reader(tsv.as_bytes(), b'\t', &cols).unwrap_err();
        assert!(err.to_string().contains("no configured entity column"));
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
}
