//! Claim-verification and package-finalize orchestration shared by the
//! server's per-task re-verify hook and the harness's end-of-run path.
//!
//! Finalizing a completed task means: re-run claim verification FROM SOURCE
//! (never trusting the agent-writable sidecar), compute structured-claims
//! coverage against the per-package expected-claim manifest, persist the
//! HMAC-signed verdict sink (de-vacuifies audit-proof Inv 1/5), then refresh
//! package-wide evidence and audit artifacts at the package convergence point.
//!
//! This module owns only the pure/sync orchestration: no session, no HTTP,
//! no telemetry, no state transitions. Those stay in the server, which calls
//! [`finalize_task_verdicts`] and acts on the returned
//! [`TaskFinalizeOutcome`]. The harness (which links only against `core`) calls
//! [`finalize_package`] to refresh package-wide artifacts once after all tasks
//! are terminal.

use crate::claim_contract::ClaimContract;
use crate::claim_extractor::{
    claim_dedupe_key, extract_claims, extract_markdown_table_claims,
    resolve_result_table_columns_with_schema, Claim, Direction, ExtractorConfig,
};
use crate::claim_verifier::{
    demote_claims_from_deviations, verdict_class_of, verify_claims_with_discovery,
    verify_claims_with_discovery_cached, verify_narrative_counts_for, verify_structured_claims,
    ClaimDiscoveryCache, ClaimStatus, ClaimStrength, ClaimVerdict, ClaimVerificationReport,
    StructuredClaim, VerdictAudit, VerdictClass, CLAIM_VERIFIER_VERSION,
};
use crate::clock::WallClock;
use crate::coverage::CoverageResult;
use crate::decision_log::DecisionRecord;
use crate::project_class::ProjectClass;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Result of running verification for a single task.
pub struct TaskVerification {
    /// Absolute path to the narrative artifact that was verified.
    pub narrative_path: PathBuf,
    /// Claim-by-claim verification report.
    pub report: ClaimVerificationReport,
}

/// Outcome of attempting verification for a single task, distinguishing
/// the three states a caller must handle differently:
///
/// - `Verified` — verification actually ran over ≥1 claim (mismatch or not);
///   inspect the report.
/// - `Disabled` — the policy is present but `verifiableEntities` is off, OR
///   the task genuinely had nothing to verify under an enabled policy (no
///   narrative + no structured claims). Either way this is a benign
///   "nothing to do", NOT a configuration defect.
/// - `Unavailable` — the policy file is absent/unreadable/malformed, or its
///   extractor config failed to build. A configuration defect that callers
///   must surface loudly rather than treating as a benign 200.
pub enum VerifyOutcome {
    Verified(TaskVerification),
    Disabled,
    Unavailable { reason: String },
}

impl VerifyOutcome {
    /// Short discriminant label for logging / diagnostics without requiring
    /// `Debug` on the embedded `TaskVerification`.
    pub fn label(&self) -> &'static str {
        match self {
            VerifyOutcome::Verified(_) => "verified",
            VerifyOutcome::Disabled => "disabled",
            VerifyOutcome::Unavailable { .. } => "unavailable",
        }
    }
}

/// The narrative-derived claim set of ONE task: prose claims plus
/// markdown-table row claims, in document order. One source of truth so the
/// cross-task ledger prepass and the verification pass extract IDENTICALLY —
/// a drift between the two would silently un-dedupe or drop claims.
fn narrative_claims_from_text(narrative: &str, cfg: &ExtractorConfig) -> Vec<Claim> {
    let mut claims = extract_claims(narrative, cfg);
    claims.extend(extract_markdown_table_claims(narrative, cfg));
    claims
}

/// Read the task-owned prose used for claim extraction. A dedicated narrative
/// artifact wins. Compute stages commonly retain their only prose in the
/// standard result envelope, so fall back to the top-level `narrative`,
/// `narrative_text`, `summary`, and `interpretation` string fields in that
/// order, joining distinct values when an envelope intentionally carries more
/// than one.
fn read_task_narrative(package_root: &Path, task_id: &str) -> Option<String> {
    if let Some(path) = find_narrative_artifact(package_root, task_id) {
        if let Ok(text) = std::fs::read_to_string(path) {
            if !text.trim().is_empty() {
                return Some(text);
            }
        }
    }
    let task_dir = resolve_task_runtime_dir_local(package_root, task_id)?;
    let result: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(task_dir.join("result.json")).ok()?).ok()?;
    let mut parts = Vec::new();
    for field in ["narrative", "narrative_text", "summary", "interpretation"] {
        let Some(text) = result.get(field).and_then(serde_json::Value::as_str) else {
            continue;
        };
        let text = text.trim();
        if !text.is_empty() && !parts.contains(&text) {
            parts.push(text);
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

/// Which task OWNS each distinct narrative assertion in a package, and which
/// other tasks repeat it.
///
/// A `final_reporting` narrative typically restates the `reporting` narrative
/// (in production: 4,089 identical claim texts, 0 unique to either), so
/// verifying both yields two verdicts per assertion — inflating `n_checked`
/// with no added signal and double-counting every row of a copied table. The
/// ledger assigns each `(normalized_text, contract)` key to the FIRST task that
/// asserts it (deterministic task order) and records the full asserter list, so
/// downstream verification emits ONE verdict per assertion while still stating
/// which tasks made it.
///
/// Dedupe is strictly ACROSS tasks: within a single task every claim is kept,
/// so several entities extracted from one sentence, and a sentence repeated in
/// two places of the same report, remain independently verifiable.
#[derive(Debug, Default)]
pub struct CrossTaskClaimLedger {
    entries: BTreeMap<(String, &'static str), LedgerEntry>,
}

#[derive(Debug)]
struct LedgerEntry {
    owner: String,
    asserters: Vec<String>,
}

impl CrossTaskClaimLedger {
    fn record(&mut self, task_id: &str, claim: &Claim) {
        let key = claim_dedupe_key(claim);
        let entry = self.entries.entry(key).or_insert_with(|| LedgerEntry {
            owner: task_id.to_string(),
            asserters: Vec::new(),
        });
        if entry.asserters.last().map(String::as_str) != Some(task_id) {
            entry.asserters.push(task_id.to_string());
        }
    }

    /// Build the ledger by extracting every listed task's narrative claims in
    /// the given order. `task_ids` must be in a deterministic order (the
    /// caller's `WORKFLOW.json` key order) — ownership is first-seen-wins, so
    /// the order decides which task's verdict survives.
    pub fn build(package_root: &Path, task_ids: &[String], cfg: &ExtractorConfig) -> Self {
        let mut ledger = Self::default();
        for task_id in task_ids {
            let Some(narrative) = read_task_narrative(package_root, task_id) else {
                continue;
            };
            for claim in narrative_claims_from_text(&narrative, cfg) {
                ledger.record(task_id, &claim);
            }

            // Aggregate count claims are produced by the numeric verifier
            // rather than the prose extractor. Record their assertion keys as
            // well, otherwise a verbatim reporting → final_reporting copy
            // duplicates every count verdict even though entity claims are
            // correctly deduped.
            let tables_root = package_root.join("results").join("tables");
            let effective_root = if tables_root.is_dir() {
                tables_root
            } else {
                resolve_task_runtime_dir_local(package_root, task_id)
                    .unwrap_or_else(|| package_root.join("runtime").join(task_id))
            };
            for verdict in verify_narrative_counts_for(
                &narrative,
                &effective_root,
                package_root,
                cfg,
                Some(task_id.as_str()),
            ) {
                ledger.record(task_id, &verdict.claim);
            }
        }
        ledger
    }

    /// `true` when `task_id` is the owner of this claim's assertion (or the
    /// assertion is unknown to the ledger, which keeps the claim rather than
    /// silently dropping it).
    pub fn owns(&self, task_id: &str, claim: &Claim) -> bool {
        match self.entries.get(&claim_dedupe_key(claim)) {
            Some(entry) => entry.owner == task_id,
            None => true,
        }
    }

    /// The OTHER tasks that asserted this claim verbatim, in first-seen order.
    pub fn co_asserters(&self, task_id: &str, claim: &Claim) -> Vec<String> {
        self.entries
            .get(&claim_dedupe_key(claim))
            .map(|entry| {
                entry
                    .asserters
                    .iter()
                    .filter(|t| t.as_str() != task_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Number of distinct assertions recorded.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no assertion was recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Record on a verdict that the SAME assertion was made verbatim by other
/// tasks, so collapsing the duplicates to one verdict stays auditable. Appends
/// to the audit rationale (creating a class-only audit when the verifier
/// produced none) rather than adding a field to the wire-facing verdict type.
fn note_shared_assertion(verdict: &mut ClaimVerdict, others: &[String]) {
    if others.is_empty() {
        return;
    }
    let note = format!("also asserted verbatim by task(s): {}", others.join(", "));
    if verdict.audit.is_none() {
        let entity = verdict.claim.entity.trim().to_string();
        let class = verdict_class_of(&verdict.claim);
        verdict.audit = Some(VerdictAudit {
            class,
            source_table: None,
            entity_column: None,
            entity_value: (!entity.is_empty()).then_some(entity),
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
        });
    }
    let Some(audit) = verdict.audit.as_mut() else {
        return;
    };
    audit.rationale = Some(match audit.rationale.take() {
        Some(existing) if !existing.is_empty() => format!("{existing}; {note}"),
        _ => note,
    });
}

/// Absolute-tolerance FLOOR for the `report-data.json` → source-artifact
/// transcription check. Both sides parse the same decimal text, so agreement is
/// equality up to float round-trip; the floor plus
/// [`REPORT_DATA_TRANSCRIPTION_RELATIVE`] only absorbs a re-render at lower
/// precision, never an order-of-magnitude error.
const REPORT_DATA_TRANSCRIPTION_TOLERANCE: f64 = 1e-12;
/// Relative component of the transcription tolerance, so a legitimately tiny
/// significance value is not compared against an absolute floor that swamps it.
const REPORT_DATA_TRANSCRIPTION_RELATIVE: f64 = 1e-9;

/// One `TaskNode` as seen by result-schema resolution — only the two fields it
/// needs. Mirrors `provenance::sidecars::TaskNodeReadAllowanceRow`'s
/// minimal-shape rationale: a narrow row keeps the parse tolerant of every
/// other `TaskNode` field evolving.
#[derive(serde::Deserialize)]
struct TaskNodeResultSchemaRow {
    id: String,
    #[serde(default)]
    attributes: BTreeMap<String, serde_json::Value>,
}

/// Each task's DECLARED [`crate::report_contract::ResultSchema`], read from
/// `runtime/task-nodes.json`'s `TaskNode::attributes["result_schema"]` (put
/// there by `workflow_contracts::from_atom::preserve_attributes` from the atom's
/// own declaration).
///
/// This is the SAME declaration `report_contract::assemble` reads when it builds
/// `report-data.json`, so resolving a checker's columns through it is what makes
/// the assertion and the evidence address the same cells. Best-effort by design:
/// an absent / unreadable / unparseable file yields an empty map and every
/// caller falls back to header-only resolution, so an older package still
/// verifies (just without the declared-name guarantee). Tasks that declare no
/// schema are simply omitted.
fn read_task_result_schemas(
    package_root: &Path,
) -> BTreeMap<String, crate::report_contract::ResultSchema> {
    let path = package_root.join("runtime/task-nodes.json");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    let Ok(rows) = serde_json::from_str::<Vec<TaskNodeResultSchemaRow>>(&body) else {
        return BTreeMap::new();
    };
    rows.into_iter()
        .filter_map(|row| {
            let raw = row.attributes.get("result_schema")?.clone();
            let parsed: crate::report_contract::ResultSchema = serde_json::from_value(raw).ok()?;
            Some((row.id, parsed))
        })
        .collect()
}

/// Extend the policy's global verifier vocabulary with every column role
/// declared by the package's executable result schemas.
///
/// The interpretation policy provides useful cross-project defaults, but it
/// cannot enumerate every header an arbitrary analytical atom may introduce.
/// A retained `ResultSchema` is the authoritative, modality-neutral contract
/// for those additional names. Adding its entity, effect, and significance
/// columns lets narrative/count verification load and inspect such artifacts
/// without teaching the verifier task ids, scientific nouns, or modality
/// vocabularies.
///
/// Policy order stays intact and declared names are appended only when absent.
/// Per-artifact cell verification still uses the producing schema directly
/// through [`resolve_result_table_columns_with_schema`], so this package-wide
/// vocabulary is a discovery fallback rather than a replacement for exact
/// schema binding.
fn extend_extractor_config_with_result_schemas(
    package_root: &Path,
    mut cfg: ExtractorConfig,
) -> ExtractorConfig {
    fn append_unique(columns: &mut Vec<String>, candidate: &str) {
        let candidate = candidate.trim();
        if candidate.is_empty()
            || columns
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(candidate))
        {
            return;
        }
        columns.push(candidate.to_string());
    }

    for schema in read_task_result_schemas(package_root).into_values() {
        append_unique(&mut cfg.entity_columns, &schema.entity_column);
        for alias in &schema.entity_column_aliases {
            append_unique(&mut cfg.entity_columns, alias);
        }
        if let Some(effect) = &schema.signed_effect_column {
            append_unique(&mut cfg.effect_size_columns, effect);
        }
        for alias in &schema.signed_effect_aliases {
            append_unique(&mut cfg.effect_size_columns, alias);
        }
        if let Some(significance) = &schema.significance {
            append_unique(&mut cfg.pvalue_columns, &significance.column);
        }
    }
    cfg
}

/// One cell-level comparison inside a report-data-derived verdict.
struct CellCheck {
    column: String,
    claimed: f64,
    observed: Option<f64>,
    tolerance: f64,
}

impl CellCheck {
    fn agrees(&self) -> Option<bool> {
        self.observed
            .map(|obs| (self.claimed - obs).abs() <= self.tolerance)
    }
}

/// Render a number for a human-readable trace excerpt: scientific for tiny
/// magnitudes (a significance value's full decimal expansion is unreadable),
/// plain otherwise. Locale-independent and byte-stable.
fn fmt_trace_number(v: f64) -> String {
    if v != 0.0 && v.abs() < 1e-4 {
        format!("{v:e}")
    } else {
        format!("{v}")
    }
}

/// The CANONICAL claim set over a package's significant entities, derived
/// straight from `report-data.json` and checked cell-by-cell against the
/// agent's ORIGINAL result artifact.
///
/// Why this exists: the terminal reports carry a marker-delimited block that is
/// RENDERED from `report-data.json`, so mining its rows as claims and checking
/// them against that same data is circular (the extractor now skips the block —
/// see `claim_extractor::strip_system_generated_blocks`). The non-circular
/// check is the one made here: `report-data.json` asserts a value; the source
/// artifact the assembler read is the evidence. Each verdict therefore carries
/// a full cell-level trace — `source_table`, `entity_column`,
/// `measurement_column`, `claimed_value`, `observed_value`,
/// `comparison_operator`, `absolute_tolerance`.
///
/// Emitted by exactly ONE task per package: the task whose runtime directory
/// holds `report-data.json` (the assembler's output). Every other task returns
/// an empty vec, so the canonical set is never duplicated across the reporting
/// and final-reporting stages.
///
/// Modality-agnostic: the entity / effect / significance columns are resolved
/// by [`resolve_result_table_columns_with_schema`] from the producing atom's
/// DECLARED [`ResultSchema`] — the same declaration the assembler read when it
/// wrote `report-data.json` — falling back to the artifact's own headers plus
/// the policy's configured columns. No domain column name appears here.
///
/// Reading the declared schema is what keeps the two sides comparable. The
/// assembler resolves `signed_effect_column` / `significance.column` by name;
/// a checker that re-derives the binding from headers alone can pick a
/// DIFFERENT column of the same family — an FDR-controlled claim against the
/// raw-p column, a normalized enrichment score against the raw one — and then
/// report every correctly-transcribed row as a mismatch.
///
/// Abstain-first: when NONE of an artifact's significant entities resolve to a
/// row, the artifact is skipped entirely (the assembler keyed on a different
/// identifier form — an id-shape difference, not a fabrication), and an
/// individual unresolved entity is `Unverifiable`, never `Mismatch`.
fn report_data_cell_verdicts(
    package_root: &Path,
    task_id: &str,
    cfg: &ExtractorConfig,
) -> Vec<ClaimVerdict> {
    let schemas = read_task_result_schemas(package_root);
    let Some(dir) = resolve_task_runtime_dir_local(package_root, task_id) else {
        return Vec::new();
    };
    let rd_path = dir.join("report-data.json");
    if !rd_path.is_file() {
        return Vec::new();
    }
    let Ok(raw) = std::fs::read_to_string(&rd_path) else {
        return Vec::new();
    };
    let report_data: crate::report_contract::ReportData = match serde_json::from_str(&raw) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                target: "ecaa::finalize",
                error = %e,
                task_id,
                "report-data.json did not deserialize — canonical claim set skipped"
            );
            return Vec::new();
        }
    };

    let mut out: Vec<ClaimVerdict> = Vec::new();
    for artifact in &report_data.artifacts {
        if artifact.spilled_to_attachment_only || artifact.significant_entities.is_empty() {
            continue;
        }
        let table_rel = format!(
            "runtime/outputs/{}/{}",
            artifact.stage_id, artifact.artifact
        );
        // The DECLARED schema of the stage that produced this artifact. Absent
        // (a package emitted before `task-nodes.json` recorded it) → header-only
        // resolution, exactly as before.
        let schema = schemas.get(&artifact.stage_id);
        let Some(table) = SourceArtifactIndex::load(&package_root.join(&table_rel), cfg, schema)
        else {
            continue;
        };
        // Abstain wholesale when the assembler's entity ids do not live in the
        // column we resolved: emitting one Mismatch per entity there would be a
        // mass false positive over an identifier-form difference.
        let any_resolved = artifact
            .significant_entities
            .iter()
            .any(|row| table.get(&row.entity).is_some());
        if !any_resolved {
            tracing::warn!(
                target: "ecaa::finalize",
                stage_id = %artifact.stage_id,
                table = %table_rel,
                entity_column = %table.entity_column,
                "no report-data entity resolves in the source artifact — canonical claim set \
                 skipped for this artifact"
            );
            continue;
        }

        for entity_row in &artifact.significant_entities {
            out.push(report_data_verdict(
                &artifact.stage_id,
                &table_rel,
                &table,
                entity_row,
                cfg,
            ));
        }
    }
    out
}

/// A source result artifact indexed by entity for cell lookup, with the
/// column roles resolved from its own headers.
struct SourceArtifactIndex {
    entity_column: String,
    effect_column: Option<String>,
    significance_column: Option<String>,
    /// normalized entity -> (effect cell, significance cell). First row wins on
    /// a duplicate key, matching the verifier's by-entity lookup semantics.
    rows: BTreeMap<String, (Option<f64>, Option<f64>)>,
}

impl SourceArtifactIndex {
    fn normalize(entity: &str) -> String {
        entity.trim().to_lowercase()
    }

    fn get(&self, entity: &str) -> Option<&(Option<f64>, Option<f64>)> {
        self.rows.get(&Self::normalize(entity))
    }

    /// Load `path`, resolving its entity / effect / significance columns against
    /// the producing atom's DECLARED `schema` first (`None` → header-only
    /// resolution). `None` when the file is unreadable or exposes no
    /// recognizable entity column (i.e. it is not a result table).
    fn load(
        path: &Path,
        cfg: &ExtractorConfig,
        schema: Option<&crate::report_contract::ResultSchema>,
    ) -> Option<Self> {
        let raw = std::fs::read(path).ok()?;
        let delimiter = crate::report_contract::assemble::delimiter_for(path);
        let mut reader = csv::ReaderBuilder::new()
            .delimiter(delimiter)
            .has_headers(true)
            .flexible(true)
            .from_reader(&raw[..]);
        let headers: Vec<String> = reader.headers().ok()?.iter().map(str::to_string).collect();
        let cols = resolve_result_table_columns_with_schema(&headers, cfg, schema)?;
        let entity_idx = headers.iter().position(|h| *h == cols.entity)?;
        let effect_idx = cols
            .effect
            .as_ref()
            .and_then(|name| headers.iter().position(|h| h == name));
        let significance_idx = cols
            .significance
            .as_ref()
            .and_then(|name| headers.iter().position(|h| h == name));

        let mut rows: BTreeMap<String, (Option<f64>, Option<f64>)> = BTreeMap::new();
        for record in reader.records().flatten() {
            let key = Self::normalize(record.get(entity_idx).unwrap_or_default());
            if key.is_empty() {
                continue;
            }
            let cell = |i: Option<usize>| {
                i.and_then(|i| record.get(i))
                    .map(str::trim)
                    .and_then(|c| c.parse::<f64>().ok())
                    .filter(|v| v.is_finite())
            };
            rows.entry(key)
                .or_insert_with(|| (cell(effect_idx), cell(significance_idx)));
        }
        Some(Self {
            entity_column: cols.entity,
            effect_column: cols.effect,
            significance_column: cols.significance,
            rows,
        })
    }
}

/// Build the verdict for ONE report-data significant entity, with its
/// cell-level trace.
fn report_data_verdict(
    stage_id: &str,
    table_rel: &str,
    table: &SourceArtifactIndex,
    entity_row: &crate::report_contract::EntityRow,
    cfg: &ExtractorConfig,
) -> ClaimVerdict {
    let observed = table.get(&entity_row.entity);
    let mut checks: Vec<CellCheck> = Vec::new();
    if let (Some(column), Some(claimed)) = (table.effect_column.clone(), entity_row.effect) {
        checks.push(CellCheck {
            column,
            claimed,
            observed: observed.and_then(|(e, _)| *e),
            // The effect column's tolerance is the policy's, so a report that
            // legitimately re-renders at the policy's precision still agrees.
            tolerance: cfg.log2fc_tolerance,
        });
    }
    if let (Some(column), Some(claimed)) =
        (table.significance_column.clone(), entity_row.significance)
    {
        checks.push(CellCheck {
            column,
            claimed,
            observed: observed.and_then(|(_, s)| *s),
            tolerance: REPORT_DATA_TRANSCRIPTION_TOLERANCE
                .max(claimed.abs() * REPORT_DATA_TRANSCRIPTION_RELATIVE),
        });
    }

    let mut excerpt = format!(
        "report-data.json asserts {} for stage {stage_id}",
        entity_row.entity
    );
    for check in &checks {
        excerpt.push_str(&format!(
            ": {} = {}",
            check.column,
            fmt_trace_number(check.claimed)
        ));
    }
    excerpt.push_str(&format!(" ({table_rel})"));

    let claim = Claim {
        entity: entity_row.entity.clone(),
        direction: entity_row.effect.map(|e| {
            if e >= 0.0 {
                Direction::Up
            } else {
                Direction::Down
            }
        }),
        effect_size: entity_row.effect,
        pvalue: entity_row.significance,
        source_table: Some(table_rel.to_string()),
        excerpt,
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

    // Report the comparison that DECIDES the verdict: the first disagreeing
    // cell when there is one, else the first compared cell, else the first
    // (uncompared) cell.
    let decisive = checks
        .iter()
        .find(|c| c.agrees() == Some(false))
        .or_else(|| checks.iter().find(|c| c.agrees() == Some(true)))
        .or_else(|| checks.first());

    let (status, rationale) = if observed.is_none() {
        (
            ClaimStatus::Unverifiable {
                reason: format!(
                    "entity `{}` not found in `{}` (column `{}`)",
                    entity_row.entity, table_rel, table.entity_column
                ),
            },
            format!(
                "report-data entity `{}` did not resolve to a row of `{table_rel}` — abstaining \
                 rather than flagging an identifier-form difference",
                entity_row.entity
            ),
        )
    } else if let Some(bad) = checks.iter().find(|c| c.agrees() == Some(false)) {
        (
            ClaimStatus::Mismatch {
                detail: format!(
                    "report-data.json states {} = {} for `{}` but `{table_rel}` holds {}",
                    bad.column,
                    fmt_trace_number(bad.claimed),
                    entity_row.entity,
                    bad.observed.map(fmt_trace_number).unwrap_or_default(),
                ),
            },
            format!(
                "transcription: report-data {} = {} disagrees with the source cell {} (absolute, \
                 tolerance {})",
                bad.column,
                fmt_trace_number(bad.claimed),
                bad.observed.map(fmt_trace_number).unwrap_or_default(),
                fmt_trace_number(bad.tolerance),
            ),
        )
    } else if let Some(good) = checks.iter().find(|c| c.agrees() == Some(true)) {
        (
            ClaimStatus::Verified,
            format!(
                "transcription: report-data {} = {} agrees with the source cell {} (absolute, \
                 tolerance {})",
                good.column,
                fmt_trace_number(good.claimed),
                good.observed.map(fmt_trace_number).unwrap_or_default(),
                fmt_trace_number(good.tolerance),
            ),
        )
    } else if checks.is_empty() {
        // No numeric slot recorded for this entity: the assertion that remains
        // is PRESENCE in the source artifact, and the row was found.
        (
            ClaimStatus::Verified,
            format!(
                "entity presence: `{}` is a row of `{table_rel}` (column `{}`); report-data \
                 records no numeric value for it",
                entity_row.entity, table.entity_column
            ),
        )
    } else {
        (
            ClaimStatus::Unverifiable {
                reason: format!(
                    "no comparable cell for `{}` in `{table_rel}`",
                    entity_row.entity
                ),
            },
            format!(
                "the source row for `{}` carries no parseable value in the compared column",
                entity_row.entity
            ),
        )
    };

    let class = if checks.is_empty() {
        VerdictClass::EntityPresence
    } else {
        VerdictClass::NumericTable
    };
    let audit = VerdictAudit {
        class,
        source_table: Some(table_rel.to_string()),
        entity_column: Some(table.entity_column.clone()),
        entity_value: Some(entity_row.entity.clone()),
        measurement_column: decisive.map(|c| c.column.clone()),
        claimed_value: decisive.map(|c| c.claimed),
        observed_value: decisive.and_then(|c| c.observed),
        comparison_operator: decisive.map(|_| "absolute".to_string()),
        absolute_tolerance: decisive.map(|c| c.tolerance),
        relative_tolerance: None,
        unit_conversion: None,
        verifier_version: CLAIM_VERIFIER_VERSION.to_string(),
        rationale: Some(rationale),
        parse_coverage: 1.0,
    };

    ClaimVerdict {
        claim,
        status,
        strength: ClaimStrength::Exploratory,
        audit: Some(audit),
    }
}

/// Class-aware + confirmatory-aware task verifier. Picks the
/// `interpretation-policy.<class>.json` overlay,
/// runs the verifier, and then demotes claims whose supporting stage
/// lineage contains a `PostHocDeviation` record.
///
/// Returns a typed [`VerifyOutcome`] so an unreadable/malformed policy
/// (`Unavailable`) is observable and never silently collapses into the same
/// "nothing to verify" branch as an intentionally disabled policy
/// (`Disabled`). A task with no narrative AND no structured claims is
/// reported as `Disabled` (cheap common case), not `Unavailable`.
pub fn verify_task_with_context(
    package_root: &Path,
    task_id: &str,
    config_dir: &Path,
    project_class: ProjectClass,
    decisions: &[DecisionRecord],
    is_confirmatory: bool,
) -> VerifyOutcome {
    verify_task_with_context_deduped(
        package_root,
        task_id,
        config_dir,
        project_class,
        decisions,
        is_confirmatory,
        None,
    )
}

/// [`verify_task_with_context`] with cross-task claim dedupe.
///
/// `ledger` (built once per package by [`CrossTaskClaimLedger::build`]) narrows
/// this task's narrative claim set to the assertions it OWNS, so an assertion
/// repeated verbatim by a later task yields ONE verdict instead of two; the
/// surviving verdict records the other asserting tasks in its audit rationale.
/// `None` reproduces the single-task behaviour exactly (the server's
/// incremental per-task hook has no package-wide view).
pub fn verify_task_with_context_deduped(
    package_root: &Path,
    task_id: &str,
    config_dir: &Path,
    project_class: ProjectClass,
    decisions: &[DecisionRecord],
    is_confirmatory: bool,
    ledger: Option<&CrossTaskClaimLedger>,
) -> VerifyOutcome {
    verify_task_with_context_deduped_cached(
        package_root,
        task_id,
        config_dir,
        project_class,
        decisions,
        is_confirmatory,
        ledger,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn verify_task_with_context_deduped_cached(
    package_root: &Path,
    task_id: &str,
    config_dir: &Path,
    project_class: ProjectClass,
    decisions: &[DecisionRecord],
    is_confirmatory: bool,
    ledger: Option<&CrossTaskClaimLedger>,
    discovery_cache: Option<&mut ClaimDiscoveryCache>,
) -> VerifyOutcome {
    let policy = match load_interpretation_policy(config_dir) {
        PolicyLoad::Loaded(value) => value,
        PolicyLoad::Disabled => return VerifyOutcome::Disabled,
        PolicyLoad::Unavailable { reason } => return VerifyOutcome::Unavailable { reason },
    };
    let cfg = match ExtractorConfig::from_policy_for_class(&policy, config_dir, project_class) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::error!(
                target: "verification",
                config_dir = %config_dir.display(),
                error = %e,
                "interpretation policy parsed but extractor config failed to build — \
                 claim verification is NOT running"
            );
            return VerifyOutcome::Unavailable {
                reason: format!("extractor config: {}", e),
            };
        }
    };
    let cfg = extend_extractor_config_with_result_schemas(package_root, cfg);

    let narrative_path = find_narrative_artifact(package_root, task_id);
    let narrative_text = read_task_narrative(package_root, task_id);
    let has_narrative = narrative_text.is_some();
    let mut report = ClaimVerificationReport::empty();

    // 1. Prose-narrative claims. Prefer a dedicated `.md` / `.txt` artifact;
    // when the task emitted its standard prose in `result.json.summary` or
    // `result.json.narrative`, verify that text instead. Treating the standard
    // result envelope as claim-bearing closes a modality-independent recall
    // gap for compute stages that do not own a separate report file.
    if let Some(narrative) = narrative_text {
        let tables_root = package_root.join("results").join("tables");
        let effective_root = if tables_root.is_dir() {
            tables_root
        } else {
            // Tables may live alongside the narrative in the task
            // runtime directory. Canonical layout is
            // `runtime/outputs/<task_id>/`; legacy used
            // `runtime/<task_id>/`.
            resolve_task_runtime_dir_local(package_root, task_id)
                .unwrap_or_else(|| package_root.join("runtime").join(task_id))
        };
        let mut claims = narrative_claims_from_text(&narrative, &cfg);
        // Cross-task dedupe: keep only the assertions this task OWNS, so a
        // narrative copied verbatim into a later task does not double every
        // verdict (see [`CrossTaskClaimLedger`]).
        if let Some(l) = ledger {
            claims.retain(|c| l.owns(task_id, c));
        }
        let verdicts = match discovery_cache {
            Some(cache) => verify_claims_with_discovery_cached(
                &claims,
                &effective_root,
                package_root,
                &cfg,
                cache,
            ),
            None => verify_claims_with_discovery(&claims, &effective_root, package_root, &cfg),
        };
        for mut v in verdicts {
            if let Some(l) = ledger {
                // Resolve co-asserters before the mutable borrow: the
                // lookup reads `v.claim`, so it cannot overlap `&mut v`.
                let co_asserters = l.co_asserters(task_id, &v.claim);
                note_shared_assertion(&mut v, &co_asserters);
            }
            report.push(v);
        }
        // VF-16: aggregate count sentences ("2209 genes upregulated at
        // FDR<0.05 (Table N)") carry no per-entity Claim, so recompute them
        // from cited or matching emitted evidence and fold the verdicts in.
        // Hedged, rounded, or combined claims remain unverifiable.
        for mut v in verify_narrative_counts_for(
            &narrative,
            &effective_root,
            package_root,
            &cfg,
            Some(task_id),
        ) {
            if let Some(l) = ledger {
                if !l.owns(task_id, &v.claim) {
                    continue;
                }
                let co_asserters = l.co_asserters(task_id, &v.claim);
                note_shared_assertion(&mut v, &co_asserters);
            }
            report.push(v);
        }
    }

    // 2. Structured `result.json` claims (evidence-backed) — verifiable
    //    even when the task wrote no prose narrative at all
    //    (e.g. differential_expression / pathway_enrichment, whose
    //    outputs are tables + a structured claims list).
    let structured = load_structured_claims(package_root, task_id);
    for v in verify_structured_claims(&structured, package_root, &cfg) {
        report.push(v);
    }

    // 2b. (A4) Structured summary counts — `result.json`'s `n_up_fdr05` /
    //     `n_down_fdr05` directional DE split. Nothing else recomputes these,
    //     so an up/down split error slips through. Recompute from
    //     `de_results.tsv` and fold in a real Mismatch on any disagreement.
    for v in crate::claim_verifier::verify_structured_counts(package_root, &cfg) {
        report.push(v);
    }

    // 2c. The CANONICAL significant-entity claim set, derived straight from
    //     `report-data.json` and traced cell-by-cell against the agent's
    //     original result artifact. Emitted by exactly one task per package
    //     (the one that owns `report-data.json`), replacing the circular
    //     row-mining of the marker-delimited block the reports carry.
    for v in report_data_cell_verdicts(package_root, task_id, &cfg) {
        report.push(v);
    }

    // NOTE (C3 reverted): we deliberately do NOT mine raw result-table rows
    // (`de_results.tsv` etc.) as claims. Doing so emits one claim per row and
    // then "verifies" each row against the very table it was read from — a
    // circular self-check that inflates the Verified count by tens of thousands
    // of vacuous entries (e.g. ~17.8k for a per-gene DE table) without adding
    // any narrative-to-evidence signal. The meaningful audit is narrative prose
    // checked against tables; a genuine gene mention in a task's narrative is
    // now resolvable because `de_results.tsv` (entity header `gene_id`) loads
    // via the policy `entityColumns` (Workstream A). Summary numerics that a
    // table-only stage should surface belong in that stage's narrative, not in
    // a row-by-row table mine.

    // Nothing to verify: no narrative AND no structured claims. The policy
    // is enabled and loadable here — this is normally a benign "nothing to
    // do", reported as Disabled rather than Unavailable.
    //
    // EXCEPT when the per-package manifest declares Required expected claims:
    // "said nothing" against a non-empty Required manifest is a RECALL gap,
    // not an empty pass. A bare `Disabled` here would let the downstream
    // finalize Disabled arm no-op — no coverage computed, no signed sink
    // written, no recall-gap block — so the at-rest loader would fall back to
    // the emit-time stub and Inv 1 would Pass. Close that hole: when coverage
    // over the package manifest yields a Required recall gap (absent or
    // unverifiable), fall through to `Verified` carrying the (empty) report.
    // The Verified arm then recomputes coverage, persists the signed sink with
    // the coverage block, and regenerates the audit-proof report so Inv 1
    // (claim_completeness) Fails. Determinism boundary holds:
    // `compute_task_coverage` reads only the package manifest + structured
    // `result.json claims[]`, never the regex/narrative path.
    if !has_narrative && report.n_checked == 0 {
        let recall_gap = compute_task_coverage(package_root, task_id, &cfg)
            .map(|cov| coverage_should_block(&cov))
            .unwrap_or(false);
        if !recall_gap {
            return VerifyOutcome::Disabled;
        }
        // Fall through with the empty report; the Verified arm owns the
        // coverage recompute, signed-sink persist, and recall-gap block.
    }

    demote_claims_from_deviations(&mut report, decisions, is_confirmatory);

    // Locate the agent's runtime decision log if it exists, and attach its
    // package-relative path so the UI can cross-reference the SME-level
    // `decisions.jsonl` against what the agent recorded internally. Convention:
    // the agent writes `runtime/outputs/<task_id>/runtime-decisions.jsonl` (or
    // its legacy sibling `runtime/<task_id>/runtime-decisions.jsonl`). Falls
    // back to `runtime/RUNTIME_DECISION_LOG.jsonl` (package-wide log) if the
    // per-task variant is absent.
    let task_dir = resolve_task_runtime_dir_local(package_root, task_id);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(td) = task_dir {
        candidates.push(td.join("runtime-decisions.jsonl"));
    }
    candidates.push(
        package_root
            .join("runtime")
            .join("RUNTIME_DECISION_LOG.jsonl"),
    );
    for candidate in candidates {
        if candidate.is_file() {
            if let Ok(rel) = candidate.strip_prefix(package_root) {
                report.runtime_decision_log_path = Some(rel.to_string_lossy().into_owned());
                break;
            }
        }
    }

    // For the response's `narrative_path`, fall back to the task's
    // result.json when there was no prose narrative.
    let narrative_path = narrative_path.unwrap_or_else(|| {
        resolve_task_runtime_dir_local(package_root, task_id)
            .map(|d| d.join("result.json"))
            .unwrap_or_else(|| package_root.join("runtime").join(task_id))
    });

    VerifyOutcome::Verified(TaskVerification {
        narrative_path,
        report,
    })
}

/// Result of finalizing one completed task.
pub struct TaskFinalizeOutcome {
    pub outcome: VerifyOutcome,
    pub coverage: Option<CoverageResult>,
}

/// Reconcile evidence entities, regenerate the audit proof, project its
/// verdicts into the RO-Crate descriptor, and re-seal the manifest.
///
/// These operations scan and rewrite package-wide state. They must run once at
/// package convergence, not once per task-completion callback.
fn refresh_package_artifacts(
    root: &Path,
    writer: &crate::audit_writer::AuditWriter,
    context: &str,
) {
    if let Err(e) = crate::ro_crate::finalize_evidence_registration_with_verifier(
        root,
        &WallClock,
        Some(writer),
    ) {
        tracing::warn!(
            target: "ecaa::finalize",
            error = %e,
            finalize_context = context,
            "evidence registration / BagIt manifest reconcile failed"
        );
    }

    let validator = crate::wrroc_validator::NoopWrrocValidator;
    if let Ok(doc) = crate::audit_proof::run_audit_proof_with_verifier(
        root,
        &validator,
        &WallClock,
        Some(writer),
    ) {
        if let Ok(bytes) = serde_json::to_vec_pretty(&doc) {
            let _ = std::fs::write(root.join("runtime/audit-proof-report.json"), bytes);
        }

        let report_value = serde_json::to_value(&doc).unwrap_or(serde_json::Value::Null);
        if let Err(e) = crate::ro_crate::reinject_audit_proof_verdicts(root, &report_value) {
            tracing::warn!(
                target: "ecaa::finalize",
                error = %e,
                finalize_context = context,
                "audit-proof verdict re-injection into descriptor failed"
            );
        } else if let Err(e) = crate::emitter::regenerate_bagit_manifest(root, &WallClock) {
            tracing::warn!(
                target: "ecaa::finalize",
                error = %e,
                finalize_context = context,
                "BagIt manifest re-seal after verdict re-injection failed"
            );
        }
    }
}

/// Verify one completed task FROM SOURCE, persist the HMAC-signed verdict
/// sink, register produced evidence + re-seal the BagIt manifest, and
/// regenerate the at-rest audit-proof report. Pure/sync; no session, no HTTP,
/// no state transition, no telemetry. `secret` signs the verdict sink so the
/// audit-proof loader can read it (de-vacuifies Inv 1/5); pass the same bytes
/// used to later verify with `ecaa-workflow-audit-proof --secret`. `None` ⇒
/// skip the signed sink (Inv 1 stays Unverified).
///
/// The signed-sink persist, evidence registration, manifest re-seal, and
/// audit-proof regeneration run only inside the `Verified` arm and only when a
/// `secret` is supplied — mirroring the server's incremental path. Failures in
/// any of those at-rest writes are best-effort (logged, not propagated) so a
/// finalize hiccup never aborts the run; the recomputed [`VerifyOutcome`] is
/// always returned.
pub fn finalize_task(
    root: &Path,
    task_id: &str,
    config_dir: &Path,
    project_class: ProjectClass,
    decisions: &[DecisionRecord],
    is_confirmatory: bool,
    secret: Option<&[u8; 32]>,
) -> anyhow::Result<TaskFinalizeOutcome> {
    finalize_task_deduped(
        root,
        task_id,
        config_dir,
        project_class,
        decisions,
        is_confirmatory,
        secret,
        None,
    )
}

/// Verify one completed task FROM SOURCE and persist task-scoped verdict,
/// coverage, and repair records without rewriting package-wide artifacts.
///
/// The server completion callback uses this bounded path so it can enforce
/// mismatch and recall gates before acknowledging the event. The harness calls
/// [`finalize_package`] at run convergence to register evidence, regenerate the
/// audit proof, project descriptor verdicts, and re-seal the manifest once.
pub fn finalize_task_verdicts(
    root: &Path,
    task_id: &str,
    config_dir: &Path,
    project_class: ProjectClass,
    decisions: &[DecisionRecord],
    is_confirmatory: bool,
    secret: Option<&[u8; 32]>,
) -> anyhow::Result<TaskFinalizeOutcome> {
    finalize_task_deduped_inner(
        root,
        task_id,
        config_dir,
        project_class,
        decisions,
        is_confirmatory,
        secret,
        None,
        false,
        None,
    )
}

/// [`finalize_task`] with the package-wide [`CrossTaskClaimLedger`] threaded
/// into verification, so an assertion repeated verbatim by several tasks is
/// verified once. `None` reproduces [`finalize_task`] exactly.
#[allow(clippy::too_many_arguments)]
pub fn finalize_task_deduped(
    root: &Path,
    task_id: &str,
    config_dir: &Path,
    project_class: ProjectClass,
    decisions: &[DecisionRecord],
    is_confirmatory: bool,
    secret: Option<&[u8; 32]>,
    ledger: Option<&CrossTaskClaimLedger>,
) -> anyhow::Result<TaskFinalizeOutcome> {
    finalize_task_deduped_inner(
        root,
        task_id,
        config_dir,
        project_class,
        decisions,
        is_confirmatory,
        secret,
        ledger,
        true,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn finalize_task_deduped_inner(
    root: &Path,
    task_id: &str,
    config_dir: &Path,
    project_class: ProjectClass,
    decisions: &[DecisionRecord],
    is_confirmatory: bool,
    secret: Option<&[u8; 32]>,
    ledger: Option<&CrossTaskClaimLedger>,
    refresh_package: bool,
    discovery_cache: Option<&mut ClaimDiscoveryCache>,
) -> anyhow::Result<TaskFinalizeOutcome> {
    let outcome = verify_task_with_context_deduped_cached(
        root,
        task_id,
        config_dir,
        project_class,
        decisions,
        is_confirmatory,
        ledger,
        discovery_cache,
    );

    let mut coverage = None;
    if let VerifyOutcome::Verified(v) = &outcome {
        // Coverage against the injected expected-claim manifest. The
        // ExtractorConfig is rebuilt from the same policy the verify used.
        // `None` when the package carries no manifest (un-anchored task).
        if let PolicyLoad::Loaded(p) = load_interpretation_policy(config_dir) {
            if let Ok(cfg) = ExtractorConfig::from_policy_for_class(&p, config_dir, project_class) {
                let cfg = extend_extractor_config_with_result_schemas(root, cfg);
                coverage = compute_task_coverage(root, task_id, &cfg);
            }
        }

        // Refresh the plaintext operator/UI-visible sidecar
        // (`runtime/claim-verification.json`) so its `n_checked` + `verdicts[]`
        // reflect the recomputed report, aggregated across every finalized task
        // (read-modify-write keyed by `task_id`; idempotent on re-finalize).
        // The signed sink below remains the trust surface; this is the populated
        // human-readable view that was previously left as an empty emit-time
        // stub after a standalone harness run. Best-effort: a write/serialize
        // failure warns and continues — never aborts finalize. Runs regardless
        // of `secret`, since the plaintext carries no HMAC.
        if let Err(e) = crate::claim_sink::refresh_plaintext_sidecar_with_coverage(
            root,
            task_id,
            &v.report,
            coverage.as_ref(),
        ) {
            tracing::warn!(
                target: "ecaa::finalize",
                error = %e,
                task_id,
                "plaintext claim-verification.json refresh failed"
            );
        }

        // Type-aware repair plan (runtime/claim-repair-plan.json): records, per
        // failing claim, the correct repair action (narrative correction /
        // citation fix / evidence completion / review) plus the verifier's
        // detail (which states the table's correct value). Informational +
        // non-destructive — it never rewrites a narrative. A claim-verification
        // failure is a narrative/evidence problem, never a trigger to re-run the
        // analysis (re-execution is the harness's bounded response to
        // analysis-validation failures, a different subsystem). Best-effort.
        if let Err(e) = crate::claim_repair::persist_repair_plan(root, task_id, &v.report) {
            tracing::warn!(
                target: "ecaa::finalize",
                error = %e,
                task_id,
                "claim-repair-plan.json refresh failed"
            );
        }

        // Signed verdict sink (de-vacuifies audit-proof Inv 1/5). The agent's
        // container has already exited; this host-side write is outside any
        // agent-writable window and outside the emit byte-diff baseline
        // (runtime/verification-reports/ is BagIt-excluded). Holds the
        // per-session secret, which the agent never sees, so the sink cannot
        // be forged from the executor side. The coverage block (when present)
        // rides the same signed payload.
        if let Some(sec) = secret {
            let writer = crate::audit_writer::AuditWriter::with_secret(*sec);
            if let Err(e) = crate::claim_sink::persist_signed_verdicts(
                root,
                task_id,
                &v.report,
                coverage.as_ref(),
                &writer,
            ) {
                // Error, not warn: the task verified, so the plaintext sidecar
                // records its verdicts while the signed sink — the artifact
                // audit-proof Invariant 1 reads and a deposit re-verify
                // cross-checks — is now short by this task. A truncated sink
                // still passes its own header-vs-rows consistency check, so
                // nothing downstream notices unless this is loud. The durable
                // marker written by `claim_sink` is what a reader consults; see
                // `claim_sink::unpersisted_tasks`.
                tracing::error!(
                    target: "ecaa::finalize",
                    error = %e,
                    task_id,
                    "signed verdict sink write failed; sink is short by this task"
                );
            }

            if refresh_package {
                refresh_package_artifacts(root, &writer, task_id);
            }
        }
    }
    Ok(TaskFinalizeOutcome { outcome, coverage })
}

/// Summary of finalizing every completed task in a package.
pub struct PackageFinalizeSummary {
    pub tasks_finalized: usize,
    /// One entry per task whose coverage carries a Required recall gap,
    /// formatted `"<task_id>: N absent, M unverifiable"`.
    pub coverage_gaps: Vec<String>,
}

/// Build the package-wide claim-ownership ledger, or `None` when the
/// interpretation policy is disabled/unloadable (in which case nothing is
/// verified anyway and the finalize loop behaves exactly as before).
fn build_cross_task_ledger(
    root: &Path,
    config_dir: &Path,
    project_class: ProjectClass,
    completed: &[String],
) -> Option<CrossTaskClaimLedger> {
    let PolicyLoad::Loaded(policy) = load_interpretation_policy(config_dir) else {
        return None;
    };
    let cfg = ExtractorConfig::from_policy_for_class(&policy, config_dir, project_class).ok()?;
    let cfg = extend_extractor_config_with_result_schemas(root, cfg);
    Some(CrossTaskClaimLedger::build(root, completed, &cfg))
}

/// Finalize every completed task in an emitted package. Reads completed task
/// ids from `WORKFLOW.json`. Intended to be called once at harness end-of-run
/// on both standalone and session-backed paths.
///
/// Cross-task claim dedupe applies here (and only here): a package-wide
/// [`CrossTaskClaimLedger`] prepass assigns each distinct narrative assertion
/// to one owning task, so a `final_reporting` report that restates the
/// `reporting` report contributes no duplicate verdicts.
pub fn finalize_package(
    root: &Path,
    config_dir: &Path,
    project_class: ProjectClass,
    decisions: &[DecisionRecord],
    is_confirmatory: bool,
    secret: Option<&[u8; 32]>,
) -> anyhow::Result<PackageFinalizeSummary> {
    let wf: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("WORKFLOW.json"))?)?;
    let mut summary = PackageFinalizeSummary {
        tasks_finalized: 0,
        coverage_gaps: Vec::new(),
    };
    let mut verified_any = false;
    if let Some(tasks) = wf.get("tasks").and_then(|t| t.as_object()) {
        // `tasks` is a JSON object keyed by task_id; serde_json preserves a
        // BTree-ordered map internally only with the `preserve_order` feature
        // off, so iteration is deterministic by key here.
        let completed: Vec<String> = tasks
            .iter()
            .filter(|(_, t)| {
                t.get("state")
                    .and_then(|s| s.get("status").or(Some(s)))
                    .and_then(|s| s.as_str())
                    == Some("completed")
            })
            .map(|(task_id, _)| task_id.clone())
            .collect();

        // Package-wide claim ownership prepass: a `final_reporting` narrative
        // that restates the `reporting` narrative must yield ONE verdict per
        // assertion, not two. Built over the same deterministic task order the
        // finalize loop uses, so ownership is reproducible.
        let ledger = build_cross_task_ledger(root, config_dir, project_class, &completed);
        // All completed tasks read the same immutable result tables. Reuse one
        // package-scoped parse/index cache across their verification calls;
        // the cache is dropped at function exit and can never leak state into
        // another package or a later server request.
        let mut discovery_cache = ClaimDiscoveryCache::default();

        for task_id in &completed {
            let res = finalize_task_deduped_inner(
                root,
                task_id,
                config_dir,
                project_class,
                decisions,
                is_confirmatory,
                secret,
                ledger.as_ref(),
                false,
                Some(&mut discovery_cache),
            )?;
            verified_any |= matches!(&res.outcome, VerifyOutcome::Verified(_));
            summary.tasks_finalized += 1;
            if let Some(cov) = res.coverage {
                if coverage_should_block(&cov) {
                    summary.coverage_gaps.push(format!(
                        "{task_id}: {} absent, {} unverifiable",
                        cov.required_absent, cov.required_unverifiable
                    ));
                }
            }
        }
    }

    if verified_any {
        if let Some(sec) = secret {
            let writer = crate::audit_writer::AuditWriter::with_secret(*sec);
            refresh_package_artifacts(root, &writer, "package");
        }
    }

    // Observability (E1): a package that finalized zero tasks is almost always
    // emit-only / never executed (no task reached `completed`), NOT a
    // verification failure. Distinguish the two so an emit-only
    // `~/.ecaa-workflow` package's `claim-verification.json n_checked:0` is not
    // mistaken for — or quoted as — a real "0 claims" result.
    if summary.tasks_finalized == 0 {
        let total_tasks = wf
            .get("tasks")
            .and_then(|t| t.as_object())
            .map(|o| o.len())
            .unwrap_or(0);
        tracing::warn!(
            target: "ecaa::finalize",
            total_tasks,
            "finalize_package finalized 0 tasks: package appears emit-only / not executed — \
             claim-verification.json n_checked:0 reflects 'not run', not a verification failure"
        );
    }

    Ok(summary)
}

/// Load a task's structured claims from `result.json`'s `claims` array.
/// Returns an empty vec when the file is missing, unparsable, or has no
/// `claims` field — structured claims are optional, not an error.
fn load_structured_claims(package_root: &Path, task_id: &str) -> Vec<StructuredClaim> {
    let Some(dir) = resolve_task_runtime_dir_local(package_root, task_id) else {
        return Vec::new();
    };
    let path = dir.join("result.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    value
        .get("claims")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| serde_json::from_value::<StructuredClaim>(v.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// True when a coverage result has any Required recall gap (absent or
/// unverifiable). Drives the on-completion `ValidationFailed` block for
/// Required+Unverifiable (and Required-absent).
pub fn coverage_should_block(cov: &CoverageResult) -> bool {
    cov.required_absent > 0 || cov.required_unverifiable > 0
}

/// Compute the structured-claims-only CoverageResult for a task. Reads the
/// emitted `policies/interpretation-policy.json`'s `verifiableEntities.
/// expected` block (the manifest the emitter injected), narrows it to entries
/// relevant to the current task/source atom, reconciles it against the task's
/// structured `result.json claims[]` verdicts, and returns the coverage.
/// `None` when no manifest is present or no manifest entry belongs to this
/// task.
fn compute_task_coverage(
    package_root: &Path,
    task_id: &str,
    cfg: &ExtractorConfig,
) -> Option<CoverageResult> {
    // Read the injected manifest from the per-package policy.
    let policy_path = package_root.join("policies/interpretation-policy.json");
    let raw = std::fs::read_to_string(&policy_path).ok()?;
    let policy: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let expected = policy
        .get("verifiableEntities")
        .and_then(|v| v.get("expected"))
        .cloned()?;
    let mut entries: Vec<crate::expected_claim::ExpectedClaim> =
        serde_json::from_value(expected).ok()?;
    let task_stems = task_expected_claim_stems(package_root, task_id);
    entries.retain(|entry| expected_claim_matches_task(entry, &task_stems));
    if entries.is_empty() {
        return None;
    }
    let manifest = crate::expected_claim::ExpectedClaimManifest {
        schema_version: "1".into(),
        entries,
    };
    // Structured claims ONLY — never the regex/narrative path.
    let structured = load_structured_claims(package_root, task_id);
    let mut verdicts = verify_structured_claims(&structured, package_root, cfg);
    // Scoped reconcile: this caller has already restricted both the manifest
    // entries and the verdicts to `task_id`, so the provenance arm can credit a
    // Verified result-table claim to this stage's Required entry even when the
    // agent's table basename differs from the auto-generated atom-id stem.
    let mut cov = crate::coverage::reconcile_coverage_scoped(&manifest, &verdicts, &task_stems);

    // Harness derivation (recall floor of last resort). If a Required entry is
    // STILL Absent — the agent emitted no structured claim addressing this
    // stage, the way pathway_enrichment shipped an empty `claims[]` — but the
    // stage DID produce a recomputable significant-count table, synthesize ONE
    // count claim and RECOMPUTE it from that table. The agent's own scalar is
    // never trusted: the claim is re-derived and re-verified, so a stage that
    // fabricated or omitted a count cannot launder the floor shut — a recompute
    // mismatch yields no Verified claim and the gap stays an honest recall gap.
    if cov.required_absent > 0 {
        let derived =
            synthesize_declared_stage_count_claim(package_root, task_id).map(|(claim, schema)| {
                let mut schema_cfg = cfg.clone();
                if !schema_cfg.entity_columns.contains(&schema.entity_column) {
                    schema_cfg.entity_columns.push(schema.entity_column);
                }
                if let Some(effect) = schema.signed_effect_column {
                    if !schema_cfg.effect_size_columns.contains(&effect) {
                        schema_cfg.effect_size_columns.push(effect);
                    }
                }
                if let Some(significance) = schema.significance {
                    if !schema_cfg.pvalue_columns.contains(&significance.column) {
                        schema_cfg.pvalue_columns.push(significance.column);
                    }
                }
                (claim, schema_cfg)
            });
        if let Some((derived, derived_cfg)) = derived {
            let dv = verify_structured_claims(
                std::slice::from_ref(&derived),
                package_root,
                &derived_cfg,
            );
            if dv
                .iter()
                .any(|v| matches!(v.status, crate::claim_verifier::ClaimStatus::Verified))
            {
                verdicts.extend(dv);
                cov = crate::coverage::reconcile_coverage_scoped(&manifest, &verdicts, &task_stems);
            }
        }
    }
    Some(cov)
}

/// Build one significant-count claim directly from the terminal atom's declared
/// [`crate::report_contract::ResultSchema`] and its primary result artifact.
///
/// The result schema is the executable contract for the table: it names the
/// artifact, significance column, comparator, and threshold. Recomputing the
/// total and significant counts from that table avoids coupling the recall
/// floor to an agent-authored `result.json` key vocabulary (`n_selected`,
/// `qualifying_record_count`, and similar names can describe the same fact).
/// The resulting structured claim is still passed through
/// [`verify_structured_claims`], so the claim cannot close coverage unless the
/// cited table independently supports it.
fn synthesize_declared_stage_count_claim(
    package_root: &Path,
    task_id: &str,
) -> Option<(StructuredClaim, crate::report_contract::ResultSchema)> {
    let schema = read_task_result_schemas(package_root).remove(task_id)?;
    let significance = schema.significance.as_ref()?;
    if !significance.threshold.is_finite() {
        return None;
    }

    let dir = resolve_task_runtime_dir_local(package_root, task_id)?;
    let table = dir.join(&schema.artifact);
    let (headers, rows) = crate::report_contract::assemble::read_table(&table).ok()?;
    let synonyms = crate::report_contract::load_policy_column_synonyms(package_root);
    // Bind the synthesized assertion to the PHYSICAL column that the same
    // schema+policy resolver selected for the recomputation. A declared role
    // may be logical (`padj`, `pathway`) while the retained table uses policy
    // synonyms (`adj_p_value`, `term`). Quoting the logical name in an exact
    // threshold assertion made the verifier correctly refuse a column that did
    // not exist, leaving an otherwise recomputable stage falsely absent from
    // coverage. Resolving once here keeps the assertion and recomputation on
    // the identical cell family for every schema and modality.
    let resolved = crate::report_contract::resolve_ranking_columns(&headers, &schema, &synonyms)?;
    let significance_column = resolved
        .significance
        .and_then(|index| headers.get(index))?
        .to_string();
    let stats = crate::report_contract::summarize_artifact(&rows, &headers, &schema, &synonyms);
    let count = stats.n_significant?;
    let total = stats.n_total;
    if total == 0 {
        return None;
    }

    let rel = table
        .strip_prefix(package_root)
        .ok()?
        .to_string_lossy()
        .into_owned();
    let comparator = match significance.comparator {
        crate::report_contract::Comparator::Lt => "<",
        crate::report_contract::Comparator::Gt => ">",
    };

    let claim = StructuredClaim {
        claim: format!(
            "{count} of {total} entities significant at `{}` {comparator} {}",
            significance_column, significance.threshold
        ),
        evidence: Some(rel),
    };
    Some((claim, schema))
}

fn expected_claim_matches_task(
    entry: &crate::expected_claim::ExpectedClaim,
    task_stems: &std::collections::BTreeSet<String>,
) -> bool {
    if task_stems.contains(&expected_claim_stem(&entry.entity)) {
        return true;
    }
    entry
        .expected_output_table
        .as_deref()
        .map(expected_claim_stem)
        .is_some_and(|stem| task_stems.contains(&stem))
}

fn task_expected_claim_stems(
    package_root: &Path,
    task_id: &str,
) -> std::collections::BTreeSet<String> {
    let mut stems = std::collections::BTreeSet::from([expected_claim_stem(task_id)]);
    if let Some(dir) = resolve_task_runtime_dir_local(package_root, task_id) {
        let path = dir.join("task-spec.json");
        if let Ok(raw) = std::fs::read_to_string(path) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
                if let Some(source_atom_id) = value.get("source_atom_id").and_then(|v| v.as_str()) {
                    stems.insert(expected_claim_stem(source_atom_id));
                }
            }
        }
    }
    stems
}

fn expected_claim_stem(token: &str) -> String {
    let mut base = token
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(token)
        .to_ascii_lowercase();
    for suffix in [".gz", ".bz2", ".xz", ".zst", ".zip"] {
        if let Some(stripped) = base.strip_suffix(suffix) {
            base = stripped.to_string();
            break;
        }
    }
    match base.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => stem.to_string(),
        _ => base,
    }
}

// Canonical task-outputs layout is `runtime/outputs/<task_id>/`; legacy
// (pre-harness-canonicalization) packages used `runtime/<task_id>/`.
// Return whichever exists, preferring the canonical layout.
fn resolve_task_runtime_dir_local(package_root: &Path, task_id: &str) -> Option<PathBuf> {
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

fn find_narrative_artifact(package_root: &Path, task_id: &str) -> Option<PathBuf> {
    let runtime_dir = resolve_task_runtime_dir_local(package_root, task_id)?;
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
    // Prefer files named with "report", "interpretation", or "summary" —
    // those are the conventional narrative outputs.
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

/// Three-state load result for the interpretation policy so callers can
/// distinguish *intentionally disabled* from *broken configuration*.
///
/// - `Disabled` — file present + parsed, but no `verifiableEntities.enabled`
///   → verification is off by design; return the benign "disabled" response.
/// - `Loaded` — file present + parsed + `verifiableEntities.enabled: true`.
/// - `Unavailable` — file absent, unreadable, or malformed JSON. This is a
///   configuration defect (wrong CWD / `ECAA_CONFIG_DIR`), NOT a legitimate
///   "nothing to verify". Callers must surface it loudly rather than silently
///   returning a clean 200.
#[derive(Debug)]
pub enum PolicyLoad {
    Disabled,
    Loaded(serde_json::Value),
    Unavailable { reason: String },
}

fn load_interpretation_policy(config_dir: &Path) -> PolicyLoad {
    // Resolve the base policy with the downstream-policy-first / flat-fallback
    // precedence so a self-contained emitted package (policy files copied FLAT
    // under `<root>/policies/`) finalizes without a repo `config/` reachable,
    // while a repo `config/` (the server) resolves exactly as before.
    let Some(path) =
        crate::claim_extractor::resolve_policy_file(config_dir, "interpretation-policy.json")
    else {
        let attempted = config_dir
            .join("downstream-policy")
            .join("interpretation-policy.json");
        tracing::error!(
            target: "verification",
            config_dir = %config_dir.display(),
            "interpretation-policy.json not found under downstream-policy/ or flat — \
             claim verification is NOT running; check ECAA_CONFIG_DIR / working directory"
        );
        return PolicyLoad::Unavailable {
            reason: format!("read {}: not found", attempted.display()),
        };
    };
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(
                target: "verification",
                policy_path = %path.display(),
                error = %e,
                "interpretation-policy.json unreadable — claim verification is NOT running; \
                 check ECAA_CONFIG_DIR / working directory"
            );
            return PolicyLoad::Unavailable {
                reason: format!("read {}: {}", path.display(), e),
            };
        }
    };
    let value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                target: "verification",
                policy_path = %path.display(),
                error = %e,
                "interpretation-policy.json is malformed — claim verification is NOT running"
            );
            return PolicyLoad::Unavailable {
                reason: format!("parse {}: {}", path.display(), e),
            };
        }
    };
    let enabled = value
        .get("verifiableEntities")
        .and_then(|v| v.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if enabled {
        PolicyLoad::Loaded(value)
    } else {
        PolicyLoad::Disabled
    }
}

/// True when the interpretation policy at `config_dir` is present,
/// parseable, and has `verifiableEntities.enabled: true`.
pub fn default_policy_is_loadable(config_dir: &Path) -> bool {
    matches!(
        load_interpretation_policy(config_dir),
        PolicyLoad::Loaded(_)
    )
}

/// Boot-time check: emit a loud error + telemetry signal (no panic) when
/// the default policy is unavailable, so a CWD/`ECAA_CONFIG_DIR`
/// misconfiguration is visible rather than silently disabling verification
/// fleet-wide. Deliberately does NOT panic — host-mode / no-LLM deployments
/// may legitimately run without claim verification.
pub fn assert_default_policy_present(config_dir: &Path) {
    match load_interpretation_policy(config_dir) {
        PolicyLoad::Loaded(_) => {}
        PolicyLoad::Disabled => tracing::warn!(
            target: "verification",
            config_dir = %config_dir.display(),
            "interpretation policy present but verifiableEntities disabled — claim verification off by config"
        ),
        PolicyLoad::Unavailable { reason } => tracing::error!(
            target: "verification",
            config_dir = %config_dir.display(),
            %reason,
            "DEFAULT interpretation policy UNAVAILABLE at boot — claim verification will not run fleet-wide"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn declared_schema_synthesizes_counts_for_unrelated_modalities_and_comparators() {
        let cases = [
            (
                "feature_association",
                serde_json::json!({
                    "artifact": "association_estimates.tsv",
                    "entity_column": "analyte",
                    "signed_effect_column": "standardized_effect",
                    "significance": {
                        "column": "false_discovery_rate",
                        "comparator": "lt",
                        "threshold": 0.05
                    }
                }),
                "association_estimates.tsv",
                "analyte\tstandardized_effect\tfalse_discovery_rate\nA\t1\t0.01\nB\t-1\t0.04\nC\t0\t0.2\nD\t0\tNA\n",
                serde_json::json!({"qualifying_record_count": 999}),
                "2 of 4 entities significant at `false_discovery_rate` < 0.05",
            ),
            (
                "anomaly_screen",
                serde_json::json!({
                    "artifact": "anomaly_scores.tsv",
                    "entity_column": "event_id",
                    "significance": {
                        "column": "anomaly_score",
                        "comparator": "gt",
                        "threshold": 2.5
                    }
                }),
                "anomaly_scores.tsv",
                "event_id\tanomaly_score\nE1\t0.5\nE2\t2.6\nE3\t8.0\nE4\t2.5\n",
                serde_json::json!({"n_selected": 999, "threshold": 999}),
                "2 of 4 entities significant at `anomaly_score` > 2.5",
            ),
        ];

        for (task_id, schema, table_name, table, misleading_result, expected_claim) in cases {
            let tmp = tempdir().unwrap();
            let dir = tmp.path().join("runtime/outputs").join(task_id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(
                dir.join("result.json"),
                serde_json::to_vec_pretty(&misleading_result).unwrap(),
            )
            .unwrap();
            fs::write(dir.join(table_name), table).unwrap();
            write_task_nodes(tmp.path(), &[(task_id, schema)]);

            let (claim, schema) = synthesize_declared_stage_count_claim(tmp.path(), task_id)
                .unwrap_or_else(|| panic!("{task_id}: declared count claim missing"));
            assert_eq!(claim.claim, expected_claim);
            assert_eq!(
                claim.evidence.as_deref(),
                Some(format!("runtime/outputs/{task_id}/{table_name}").as_str())
            );
            let mut cfg = test_cfg();
            cfg.entity_columns.push(schema.entity_column);
            if let Some(significance) = schema.significance {
                cfg.pvalue_columns.push(significance.column);
            }
            let verdicts = verify_structured_claims(std::slice::from_ref(&claim), tmp.path(), &cfg);
            assert!(
                matches!(
                    verdicts[0].status,
                    crate::claim_verifier::ClaimStatus::Verified
                ),
                "{task_id}: {:?}",
                verdicts[0].status
            );
        }
    }

    #[test]
    fn compute_task_coverage_uses_declared_schema_for_unrelated_modalities() {
        let cases = [
            (
                "feature_association",
                serde_json::json!({
                    "artifact": "association_estimates.tsv",
                    "entity_column": "analyte",
                    "signed_effect_column": "standardized_effect",
                    "significance": {
                        "column": "false_discovery_rate",
                        "comparator": "lt",
                        "threshold": 0.05
                    }
                }),
                "association_estimates.tsv",
                "analyte\tstandardized_effect\tfalse_discovery_rate\n\
                 A\t1\t0.01\nB\t-1\t0.04\nC\t0\t0.2\nD\t0\tNA\n",
            ),
            (
                "anomaly_screen",
                serde_json::json!({
                    "artifact": "anomaly_scores.tsv",
                    "entity_column": "event_id",
                    "significance": {
                        "column": "anomaly_score",
                        "comparator": "gt",
                        "threshold": 2.5
                    }
                }),
                "anomaly_scores.tsv",
                "event_id\tanomaly_score\nE1\t0.5\nE2\t2.6\nE3\t8.0\nE4\t2.5\n",
            ),
        ];

        for (task_id, schema, table_name, table) in cases {
            let tmp = tempdir().unwrap();
            let policy_dir = tmp.path().join("policies");
            fs::create_dir_all(&policy_dir).unwrap();
            fs::write(
                policy_dir.join("interpretation-policy.json"),
                serde_json::to_vec_pretty(&serde_json::json!({
                    "verifiableEntities": {
                        "enabled": true,
                        "expected": [{
                            "entity": task_id,
                            "expected_output_table": task_id,
                            "requirement": "required"
                        }]
                    }
                }))
                .unwrap(),
            )
            .unwrap();
            let output_dir = tmp.path().join("runtime/outputs").join(task_id);
            fs::create_dir_all(&output_dir).unwrap();
            fs::write(
                output_dir.join("result.json"),
                serde_json::to_vec_pretty(&serde_json::json!({"task_id": task_id, "claims": []}))
                    .unwrap(),
            )
            .unwrap();
            fs::write(output_dir.join(table_name), table).unwrap();
            write_task_nodes(tmp.path(), &[(task_id, schema)]);

            let coverage = compute_task_coverage(tmp.path(), task_id, &test_cfg())
                .unwrap_or_else(|| panic!("{task_id} coverage was not computed"));
            assert_eq!(
                (coverage.required_total, coverage.required_addressed),
                (1, 1),
                "{task_id}: {coverage:?}"
            );
            assert_eq!(coverage.required_absent, 0, "{task_id}: {coverage:?}");
            assert_eq!(coverage.required_unverifiable, 0, "{task_id}: {coverage:?}");
        }
    }

    #[test]
    fn compute_task_coverage_resolves_declared_roles_through_policy_synonyms() {
        let tmp = tempdir().unwrap();
        let task_id = "set_enrichment";
        let policy_dir = tmp.path().join("policies");
        fs::create_dir_all(&policy_dir).unwrap();
        fs::write(
            policy_dir.join("interpretation-policy.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "verifiableEntities": {
                    "enabled": true,
                    "entityColumns": ["feature", "term", "pathway"],
                    "effectSizeColumns": ["effect", "NES"],
                    "pvalueColumns": ["p_value", "padj", "adj_p_value"],
                    "expected": [{
                        "entity": task_id,
                        "expected_output_table": task_id,
                        "requirement": "required"
                    }]
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let output_dir = tmp.path().join("runtime/outputs").join(task_id);
        fs::create_dir_all(&output_dir).unwrap();
        fs::write(
            output_dir.join("result.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "task_id": task_id,
                "claims": [],
                "n_significant": 2,
                "n_total_tested": 4
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            output_dir.join("set_results.tsv"),
            "term\tcollection\tNES\tp_value\tadj_p_value\n\
             S1\tA\t1.2\t0.001\t0.01\n\
             S2\tA\t-1.1\t0.002\t0.20\n\
             S3\tB\t0.3\t0.003\t0.25\n\
             S4\tB\t0.1\t0.004\t0.80\n",
        )
        .unwrap();
        write_task_nodes(
            tmp.path(),
            &[(
                task_id,
                serde_json::json!({
                    "artifact": "set_results.tsv",
                    "entity_column": "pathway",
                    "grouping_column": "collection",
                    "signed_effect_column": "NES",
                    "significance": {
                        "column": "padj",
                        "comparator": "lt",
                        "threshold": 0.25
                    }
                }),
            )],
        );

        let mut cfg = test_cfg();
        cfg.pvalue_columns.push("adj_p_value".into());
        let coverage = compute_task_coverage(tmp.path(), task_id, &cfg)
            .expect("declared stage coverage must be computed");
        assert_eq!(
            (coverage.required_total, coverage.required_addressed),
            (1, 1),
            "{coverage:?}"
        );
        assert_eq!(coverage.required_absent, 0, "{coverage:?}");
        assert_eq!(coverage.required_unverifiable, 0, "{coverage:?}");
    }

    #[test]
    fn coverage_does_not_guess_a_count_without_a_declared_result_schema() {
        let tmp = tempdir().unwrap();
        let task_id = "untyped_screen";
        let policy_dir = tmp.path().join("policies");
        fs::create_dir_all(&policy_dir).unwrap();
        fs::write(
            policy_dir.join("interpretation-policy.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "verifiableEntities": {
                    "enabled": true,
                    "expected": [{
                        "entity": task_id,
                        "expected_output_table": task_id,
                        "requirement": "required"
                    }]
                }
            }))
            .unwrap(),
        )
        .unwrap();
        let dir = tmp.path().join("runtime/outputs").join(task_id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("result.json"),
            r#"{"task_id":"untyped_screen","claims":[],"n_selected":2}"#,
        )
        .unwrap();
        fs::write(
            dir.join("scores.tsv"),
            "entity\tuntyped_score\nA\t1\nB\t2\n",
        )
        .unwrap();

        let coverage = compute_task_coverage(tmp.path(), task_id, &test_cfg()).unwrap();
        assert_eq!(coverage.required_addressed, 0, "{coverage:?}");
        assert_eq!(coverage.required_absent, 1, "{coverage:?}");
    }

    #[test]
    fn package_verification_uses_unseen_schema_roles_without_modality_branches() {
        let pkg = tempdir().unwrap();
        let config = tempdir().unwrap();
        scaffold_config_dir(config.path());

        let schema = serde_json::json!({
            "artifact": "risk_scores.tsv",
            "entity_column": "record_key",
            "entity_column_aliases": ["record_id"],
            "signed_effect_column": "impact_delta",
            "signed_effect_aliases": ["delta"],
            "significance": {
                "column": "alert_score",
                "comparator": "gt",
                "threshold": 7.5
            }
        });
        write_task_nodes(pkg.path(), &[("risk_screen", schema)]);
        let reporting = pkg.path().join("runtime/outputs/reporting");
        fs::create_dir_all(&reporting).unwrap();
        fs::write(
            reporting.join("report.md"),
            "Of 4 records tested, 2 were statistically significant at \
             `alert_score` > 7.5 (Table risk_scores.tsv).\n",
        )
        .unwrap();
        let tables = pkg.path().join("results/tables");
        fs::create_dir_all(&tables).unwrap();
        fs::write(
            tables.join("risk_scores.tsv"),
            "record_key\timpact_delta\talert_score\n\
             R1\t1.2\t9.0\nR2\t-0.4\t7.6\nR3\t0.1\t7.5\nR4\t0.0\t2.0\n",
        )
        .unwrap();

        let base = test_cfg();
        assert!(!base.entity_columns.iter().any(|c| c == "record_key"));
        let extended = extend_extractor_config_with_result_schemas(pkg.path(), base);
        for (actual, expected) in [
            (&extended.entity_columns, "record_key"),
            (&extended.entity_columns, "record_id"),
            (&extended.effect_size_columns, "impact_delta"),
            (&extended.effect_size_columns, "delta"),
            (&extended.pvalue_columns, "alert_score"),
        ] {
            assert!(
                actual.iter().any(|column| column == expected),
                "declared role `{expected}` was not added: {actual:?}"
            );
        }

        let verified = expect_verified(verify_task_with_context(
            pkg.path(),
            "reporting",
            config.path(),
            ProjectClass::TimeSeriesForecast,
            &[],
            false,
        ));
        let counts: Vec<_> = verified
            .report
            .verdicts
            .iter()
            .filter(|verdict| verdict.claim.entity.starts_with("count:"))
            .collect();
        assert_eq!(counts.len(), 2, "{:#?}", verified.report.verdicts);
        assert!(
            counts
                .iter()
                .all(|verdict| matches!(verdict.status, ClaimStatus::Verified)),
            "{counts:#?}"
        );
    }

    /// Minimal enabled policy for the claim-extraction helpers under test.
    fn test_cfg() -> ExtractorConfig {
        let policy = serde_json::json!({
            "verifiableEntities": {
                "enabled": true,
                "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}"],
                "directionVocab": {
                    "up": ["upregulated"],
                    "down": ["downregulated"]
                },
                "effectSizeColumns": ["log2FC"],
                "entityColumns": ["gene", "term", "pathway", "analyte", "event_id", "compound"],
                "pvalueColumns": ["padj"]
            }
        });
        ExtractorConfig::from_policy(&policy).expect("test policy loads")
    }

    /// A `final_reporting` narrative that restates the `reporting` narrative
    /// verbatim must yield ONE verdict, owned by the first task, with the other
    /// asserting task recorded rather than dropped silently.
    #[test]
    fn identical_claim_in_two_tasks_is_deduped_once() {
        let cfg = test_cfg();
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let narrative = "ACAN was upregulated (log2FC=2.1, Table S1).\n";
        for task in ["reporting", "final_reporting"] {
            let dir = root.join("runtime/outputs").join(task);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("report.md"), narrative).unwrap();
        }

        // Deterministic task order = the WORKFLOW.json key order.
        let tasks = vec!["final_reporting".to_string(), "reporting".to_string()];
        let ledger = CrossTaskClaimLedger::build(root, &tasks, &cfg);
        assert_eq!(
            ledger.len(),
            1,
            "the two tasks assert exactly one distinct claim between them"
        );

        let claims = narrative_claims_from_text(narrative, &cfg);
        let claim = claims
            .iter()
            .find(|c| c.entity == "ACAN")
            .expect("ACAN claim extracted");
        assert!(ledger.owns("final_reporting", claim), "first task owns it");
        assert!(
            !ledger.owns("reporting", claim),
            "the repeating task must not re-verify the same assertion"
        );
        assert_eq!(
            ledger.co_asserters("final_reporting", claim),
            vec!["reporting".to_string()],
        );

        // The surviving verdict records who else asserted it.
        let mut verdict = ClaimVerdict {
            claim: claim.clone(),
            status: ClaimStatus::Verified,
            strength: ClaimStrength::Exploratory,
            audit: None,
        };
        note_shared_assertion(&mut verdict, &ledger.co_asserters("final_reporting", claim));
        let rationale = verdict
            .audit
            .as_ref()
            .and_then(|a| a.rationale.clone())
            .expect("audit rationale records the co-asserter");
        assert!(
            rationale.contains("reporting"),
            "co-asserting task must be named: {rationale}"
        );

        // A claim the ledger never saw is kept (never silently dropped).
        let unseen = Claim {
            excerpt: "COL2A1 was downregulated (log2FC=-1.0, Table S1).".into(),
            ..claim.clone()
        };
        assert!(ledger.owns("reporting", &unseen));
    }

    #[test]
    fn identical_aggregate_count_in_two_tasks_is_deduped_once() {
        let cfg = test_cfg();
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let narrative = "Of 4 plasma compounds tested, 2 were significant at `risk_score` > 2.5.\n";
        for task in ["reporting", "final_reporting"] {
            let dir = root.join("runtime/outputs").join(task);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("report.md"), narrative).unwrap();
        }
        let result_dir = root.join("runtime/outputs/risk_screen");
        fs::create_dir_all(&result_dir).unwrap();
        fs::write(
            result_dir.join("risk_scores.tsv"),
            "compound\trisk_score\nA\t0.2\nB\t2.6\nC\t9.1\nD\t2.5\n",
        )
        .unwrap();

        let tasks = vec!["reporting".to_string(), "final_reporting".to_string()];
        let ledger = CrossTaskClaimLedger::build(root, &tasks, &cfg);
        let count_verdicts =
            verify_narrative_counts_for(narrative, &result_dir, root, &cfg, None);
        assert_eq!(count_verdicts.len(), 2, "{count_verdicts:#?}");
        assert_eq!(
            ledger.len(),
            1,
            "tested and significant facts share one sentence-level assertion key"
        );
        for verdict in &count_verdicts {
            assert!(ledger.owns("reporting", &verdict.claim));
            assert!(!ledger.owns("final_reporting", &verdict.claim));
            assert_eq!(
                ledger.co_asserters("reporting", &verdict.claim),
                vec!["final_reporting".to_string()]
            );
        }
    }

    /// The canonical claim set is derived from `report-data.json` and checked
    /// against the agent's ORIGINAL result artifact, so every verdict carries a
    /// full cell-level trace. It is emitted by exactly one task per package.
    #[test]
    fn report_data_derived_claims_carry_cell_level_trace() {
        use crate::report_contract::{
            EntityRow, LiteratureStatus, ReportData, ResultArtifactSummary,
        };

        let cfg = test_cfg();
        let tmp = tempdir().unwrap();
        let root = tmp.path();

        let de_dir = root.join("runtime/outputs/differential_expression");
        fs::create_dir_all(&de_dir).unwrap();
        fs::write(
            de_dir.join("de_results.tsv"),
            "gene\tlog2FC\tpadj\n\
             ACAN\t2.1\t0.001\n\
             COL2A1\t-1.5\t7.056e-132\n\
             MYOD1\t0.5\t0.04\n",
        )
        .unwrap();

        let entity = |name: &str, effect: f64, sig: f64| EntityRow {
            entity: name.into(),
            effect: Some(effect),
            significance: Some(sig),
            literature: LiteratureStatus::Novel,
        };
        let report_data = ReportData {
            artifacts: vec![ResultArtifactSummary {
                stage_id: "differential_expression".into(),
                artifact: "de_results.tsv".into(),
                result_schema: None,
                n_total: 3,
                n_significant: Some(3),
                direction_split: None,
                effect_distribution: None,
                grouped_significant: None,
                ranking: None,
                significant_entities: vec![
                    entity("ACAN", 2.1, 0.001),
                    entity("COL2A1", -1.5, 7.056e-132),
                    // Disagrees with the table cell (0.5) beyond tolerance.
                    entity("MYOD1", 9.9, 0.04),
                ],
                significant_table_path:
                    "runtime/outputs/differential_expression/de_results.significant.tsv".into(),
                full_table_path: "runtime/outputs/differential_expression/de_results.full.tsv"
                    .into(),
                spilled_to_attachment_only: false,
            }],
            literature: None,
        };
        let reporting = root.join("runtime/outputs/reporting");
        fs::create_dir_all(&reporting).unwrap();
        fs::write(
            reporting.join("report-data.json"),
            serde_json::to_string_pretty(&report_data).unwrap(),
        )
        .unwrap();

        let verdicts = report_data_cell_verdicts(root, "reporting", &cfg);
        assert_eq!(
            verdicts.len(),
            3,
            "one canonical claim per significant entity"
        );

        let acan = verdicts
            .iter()
            .find(|v| v.claim.entity == "ACAN")
            .expect("ACAN verdict");
        assert!(matches!(acan.status, ClaimStatus::Verified));
        let audit = acan.audit.as_ref().expect("cell-level trace");
        assert_eq!(
            audit.source_table.as_deref(),
            Some("runtime/outputs/differential_expression/de_results.tsv"),
            "the trace cites the agent's ORIGINAL artifact, not the rendered block"
        );
        assert_eq!(audit.entity_column.as_deref(), Some("gene"));
        assert_eq!(audit.measurement_column.as_deref(), Some("log2FC"));
        assert_eq!(audit.claimed_value, Some(2.1));
        assert_eq!(audit.observed_value, Some(2.1));
        assert_eq!(audit.comparison_operator.as_deref(), Some("absolute"));
        assert!(audit.absolute_tolerance.is_some());
        assert!(audit.rationale.is_some());

        let myod1 = verdicts
            .iter()
            .find(|v| v.claim.entity == "MYOD1")
            .expect("MYOD1 verdict");
        assert!(
            matches!(myod1.status, ClaimStatus::Mismatch { .. }),
            "a report-data value disagreeing with its source cell is a Mismatch, got {:?}",
            myod1.status
        );
        let myod1_audit = myod1.audit.as_ref().unwrap();
        assert_eq!(myod1_audit.claimed_value, Some(9.9));
        assert_eq!(myod1_audit.observed_value, Some(0.5));

        // Every verdict carries the full tuple.
        for v in &verdicts {
            let a = v.audit.as_ref().expect("audit present");
            assert!(a.source_table.is_some());
            assert!(a.measurement_column.is_some());
            assert!(a.claimed_value.is_some());
            assert!(a.comparison_operator.is_some());
            assert!(a.absolute_tolerance.is_some());
        }

        // Emitted by exactly ONE task: a sibling reporting task that does not
        // own `report-data.json` contributes nothing.
        let final_dir = root.join("runtime/outputs/final_reporting");
        fs::create_dir_all(&final_dir).unwrap();
        fs::write(final_dir.join("final_report.md"), "# Final\n").unwrap();
        assert!(report_data_cell_verdicts(root, "final_reporting", &cfg).is_empty());
    }

    /// Abstain-first: when the assembler keyed on an identifier form absent
    /// from the resolved entity column, the artifact is skipped wholesale
    /// rather than emitting one Mismatch per entity.
    #[test]
    fn report_data_claims_abstain_when_no_entity_resolves() {
        use crate::report_contract::{
            EntityRow, LiteratureStatus, ReportData, ResultArtifactSummary,
        };

        let cfg = test_cfg();
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let de_dir = root.join("runtime/outputs/differential_expression");
        fs::create_dir_all(&de_dir).unwrap();
        fs::write(
            de_dir.join("de_results.tsv"),
            "gene\tlog2FC\tpadj\nENSG00000000001\t2.1\t0.001\n",
        )
        .unwrap();

        let report_data = ReportData {
            artifacts: vec![ResultArtifactSummary {
                stage_id: "differential_expression".into(),
                artifact: "de_results.tsv".into(),
                result_schema: None,
                n_total: 1,
                n_significant: Some(1),
                direction_split: None,
                effect_distribution: None,
                grouped_significant: None,
                ranking: None,
                significant_entities: vec![EntityRow {
                    // Symbol form; the artifact is keyed on Ensembl ids.
                    entity: "ACAN".into(),
                    effect: Some(2.1),
                    significance: Some(0.001),
                    literature: LiteratureStatus::Novel,
                }],
                significant_table_path: "runtime/outputs/differential_expression/s.tsv".into(),
                full_table_path: "runtime/outputs/differential_expression/f.tsv".into(),
                spilled_to_attachment_only: false,
            }],
            literature: None,
        };
        let reporting = root.join("runtime/outputs/reporting");
        fs::create_dir_all(&reporting).unwrap();
        fs::write(
            reporting.join("report-data.json"),
            serde_json::to_string(&report_data).unwrap(),
        )
        .unwrap();

        assert!(
            report_data_cell_verdicts(root, "reporting", &cfg).is_empty(),
            "an identifier-form difference must abstain, never mass-Mismatch"
        );
    }

    /// Policy fixture whose `pvalueColumns` names the RAW column before the
    /// adjusted one and whose `effectSizeColumns` carries both `es` and `nes` —
    /// the shipped policy's shape, and the configuration under which a
    /// physical-column-order scan binds the wrong column.
    fn competing_alias_cfg() -> ExtractorConfig {
        let policy = serde_json::json!({
            "verifiableEntities": {
                "enabled": true,
                "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}", "R-HSA-\\d+"],
                "entityNameExcludePatterns": ["^ES$", "^NES$"],
                "directionVocab": { "up": ["upregulated"], "down": ["downregulated"] },
                "effectSizeColumns": ["log2FC", "log2FoldChange", "nes", "NES", "es"],
                "entityColumns": ["gene", "term", "pathway"],
                "pvalueColumns": ["pvalue", "pval", "padj"]
            }
        });
        ExtractorConfig::from_policy(&policy).expect("test policy loads")
    }

    /// Write `runtime/task-nodes.json` declaring one stage's `result_schema`.
    fn write_task_nodes(root: &Path, entries: &[(&str, serde_json::Value)]) {
        let rows: Vec<serde_json::Value> = entries
            .iter()
            .map(|(id, schema)| serde_json::json!({ "id": id, "attributes": { "result_schema": schema } }))
            .collect();
        fs::create_dir_all(root.join("runtime")).unwrap();
        fs::write(
            root.join("runtime/task-nodes.json"),
            serde_json::to_string(&rows).unwrap(),
        )
        .unwrap();
    }

    /// The transcription check must compare `report-data.json`'s significance
    /// against the column the assembler READ — the declared `padj` — even though
    /// the artifact prints `pvalue` first. Binding the raw column instead turns
    /// every correctly-transcribed row into a Mismatch.
    ///
    /// The tolerance is NOT widened to make this pass: it stays the
    /// float-round-trip floor, and the rows agree because the right cell is
    /// being read.
    #[test]
    fn report_data_significance_binds_the_declared_adjusted_column() {
        use crate::report_contract::{
            EntityRow, LiteratureStatus, ReportData, ResultArtifactSummary,
        };

        let cfg = competing_alias_cfg();
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let de_dir = root.join("runtime/outputs/differential_expression");
        fs::create_dir_all(&de_dir).unwrap();
        // Raw p and adjusted p differ by orders of magnitude, and `pvalue` is
        // printed FIRST — exactly the DESeq2 layout.
        fs::write(
            de_dir.join("de_results.tsv"),
            "gene\tbaseMean\tlog2FoldChange\tlfcSE\tstat\tpvalue\tpadj\n\
             ACAN\t100\t2.1\t0.1\t20\t1.0e-40\t3.5e-36\n\
             COL2A1\t200\t-1.5\t0.1\t-15\t2.0e-20\t4.5e-17\n",
        )
        .unwrap();

        let report_data = ReportData {
            artifacts: vec![ResultArtifactSummary {
                stage_id: "differential_expression".into(),
                artifact: "de_results.tsv".into(),
                result_schema: None,
                n_total: 2,
                n_significant: Some(2),
                direction_split: None,
                effect_distribution: None,
                grouped_significant: None,
                ranking: None,
                // The ADJUSTED values, as the assembler wrote them.
                significant_entities: vec![
                    EntityRow {
                        entity: "ACAN".into(),
                        effect: Some(2.1),
                        significance: Some(3.5e-36),
                        literature: LiteratureStatus::Novel,
                    },
                    EntityRow {
                        entity: "COL2A1".into(),
                        effect: Some(-1.5),
                        significance: Some(4.5e-17),
                        literature: LiteratureStatus::Novel,
                    },
                ],
                significant_table_path: "runtime/outputs/differential_expression/s.tsv".into(),
                full_table_path: "runtime/outputs/differential_expression/f.tsv".into(),
                spilled_to_attachment_only: false,
            }],
            literature: None,
        };
        let reporting = root.join("runtime/outputs/reporting");
        fs::create_dir_all(&reporting).unwrap();
        fs::write(
            reporting.join("report-data.json"),
            serde_json::to_string(&report_data).unwrap(),
        )
        .unwrap();

        let de_schema = serde_json::json!({
            "artifact": "de_results.tsv",
            "entity_column": "gene",
            "signed_effect_column": "log2FoldChange",
            "significance": { "column": "padj", "threshold": 0.05, "comparator": "lt" }
        });
        write_task_nodes(root, &[("differential_expression", de_schema)]);

        let verdicts = report_data_cell_verdicts(root, "reporting", &cfg);
        assert_eq!(verdicts.len(), 2);
        for v in &verdicts {
            assert!(
                matches!(v.status, ClaimStatus::Verified),
                "correctly-transcribed row must verify, got {:?}",
                v.status
            );
        }
        // The trace must name the DECLARED significance column, so the audit
        // records which cell was actually compared.
        let sig_columns: Vec<String> = verdicts
            .iter()
            .filter_map(|v| v.audit.as_ref())
            .filter_map(|a| a.measurement_column.clone())
            .collect();
        assert!(
            sig_columns.iter().all(|c| c != "pvalue"),
            "no verdict may be traced to the raw p column: {sig_columns:?}"
        );

        // And the same rows still verify with NO schema on disk: candidate
        // priority alone binds the adjusted column.
        fs::remove_file(root.join("runtime/task-nodes.json")).unwrap();
        let no_schema = report_data_cell_verdicts(root, "reporting", &cfg);
        assert_eq!(no_schema.len(), 2);
        for v in &no_schema {
            assert!(
                matches!(v.status, ClaimStatus::Verified),
                "candidate priority must bind `padj` without a schema, got {:?}",
                v.status
            );
        }
    }

    /// A pathway artifact whose header prints `ES` before `NES` and `pval`
    /// before `padj`: the declared schema binds `NES`/`padj`, so the assembler's
    /// values agree with their source cells.
    #[test]
    fn report_data_pathway_binds_normalized_effect_column() {
        use crate::report_contract::{
            EntityRow, LiteratureStatus, ReportData, ResultArtifactSummary,
        };

        let cfg = competing_alias_cfg();
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        let pw_dir = root.join("runtime/outputs/pathway_enrichment");
        fs::create_dir_all(&pw_dir).unwrap();
        // ES and NES disagree well beyond tolerance, as they do in practice.
        fs::write(
            pw_dir.join("pathway_results.tsv"),
            "pathway\tcollection\tterm\tpval\tpadj\tlog2err\tES\tNES\tsize\n\
             HALLMARK: Adipogenesis\tHALLMARK\tAdipogenesis\t1.0e-06\t5.17e-05\t0.6\t0.412\t1.965\t200\n",
        )
        .unwrap();

        let report_data = ReportData {
            artifacts: vec![ResultArtifactSummary {
                stage_id: "pathway_enrichment".into(),
                artifact: "pathway_results.tsv".into(),
                result_schema: None,
                n_total: 1,
                n_significant: Some(1),
                direction_split: None,
                effect_distribution: None,
                grouped_significant: None,
                ranking: None,
                significant_entities: vec![EntityRow {
                    entity: "HALLMARK: Adipogenesis".into(),
                    effect: Some(1.965),
                    significance: Some(5.17e-05),
                    literature: LiteratureStatus::Novel,
                }],
                significant_table_path: "runtime/outputs/pathway_enrichment/s.tsv".into(),
                full_table_path: "runtime/outputs/pathway_enrichment/f.tsv".into(),
                spilled_to_attachment_only: false,
            }],
            literature: None,
        };
        let reporting = root.join("runtime/outputs/reporting");
        fs::create_dir_all(&reporting).unwrap();
        fs::write(
            reporting.join("report-data.json"),
            serde_json::to_string(&report_data).unwrap(),
        )
        .unwrap();
        write_task_nodes(
            root,
            &[(
                "pathway_enrichment",
                serde_json::json!({
                    "artifact": "pathway_results.tsv",
                    "entity_column": "pathway",
                    "grouping_column": "collection",
                    "signed_effect_column": "NES",
                    "significance": { "column": "padj", "threshold": 0.25, "comparator": "lt" }
                }),
            )],
        );

        let verdicts = report_data_cell_verdicts(root, "reporting", &cfg);
        assert_eq!(verdicts.len(), 1);
        assert!(
            matches!(verdicts[0].status, ClaimStatus::Verified),
            "NES must be the compared cell, got {:?}",
            verdicts[0].status
        );
        let audit = verdicts[0].audit.as_ref().expect("trace");
        assert_eq!(audit.entity_column.as_deref(), Some("pathway"));
        assert_eq!(audit.measurement_column.as_deref(), Some("NES"));
    }

    /// Scaffold a package whose `pathway_enrichment` stage wrote a real result
    /// table, plus a `reporting` narrative carrying a markdown pathway table.
    fn scaffold_pathway_narrative(root: &Path, narrative_rows: &str) {
        let pw_dir = root.join("runtime/outputs/pathway_enrichment");
        fs::create_dir_all(&pw_dir).unwrap();
        fs::write(
            pw_dir.join("pathway_results.tsv"),
            "pathway\tcollection\tterm\tpval\tpadj\tES\tNES\n\
             REACTOME: Cytosolic tRNA Aminoacylation R-HSA-379716\tREACTOME\tCytosolic tRNA Aminoacylation R-HSA-379716\t1.0e-05\t2.10e-03\t-0.61\t-2.171\n\
             REACTOME: Formation of a pool of free 40S subunits R-HSA-72689\tREACTOME\tFormation of a pool of free 40S subunits R-HSA-72689\t0.62\t1.0\t0.18\t0.520463\n",
        )
        .unwrap();
        let reporting = root.join("runtime/outputs/reporting");
        fs::create_dir_all(&reporting).unwrap();
        fs::write(
            reporting.join("report.md"),
            format!(
                "## Depleted pathways\n\n\
                 | Pathway | Collection | NES | padj |\n\
                 |---|---|---|---|\n{narrative_rows}"
            ),
        )
        .unwrap();
        scaffold_config_dir(root);
        fs::write(
            root.join("downstream-policy/interpretation-policy.json"),
            r#"{
                "schemaVersion": "1.1",
                "targetStages": ["biological_interpretation"],
                "claimBoundary": {"associativeOnly": [], "requiresEvidence": []},
                "verifiableEntities": {
                    "enabled": true,
                    "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}", "R-HSA-\\d+"],
                    "directionVocab": {
                        "up": ["upregulated", "increased"],
                        "down": ["downregulated", "decreased"]
                    },
                    "effectSizeColumns": ["NES", "ES"],
                    "entityColumns": ["term", "pathway"],
                    "pvalueColumns": ["padj", "pval"]
                },
                "validationContract": {"requiredOutputs": [], "metrics": []},
                "evidenceRules": []
            }"#,
        )
        .unwrap();
    }

    /// A narrative table row naming a pathway ABSENT from the source table must
    /// produce a recorded non-Verified verdict rather than vanishing. Before the
    /// label-cell path, the whole-cell identifier gate dropped the row and the
    /// fabrication left no trace at all.
    #[test]
    fn fabricated_pathway_row_yields_a_non_verified_verdict() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        scaffold_pathway_narrative(
            root,
            "| Cytosolic tRNA Aminoacylation II R-HSA-379726 | REACTOME | -1.569 | 3.26e-02 |\n",
        );

        let outcome = verify_task_with_context(
            root,
            "reporting",
            root,
            ProjectClass::Bioinformatics,
            &[],
            false,
        );
        let v = expect_verified(outcome);
        let fabricated: Vec<&ClaimVerdict> = v
            .report
            .verdicts
            .iter()
            .filter(|x| x.claim.entity.contains("Aminoacylation II"))
            .collect();
        assert!(
            !fabricated.is_empty(),
            "the fabricated row must be represented in the ledger, not dropped: {:?}",
            v.report
                .verdicts
                .iter()
                .map(|x| &x.claim.entity)
                .collect::<Vec<_>>()
        );
        for f in &fabricated {
            assert!(
                !matches!(f.status, ClaimStatus::Verified),
                "a pathway absent from the source table must never verify: {:?}",
                f.status
            );
        }
    }

    /// A narrative row naming a REAL pathway with the wrong SIGN is a Mismatch:
    /// the entity resolves, the source cell is read from the declared effect
    /// column, and the values disagree.
    #[test]
    fn narrative_pathway_row_with_inverted_sign_is_a_mismatch() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        // Source NES for this pathway is +0.520463; the narrative claims -1.550.
        scaffold_pathway_narrative(
            root,
            "| Formation of a pool of free 40S subunits R-HSA-72689 | REACTOME | -1.550 | 4.94e-02 |\n",
        );

        let outcome = verify_task_with_context(
            root,
            "reporting",
            root,
            ProjectClass::Bioinformatics,
            &[],
            false,
        );
        let v = expect_verified(outcome);
        let row = v
            .report
            .verdicts
            .iter()
            .find(|x| x.claim.entity.contains("free 40S subunits"))
            .expect("the row must yield a verdict");
        assert!(
            matches!(row.status, ClaimStatus::Mismatch { .. }),
            "an inverted effect sign must be a Mismatch, got {:?}",
            row.status
        );
    }

    /// A CORRECT multi-word row verifies — the label path widens what can be
    /// checked without weakening any comparison.
    #[test]
    fn correct_multi_word_pathway_row_verifies() {
        let tmp = tempdir().unwrap();
        let root = tmp.path();
        scaffold_pathway_narrative(
            root,
            "| Cytosolic tRNA Aminoacylation R-HSA-379716 | REACTOME | -2.171 | 2.10e-03 |\n",
        );

        let outcome = verify_task_with_context(
            root,
            "reporting",
            root,
            ProjectClass::Bioinformatics,
            &[],
            false,
        );
        let v = expect_verified(outcome);
        let row = v
            .report
            .verdicts
            .iter()
            .find(|x| {
                x.claim.entity.contains("Cytosolic tRNA Aminoacylation")
                    && !x.claim.entity.contains(" II ")
            })
            .expect("the row must yield a verdict");
        assert!(
            matches!(row.status, ClaimStatus::Verified),
            "a faithfully transcribed multi-word row must verify, got {:?}",
            row.status
        );
    }

    // Throwaway local-validation harness: run the real verifier against a
    // real emitted package on disk. Ignored by default (path is machine-
    // specific). Run with:
    //   ECAA_REAL_PKG=<path> cargo test -p ecaa-workflow-core \
    //     real_package_smoke -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_package_smoke() {
        let pkg = std::env::var("ECAA_REAL_PKG").expect("set ECAA_REAL_PKG");
        let config_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");
        // Enumerate every task that produced output, so this works for any
        // modality (not just the RNA-seq task names).
        let outputs = std::path::Path::new(&pkg).join("runtime").join("outputs");
        let mut tasks: Vec<String> = std::fs::read_dir(&outputs)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        tasks.sort();
        let (mut tot_v, mut tot_m, mut tot_u) = (0usize, 0usize, 0usize);
        for task in &tasks {
            let task = task.as_str();
            match verify_task_with_context(
                std::path::Path::new(&pkg),
                task,
                &config_dir,
                ProjectClass::Bioinformatics,
                &[],
                false,
            ) {
                VerifyOutcome::Disabled | VerifyOutcome::Unavailable { .. } => {}
                VerifyOutcome::Verified(v) => {
                    let r = &v.report;
                    if r.n_checked == 0 {
                        continue;
                    }
                    tot_v += r.n_verified;
                    tot_m += r.n_mismatch;
                    tot_u += r.n_unverifiable;
                    println!(
                        "{task:28} -> checked={} VERIFIED={} mismatch={} unverifiable={}",
                        r.n_checked, r.n_verified, r.n_mismatch, r.n_unverifiable
                    );
                    for vd in &r.verdicts {
                        if let crate::claim_verifier::ClaimStatus::Mismatch { detail } = &vd.status
                        {
                            let ent: String = vd.claim.entity.chars().take(40).collect();
                            println!("      MISMATCH {ent}: {detail:.90}");
                        }
                    }
                }
            }
        }
        println!("PKG TOTALS: VERIFIED={tot_v} mismatch={tot_m} unverifiable={tot_u}");
    }

    fn write(path: &Path, body: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, body).unwrap();
    }

    /// Test helper: assert verification ran and return the `TaskVerification`.
    fn expect_verified(outcome: VerifyOutcome) -> TaskVerification {
        match outcome {
            VerifyOutcome::Verified(v) => v,
            other => panic!("expected VerifyOutcome::Verified, got {:?}", other.label()),
        }
    }

    fn scaffold_config_dir(dir: &Path) {
        let policy_dir = dir.join("downstream-policy");
        fs::create_dir_all(&policy_dir).unwrap();
        write(
            &policy_dir.join("interpretation-policy.json"),
            r#"{
                "schemaVersion": "1.1",
                "targetStages": ["biological_interpretation"],
                "claimBoundary": {"associativeOnly": [], "requiresEvidence": []},
                "verifiableEntities": {
                    "enabled": true,
                    "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}"],
                    "directionVocab": {
                        "up": ["upregulated", "increased"],
                        "down": ["downregulated", "decreased"]
                    },
                    "effectSizeColumns": ["log2FC"],
                    "entityColumns": ["gene"],
                    "pvalueColumns": ["padj"]
                },
                "validationContract": {"requiredOutputs": [], "metrics": []},
                "evidenceRules": []
            }"#,
        );
    }

    #[test]
    fn verifies_task_when_policy_and_narrative_are_present() {
        let pkg = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        scaffold_config_dir(cfg.path());

        // Package: runtime/task_interp/report.md + results/tables/summary_s1.tsv
        let task_dir = pkg.path().join("runtime").join("task_interp");
        write(
            &task_dir.join("report.md"),
            "# Findings\n\nACAN was upregulated in NP (log2FC=2.1, padj=0.001, Table S1).\n",
        );
        write(
            &pkg.path().join("results/tables/summary_s1.tsv"),
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.001\n",
        );

        let out = expect_verified(verify_task_with_context(
            pkg.path(),
            "task_interp",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[],
            false,
        ));
        assert_eq!(out.report.n_verified, 1, "{:?}", out.report.verdicts);
        assert_eq!(out.report.n_mismatch, 0);
    }

    #[test]
    fn verifies_standard_result_summary_when_no_narrative_file_exists() {
        let pkg = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        scaffold_config_dir(cfg.path());

        let task_dir = pkg.path().join("runtime/outputs/signal_analysis");
        write(
            &task_dir.join("result.json"),
            r#"{
                "status": "completed",
                "summary": "ACAN was upregulated (log2FC=2.1, padj=0.001)."
            }"#,
        );
        write(
            &task_dir.join("estimates.tsv"),
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.001\n",
        );
        write_task_nodes(
            pkg.path(),
            &[(
                "signal_analysis",
                serde_json::json!({
                    "artifact": "estimates.tsv",
                    "entity_column": "gene",
                    "signed_effect_column": "log2FC",
                    "significance": {
                        "column": "padj",
                        "comparator": "lt",
                        "threshold": 0.05
                    }
                }),
            )],
        );

        let verified = expect_verified(verify_task_with_context(
            pkg.path(),
            "signal_analysis",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[],
            false,
        ));
        assert_eq!(verified.narrative_path, task_dir.join("result.json"));
        assert!(
            verified
                .report
                .verdicts
                .iter()
                .any(|verdict| verdict.claim.entity == "ACAN"
                    && matches!(verdict.status, ClaimStatus::Verified)),
            "the standard result summary must enter the claim ledger: {:?}",
            verified.report.verdicts
        );
    }

    #[test]
    fn returns_disabled_when_no_narrative_artifact() {
        let pkg = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        scaffold_config_dir(cfg.path());
        // Empty runtime dir — no report.md. Policy is enabled + loadable, so
        // this is a benign "nothing to verify" → Disabled, not Unavailable.
        fs::create_dir_all(pkg.path().join("runtime").join("t1")).unwrap();
        assert!(matches!(
            verify_task_with_context(
                pkg.path(),
                "t1",
                cfg.path(),
                ProjectClass::Bioinformatics,
                &[],
                false,
            ),
            VerifyOutcome::Disabled
        ));
    }

    #[test]
    fn returns_unavailable_when_policy_missing() {
        let pkg = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        // No config/downstream-policy/interpretation-policy.json — a
        // configuration defect, surfaced as Unavailable (never Disabled).
        let task_dir = pkg.path().join("runtime").join("task_interp");
        write(&task_dir.join("report.md"), "ACAN was upregulated.\n");
        assert!(matches!(
            verify_task_with_context(
                pkg.path(),
                "task_interp",
                cfg.path(),
                ProjectClass::Bioinformatics,
                &[],
                false,
            ),
            VerifyOutcome::Unavailable { .. }
        ));
    }

    #[test]
    fn confirmatory_with_deviation_demotes_claim_strength() {
        // when verification runs in a confirmatory
        // session and a PostHocDeviation record covers the stage, the
        // claim's `strength` field must be demoted from the default
        // Prespecified to PostHoc.
        use crate::claim_verifier::ClaimStrength;
        use crate::decision_log::{DecisionActor, DecisionRecord, DecisionType};

        let pkg = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        scaffold_config_dir(cfg.path());

        // Narrative cites task_interp table — in confirmatory, the
        // deviation's target_stage ("task_interp") will match.
        let task_dir = pkg.path().join("runtime").join("task_interp");
        write(
            &task_dir.join("report.md"),
            "# Findings\n\nACAN was upregulated in task_interp summary_s1 \
             (log2FC=2.1, padj=0.001, Table S1).\n",
        );
        write(
            &pkg.path().join("results/tables/task_interp_summary.tsv"),
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.001\n",
        );

        let deviation = DecisionRecord::new(
            "session-x",
            DecisionType::PostHocDeviation {
                target_stage: "task_interp".into(),
                prior_method: "m1".into(),
                new_method: "m2".into(),
                reason: "SAP revised post-DB-lock".into(),
            },
            DecisionActor::Sme,
            Some("site imbalance".into()),
        );
        let out = expect_verified(verify_task_with_context(
            pkg.path(),
            "task_interp",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[deviation],
            true,
        ));
        // At least one claim must be demoted to PostHoc.
        assert!(out
            .report
            .verdicts
            .iter()
            .any(|v| matches!(v.strength, ClaimStrength::PostHoc)));
    }

    #[test]
    fn exploratory_session_never_demotes() {
        // Same narrative + deviation, but is_confirmatory=false.
        use crate::claim_verifier::ClaimStrength;
        use crate::decision_log::{DecisionActor, DecisionRecord, DecisionType};

        let pkg = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        scaffold_config_dir(cfg.path());

        let task_dir = pkg.path().join("runtime").join("task_interp");
        write(
            &task_dir.join("report.md"),
            "ACAN was upregulated task_interp (log2FC=2.1, padj=0.001, Table S1).\n",
        );
        write(
            &pkg.path().join("results/tables/summary_s1.tsv"),
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.001\n",
        );

        let deviation = DecisionRecord::new(
            "sx",
            DecisionType::PostHocDeviation {
                target_stage: "task_interp".into(),
                prior_method: "m1".into(),
                new_method: "m2".into(),
                reason: "r".into(),
            },
            DecisionActor::Sme,
            None,
        );
        let out = expect_verified(verify_task_with_context(
            pkg.path(),
            "task_interp",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[deviation],
            false,
        ));
        assert!(out
            .report
            .verdicts
            .iter()
            .all(|v| matches!(v.strength, ClaimStrength::Exploratory)));
    }

    #[test]
    fn runtime_decision_log_pointer_is_attached_when_present() {
        // (c): the verifier surfaces a pointer
        // to the agent-runtime decision log when one exists.
        let pkg = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        scaffold_config_dir(cfg.path());

        let task_dir = pkg.path().join("runtime").join("task_interp");
        write(
            &task_dir.join("report.md"),
            "ACAN was upregulated (log2FC=2.1, padj=0.001, Table S1).\n",
        );
        write(
            &pkg.path().join("results/tables/summary_s1.tsv"),
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.001\n",
        );
        // Agent-runtime log the task itself produced.
        write(
            &task_dir.join("runtime-decisions.jsonl"),
            "{\"kind\":\"method_selected\",\"value\":\"m1\"}\n",
        );

        let out = expect_verified(verify_task_with_context(
            pkg.path(),
            "task_interp",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[],
            false,
        ));
        assert_eq!(
            out.report.runtime_decision_log_path.as_deref(),
            Some("runtime/task_interp/runtime-decisions.jsonl")
        );
    }

    #[test]
    fn flags_mismatch_between_narrative_and_table() {
        let pkg = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        scaffold_config_dir(cfg.path());

        let task_dir = pkg.path().join("runtime").join("task_interp");
        // Narrative asserts UP, table says the log2FC is negative.
        write(
            &task_dir.join("report.md"),
            "ACAN was upregulated (log2FC=2.1, padj=0.001, Table S1).\n",
        );
        write(
            &pkg.path().join("results/tables/summary_s1.tsv"),
            "gene\tlog2FC\tpadj\nACAN\t-1.2\t0.001\n",
        );

        let out = expect_verified(verify_task_with_context(
            pkg.path(),
            "task_interp",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[],
            false,
        ));
        assert!(out.report.has_mismatch(), "{:?}", out.report.verdicts);
    }

    // ── disabled vs. unavailable policy distinction ──────────────────────

    #[test]
    fn malformed_policy_is_unavailable_not_disabled() {
        let cfg = tempdir().unwrap();
        let policy_dir = cfg.path().join("downstream-policy");
        fs::create_dir_all(&policy_dir).unwrap();
        // Truncated / malformed JSON — a configuration defect, not "disabled".
        write(
            &policy_dir.join("interpretation-policy.json"),
            "{ this is not json ",
        );
        match load_interpretation_policy(cfg.path()) {
            PolicyLoad::Unavailable { reason } => assert!(!reason.is_empty()),
            other => panic!("malformed policy must be Unavailable, got {:?}", other),
        }
    }

    #[test]
    fn missing_policy_is_unavailable_not_disabled() {
        let cfg = tempdir().unwrap();
        // No downstream-policy dir at all.
        match load_interpretation_policy(cfg.path()) {
            PolicyLoad::Unavailable { .. } => {}
            other => panic!("missing policy must be Unavailable, got {:?}", other),
        }
    }

    #[test]
    fn policy_without_verifiable_entities_is_disabled() {
        let cfg = tempdir().unwrap();
        let policy_dir = cfg.path().join("downstream-policy");
        fs::create_dir_all(&policy_dir).unwrap();
        // Valid JSON, no verifiableEntities block → intentionally disabled.
        write(
            &policy_dir.join("interpretation-policy.json"),
            r#"{"schemaVersion":"1.1"}"#,
        );
        assert!(matches!(
            load_interpretation_policy(cfg.path()),
            PolicyLoad::Loaded(_) | PolicyLoad::Disabled
        ));
    }

    #[test]
    fn default_policy_check_flags_unavailable_config_dir() {
        let cfg = tempdir().unwrap(); // empty → no policy
        assert!(
            !default_policy_is_loadable(cfg.path()),
            "empty config dir must report not-loadable"
        );
        // Repo's real config dir must be loadable.
        let real = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");
        assert!(
            default_policy_is_loadable(&real),
            "shipped default policy must be loadable + enabled"
        );
    }

    // ── flat-fallback policy resolution (self-contained packages) ────────

    /// An EMITTED package carries policy files FLAT under `<root>/policies/`
    /// (no `downstream-policy/` subdir). Pointing `config_dir` at that flat dir
    /// must resolve + load the policy via the flat fallback.
    #[test]
    fn loads_policy_from_flat_config_dir() {
        let cfg = tempdir().unwrap();
        // Flat: interpretation-policy.json at the top level, NO downstream-policy/.
        write(
            &cfg.path().join("interpretation-policy.json"),
            r#"{"verifiableEntities":{"enabled":true}}"#,
        );
        assert!(
            matches!(
                load_interpretation_policy(cfg.path()),
                PolicyLoad::Loaded(_)
            ),
            "flat policy must load via the flat fallback"
        );
    }

    /// Precedence proof: when BOTH locations hold a policy, the
    /// `downstream-policy/` nested file wins — so a repo `config/` (the server)
    /// resolves byte-identically to before this fallback was added.
    #[test]
    fn downstream_policy_subdir_wins_over_flat() {
        let cfg = tempdir().unwrap();
        // Flat copy is DISABLED; nested copy is ENABLED.
        write(
            &cfg.path().join("interpretation-policy.json"),
            r#"{"verifiableEntities":{"enabled":false},"marker":"flat"}"#,
        );
        write(
            &cfg.path()
                .join("downstream-policy")
                .join("interpretation-policy.json"),
            r#"{"verifiableEntities":{"enabled":true},"marker":"nested"}"#,
        );
        match load_interpretation_policy(cfg.path()) {
            PolicyLoad::Loaded(v) => assert_eq!(
                v.get("marker").and_then(|m| m.as_str()),
                Some("nested"),
                "downstream-policy/ must win over the flat copy"
            ),
            other => panic!("expected nested (enabled) policy to load, got {:?}", other),
        }
    }

    /// End-to-end self-containment: verification runs with `config_dir` pointed
    /// at the package's OWN FLAT `policies/` dir and NO repo `config/` reachable.
    /// Proves the deployment-independent finalize path: n_checked >= 1.
    #[test]
    fn verifies_with_config_dir_at_package_own_flat_policies() {
        let pkg = tempdir().unwrap();
        let root = pkg.path();
        // The package's own flat policies/ — exactly what the emitter copies.
        write(
            &root.join("policies").join("interpretation-policy.json"),
            r#"{
                "verifiableEntities": {
                    "enabled": true,
                    "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}"],
                    "directionVocab": {"up": ["upregulated"], "down": ["downregulated"]},
                    "effectSizeColumns": ["log2FC"],
                    "entityColumns": ["gene"],
                    "pvalueColumns": ["padj"]
                }
            }"#,
        );
        let task_dir = root.join("runtime").join("outputs").join("task_interp");
        write(
            &task_dir.join("report.md"),
            "ACAN was upregulated (log2FC=2.1, padj=0.001, Table S1).\n",
        );
        write(
            &root.join("results/tables/summary_s1.tsv"),
            "gene\tlog2FC\tpadj\nACAN\t2.1\t0.001\n",
        );

        // config_dir IS the package's own flat policies/ — no repo config.
        let out = expect_verified(verify_task_with_context(
            root,
            "task_interp",
            &root.join("policies"),
            ProjectClass::Bioinformatics,
            &[],
            false,
        ));
        assert!(
            out.report.n_checked >= 1,
            "flat package-own policies must drive verification; n_checked = {}",
            out.report.n_checked
        );
    }

    /// Config scaffold whose entityColumns include `gene_id` so a
    /// `de_results.tsv` (header `gene_id`) both extracts (entity column) and
    /// verifies (table-load entity column). Self-contained: does NOT depend on
    /// the separate interpretation-policy.json change from Workstream A.
    fn scaffold_config_dir_with_gene_id(dir: &Path) {
        let policy_dir = dir.join("downstream-policy");
        fs::create_dir_all(&policy_dir).unwrap();
        write(
            &policy_dir.join("interpretation-policy.json"),
            r#"{
                "schemaVersion": "1.1",
                "targetStages": ["differential_expression"],
                "claimBoundary": {"associativeOnly": [], "requiresEvidence": []},
                "verifiableEntities": {
                    "enabled": true,
                    "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}"],
                    "directionVocab": {
                        "up": ["upregulated", "increased"],
                        "down": ["downregulated", "decreased"]
                    },
                    "effectSizeColumns": ["log2fc", "log2FC"],
                    "entityColumns": ["gene_id", "gene"],
                    "pvalueColumns": ["adj_pvalue", "padj"]
                },
                "validationContract": {"requiredOutputs": [], "metrics": []},
                "evidenceRules": []
            }"#,
        );
    }

    #[test]
    fn de_results_table_is_not_mined_into_per_row_claims() {
        // C3 reverted: a table-only differential_expression task (no narrative)
        // must NOT spawn one claim per de_results.tsv row. Mining + self-verifying
        // raw result-table rows is a circular self-check that inflates n_checked
        // with vacuous Verified entries (~17.8k for a real DE table). Here the
        // single-row table must yield ZERO mined claims (the task verifies as
        // Disabled / recall-gap, never a table-row claim explosion).
        let pkg = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        scaffold_config_dir_with_gene_id(cfg.path());

        let root = pkg.path();
        let de_dir = root
            .join("runtime")
            .join("outputs")
            .join("differential_expression");
        write(
            &de_dir.join("de_results.tsv"),
            "gene_id\tlog2fc\tadj_pvalue\nENSG00000103196\t2.63\t8e-60\n",
        );
        write(
            &root.join("WORKFLOW.json"),
            r#"{"tasks":{"differential_expression":{"state":{"status":"completed"}}}}"#,
        );

        let outcome = verify_task_with_context(
            root,
            "differential_expression",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[],
            false,
        );
        // No narrative + no structured claims + no table mining => nothing to
        // verify. Whatever the outcome (Disabled or a recall-gap Verified with
        // an empty report), it must carry NO per-row table claim.
        let n = match outcome {
            VerifyOutcome::Verified(v) => v.report.n_checked,
            _ => 0,
        };
        assert_eq!(
            n, 0,
            "raw de_results.tsv rows must not be mined into claims; got n_checked = {n}"
        );
    }

    #[test]
    fn finalize_package_reports_zero_for_unexecuted_package() {
        // An emit-only package whose tasks are NOT completed must finalize 0
        // tasks — the returned summary reflects "not run", and finalize_package
        // emits the observability warn (E1). The summary flag is the asserted
        // contract here; the tracing warn is best-effort.
        let pkg = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        scaffold_config_dir(cfg.path());

        let root = pkg.path();
        // Two tasks, neither completed (emit-only / never executed).
        write(
            &root.join("WORKFLOW.json"),
            r#"{"tasks":{
                "differential_expression":{"state":{"status":"pending"}},
                "pathway_enrichment":{"state":{"status":"pending"}}
            }}"#,
        );

        let summary = finalize_package(
            root,
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[],
            false,
            None,
        )
        .expect("finalize_package over an unexecuted package must not error");
        assert_eq!(
            summary.tasks_finalized, 0,
            "an unexecuted package must finalize 0 tasks (n_checked:0 reflects 'not run')"
        );
    }
}
