//! Projects live `ClaimVerificationReport` verdicts into the audit-proof
//! C-graph shape and persists them as an HMAC-signed, agent-unforgeable sink
//! the loader verifies.

use crate::audit_writer::AuditWriter;
use crate::claim_verifier::{
    verdict_class_of, ClaimStatus, ClaimVerdict, ClaimVerificationReport, VerdictClass,
};
use crate::coverage::CoverageResult;
use ecaa_workflow_types::consts::{ECAA_VERSION, MIN_READER_VERSION};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Sink filename relative to the package RUNTIME dir. **NDJSON despite the
/// `.json` extension**: one independently HMAC-signed row per finalized task —
/// readers MUST parse line-by-line, never as a single JSON document (see
/// [`persist_signed_verdicts`]). Single source for the loader's
/// runtime-relative join so the writer and reader paths cannot drift.
pub const SIGNED_SINK_UNDER_RUNTIME: &str = "verification-reports/claim-verification.signed.json";

/// Sink path under the BagIt-excluded, never-agent-trusted reports dir,
/// relative to the PACKAGE ROOT. Kept in lockstep with
/// [`SIGNED_SINK_UNDER_RUNTIME`] by the `signed_sink_paths_agree` test.
pub const SIGNED_SINK_REL: &str = "runtime/verification-reports/claim-verification.signed.json";

/// Build the package-relative Evidence (V) reference for a verified claim's
/// `source_table`. The runtime verifier records the table it confirmed against
/// by basename (e.g. `de_results.tsv`); a bare basename is resolved to the
/// output that actually holds it, then the resulting `@id` matches what
/// [`crate::ro_crate::register_produced_output_tables`] assigns — so the C→V
/// `supported_by` link resolves and `cross_graph_integrity` (Inv 5) and
/// `evidence_coverage` (Inv 3) agree.
///
/// Resolution order (deterministic, on-disk):
///   1. verbatim if `source_table` already carries a path separator (an
///      explicit package-relative path, e.g. `results/tables/de.csv`);
///   2. the CLAIMING task's own dir `runtime/outputs/<task>/<basename>` when
///      that file exists (backward-compatible, and correct for stages that
///      summarize their own output);
///   3. otherwise the UNIQUE `runtime/outputs/<producer>/<basename>` that
///      exists on disk — a terminal reporting stage cites an UPSTREAM table
///      (e.g. `final_reporting` citing `differential_expression/de_results.tsv`),
///      so anchoring the `@id` to the claiming task's dir dangled;
///   4. otherwise the claiming task's path (previous behaviour) as a last
///      resort, so an absent file still yields a stable, if unresolved, ref.
fn evidence_ref_for(package_root: &Path, task_id: &str, source_table: &str) -> String {
    if source_table.contains('/') {
        return source_table.to_string();
    }
    let same_task = format!("runtime/outputs/{task_id}/{source_table}");
    if package_root.join(&same_task).exists() {
        return same_task;
    }
    unique_output_bearing(package_root, source_table).unwrap_or(same_task)
}

/// Return `runtime/outputs/<producer>/<basename>` iff exactly one task output
/// dir directly holds a file named `basename`. `None` when zero or more than
/// one match (ambiguous → the caller keeps the claiming-task fallback rather
/// than guess). Task dirs are visited in sorted order for determinism.
fn unique_output_bearing(package_root: &Path, basename: &str) -> Option<String> {
    let outputs = package_root.join("runtime/outputs");
    let mut task_dirs: Vec<std::ffi::OsString> = std::fs::read_dir(&outputs)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name())
        .collect();
    task_dirs.sort();
    let mut hit: Option<String> = None;
    for d in task_dirs {
        if outputs.join(&d).join(basename).exists() {
            if hit.is_some() {
                return None; // ambiguous: same basename in >1 producer dir
            }
            hit = Some(format!(
                "runtime/outputs/{}/{}",
                d.to_string_lossy(),
                basename
            ));
        }
    }
    hit
}

/// Resolve and de-duplicate every evidence table recorded for a verdict.
///
/// The numeric gate's audit tuple is authoritative because it records the
/// table that was actually opened. The extracted claim's source table is kept
/// as a fallback for non-numeric and pre-audit verdicts. Both may name the same
/// file, so the result is de-duplicated without changing first-seen order.
fn evidence_refs_for(v: &ClaimVerdict, package_root: &Path, task_id: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut push = |raw: &str| {
        let resolved = evidence_ref_for(package_root, task_id, raw);
        if !refs.contains(&resolved) {
            refs.push(resolved);
        }
    };
    if let Some(source) = v
        .audit
        .as_ref()
        .and_then(|audit| audit.source_table.as_deref())
    {
        push(source);
    }
    if let Some(source) = v.claim.source_table.as_deref() {
        push(source);
    }
    refs
}

/// Human-readable explanation carried by every non-verified verdict.
fn verdict_detail(status: &ClaimStatus) -> Option<&str> {
    match status {
        ClaimStatus::Verified => None,
        ClaimStatus::Mismatch { detail } => Some(detail),
        ClaimStatus::Unverifiable { reason }
        | ClaimStatus::Pending { reason }
        | ClaimStatus::Suspicious { reason } => Some(reason),
    }
}

/// Project live verdicts into the audit-proof C-graph row shape
/// (`{claim_id, status, supported_by, checked_against, contradicts,
/// attempted_sources}`). Deterministic; `claim_id` is positional
/// (`<task_id>#claim-<i>`) so it is collision-free within a task.
pub fn project_verdict_rows(
    report: &ClaimVerificationReport,
    task_id: &str,
    package_root: &Path,
) -> Vec<Value> {
    report
        .verdicts
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let evidence_refs = evidence_refs_for(v, package_root, task_id);
            let (status, supported_by, checked_against, contradicts, attempted_sources): (
                &str,
                Vec<String>,
                Vec<String>,
                Vec<String>,
                Vec<String>,
            ) = match &v.status {
                ClaimStatus::Verified => (
                    "verified",
                    evidence_refs.clone(),
                    evidence_refs,
                    Vec::new(),
                    Vec::new(),
                ),
                ClaimStatus::Mismatch { .. } => (
                    "mismatch",
                    // The evidence supports the verifier's adjudication while
                    // `contradicts` states its relationship to the claim text.
                    // Keeping both preserves the closed C-graph contract and
                    // removes the old verdict-without-evidence ambiguity.
                    evidence_refs.clone(),
                    evidence_refs.clone(),
                    evidence_refs,
                    Vec::new(),
                ),
                // A table WAS loaded and checked but yielded nothing
                // determinable (no effect/p column, entity not in any
                // configured entity column, etc.). Externally distinct from
                // never-adjudicated `Pending` so the two coverage buckets stay
                // honest on the wire.
                ClaimStatus::Unverifiable { .. } => {
                    let loaded = v
                        .audit
                        .as_ref()
                        .and_then(|audit| audit.source_table.as_ref())
                        .is_some();
                    if loaded {
                        (
                            "unverifiable",
                            Vec::new(),
                            evidence_refs,
                            Vec::new(),
                            Vec::new(),
                        )
                    } else {
                        (
                            "unverifiable",
                            Vec::new(),
                            Vec::new(),
                            Vec::new(),
                            evidence_refs,
                        )
                    }
                }
                // Never-adjudicated claims: a non-verified, non-blocking
                // "pending" audit-proof row. A declared but unresolved source
                // is an attempted route, never evidence that was checked.
                ClaimStatus::Pending { .. } => {
                    ("pending", Vec::new(), Vec::new(), Vec::new(), evidence_refs)
                }
                // Soft/review-required: carries the cited table it was checked
                // against (the entity was absent from it), so the audit-proof
                // supported_by floor is satisfied without a separate exemption.
                ClaimStatus::Suspicious { .. } => (
                    "suspicious",
                    evidence_refs.clone(),
                    evidence_refs,
                    Vec::new(),
                    Vec::new(),
                ),
            };
            // Carry the claim's human-readable text (`excerpt`, falling back to
            // the matched `entity` when the excerpt is empty) and the matched
            // `entity` onto the projected row. The C-subgraph projector reads
            // `text` to populate the embedded `Claim` node — without it the
            // node text was always empty. These are verbatim recorded claim
            // fields, never derived/invented.
            let text = if v.claim.excerpt.trim().is_empty() {
                v.claim.entity.clone()
            } else {
                v.claim.excerpt.clone()
            };
            let class = verdict_class_for(v);
            let mut row = json!({
                "claim_id": format!("{task_id}#claim-{i}"),
                "status": status,
                "supported_by": supported_by,
                "checked_against": checked_against,
                "contradicts": contradicts,
                "attempted_sources": attempted_sources,
                "verdict_detail": verdict_detail(&v.status),
                "text": text,
                "entity": v.claim.entity,
                "class": serde_json::to_value(class).unwrap_or(Value::Null),
            });
            // Flatten the reconstructable audit tuple (source_table, entity /
            // measurement columns, claimed vs observed, comparator, tolerances,
            // verifier_version, rationale, parse_coverage) onto the row so a
            // reviewer can reconstruct what was compared. `class` is already at
            // the top level, so skip the nested copy.
            if let Some(audit) = &v.audit {
                if let Ok(Value::Object(fields)) = serde_json::to_value(audit) {
                    if let Some(obj) = row.as_object_mut() {
                        for (k, val) in fields {
                            if k == "class" {
                                continue;
                            }
                            obj.insert(k, val);
                        }
                    }
                }
            }
            row
        })
        .collect()
}

/// The verification-method class for a verdict: the audit's class when the gate
/// recorded one, else derived from the claim's contract. Total, so per-class
/// counts sum to `n_checked`.
fn verdict_class_for(v: &ClaimVerdict) -> VerdictClass {
    v.audit
        .as_ref()
        .map(|a| a.class)
        .unwrap_or_else(|| verdict_class_of(&v.claim))
}

/// Per-class verdict tally (`numeric_table` / `directional` / `entity_presence`
/// / `literature_quotation`) for the persisted header. Sums to `verdicts.len()`.
fn class_counts_value(verdicts: &[ClaimVerdict]) -> Value {
    let (mut numeric, mut directional, mut presence, mut literature) = (0u64, 0u64, 0u64, 0u64);
    for v in verdicts {
        match verdict_class_for(v) {
            VerdictClass::NumericTable => numeric += 1,
            VerdictClass::Directional => directional += 1,
            VerdictClass::EntityPresence => presence += 1,
            VerdictClass::LiteratureQuotation => literature += 1,
        }
    }
    json!({
        "numeric_table": numeric,
        "directional": directional,
        "entity_presence": presence,
        "literature_quotation": literature,
    })
}

/// Per-class tally computed from already-projected JSON rows (each carrying a
/// `class` string). A row with a missing/unknown class is counted under
/// `numeric_table` so the four buckets always sum to the row count.
fn class_counts_from_rows(rows: &[Value]) -> Value {
    let (mut numeric, mut directional, mut presence, mut literature) = (0u64, 0u64, 0u64, 0u64);
    for row in rows {
        match row.get("class").and_then(Value::as_str) {
            Some("directional") => directional += 1,
            Some("entity_presence") => presence += 1,
            Some("literature_quotation") => literature += 1,
            _ => numeric += 1,
        }
    }
    json!({
        "numeric_table": numeric,
        "directional": directional,
        "entity_presence": presence,
        "literature_quotation": literature,
    })
}

/// Build the full claim-verification document that gets HMAC-signed and
/// written to the sink. Shape is a superset of the emit-time stub
/// (`schema_version` + counts + `verdicts`) plus the version triple and
/// provenance discriminators distinguishing it from the agent-writable
/// stub the loader must NOT trust, and — when present — the
/// structured-claims-only `coverage` block the reframed Inv 1 reads
/// (recall floor). The `coverage` block is computed only from structured
/// `result.json claims[]` verdicts (deterministic), never the regex path.
pub fn build_sink_doc(
    report: &ClaimVerificationReport,
    task_id: &str,
    coverage: Option<&CoverageResult>,
    package_root: &Path,
) -> Value {
    use crate::ablation::{AblationFlag, AblationFlagExt};
    // Site 1 of the two-site benchmark toggle (Aim 3A). Under
    // ECAA_ABLATE_CLAIM_CONSISTENCY the populated signed sink is suppressed:
    // the doc carries ZERO verdicts, zeroed counts, a suppressed coverage
    // block, and an explicit `ablated: true` marker — distinct from the
    // legacy emit-time empty stub (which has no such field). The A-vs-B'
    // contrast therefore measures the PRESENCE of carried verdicts/coverage
    // (enforcement), not a status-enum flip on a perpetually-empty file.
    let ablated = AblationFlag::ClaimConsistency.is_active();
    let mut doc = json!({
        "schema_version": "1",
        "source": "runtime-verifier",
        "task_id": task_id,
        "ecaa_version": ECAA_VERSION,
        "min_reader_version": MIN_READER_VERSION,
        "ablated": ablated,
        "n_checked": if ablated { 0 } else { report.n_checked },
        "n_verified": if ablated { 0 } else { report.n_verified },
        "n_mismatch": if ablated { 0 } else { report.n_mismatch },
        "n_unverifiable": if ablated { 0 } else { report.n_unverifiable },
        "n_pending": if ablated { 0 } else { report.n_pending },
        "n_suspicious": if ablated { 0 } else { report.n_suspicious },
        "class_counts": if ablated { class_counts_value(&[]) } else { class_counts_value(&report.verdicts) },
        "verifier_version": crate::claim_verifier::CLAIM_VERIFIER_VERSION,
        "verdicts": if ablated { Vec::new() } else { project_verdict_rows(report, task_id, package_root) },
    });
    // The coverage block (recall floor) is signed-sink content the reframed
    // Inv 1 reads; suppress it on the ablated arm alongside the verdicts.
    if !ablated {
        if let Some(cov) = coverage {
            doc["coverage"] = serde_json::to_value(cov).unwrap_or(Value::Null);
        }
    }
    doc
}

/// Build the sink doc for `task_id`, HMAC-sign it with `writer`, and APPEND
/// it as one signed JSONL row to
/// `<package_root>/runtime/verification-reports/claim-verification.signed.json`.
/// Returns the written path.
///
/// The sink is append-only: one independently-signed row per task
/// verification. The loader (`audit_proof::loader::load_claims`) unions all
/// rows so a recall gap recorded by an earlier task can never be erased by a
/// later coverage-less task — a last-writer REPLACE silently dropped it (the
/// F2 at-rest erasure). Because each row carries its own HMAC, appending
/// needs no rewrite of prior rows.
///
/// The reports dir is already excluded from the BagIt manifest
/// (`emitter/bagit.rs`) and is never trusted from the agent side
/// (`server::chat_routes::events::rate_limit`). Written post-execution by
/// the host (which holds the session secret), so it is outside the emit
/// byte-diff baseline and cannot be forged by the agent.
pub fn persist_signed_verdicts(
    package_root: &Path,
    task_id: &str,
    report: &ClaimVerificationReport,
    coverage: Option<&CoverageResult>,
    writer: &AuditWriter,
) -> std::io::Result<PathBuf> {
    let path = package_root.join(SIGNED_SINK_REL);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let doc = build_sink_doc(report, task_id, coverage, package_root);
    let mut buf = Vec::new();
    writer.write_signed_row(&mut buf, &doc)?;
    // `buf` already ends in '\n' (write_signed_row uses writeln!).

    // Idempotent replace, mirroring `refresh_plaintext_sidecar`. The sink is
    // NDJSON — one independently-MAC'd row per finalized task. A plain append
    // (the prior behaviour) was correct only for a single end-to-end run; on a
    // RE-finalize it appended a *second* row for the same task, leaving the
    // first (now stale) row in place. The audit-proof loader reads the sink as
    // the trust surface and keys on the first row per task, so it would then
    // evaluate STALE verdicts (e.g. a claim the corrected verifier no longer
    // emits) and report phantom violations. Drop any existing row whose
    // `task_id` equals this task's, preserving every other row's original bytes
    // (and therefore its signature) verbatim, then append the fresh row.
    let mut kept: Vec<u8> = Vec::new();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        for line in existing.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let belongs_to_this_task = serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|v| {
                    v.get("task_id")
                        .and_then(serde_json::Value::as_str)
                        .map(|t| t == task_id)
                })
                .unwrap_or(false);
            if belongs_to_this_task {
                continue;
            }
            kept.extend_from_slice(line.as_bytes());
            kept.push(b'\n');
        }
    }
    kept.extend_from_slice(&buf);

    // Atomic-ish rewrite: a full overwrite replaces the prior contents in one
    // call (matching the plaintext sidecar's `std::fs::write`).
    std::fs::write(&path, &kept)?;
    Ok(path)
}

/// Plaintext (operator/UI-visible) sidecar path. This is the human-readable,
/// agent-writable view the UI renders and `jq '.n_checked'` probes; the
/// signed sink ([`SIGNED_SINK_REL`]) remains the trust surface the
/// audit-proof loader prefers.
pub const PLAINTEXT_SIDECAR_REL: &str = "runtime/claim-verification.json";

/// Refresh the plaintext `runtime/claim-verification.json` so its `n_checked`
/// and `verdicts[]` reflect this task's recomputed verdicts, AGGREGATED across
/// every finalized task in the package.
///
/// Schema: the flat emit-time stub
/// (`schema_version` + `n_checked`/`n_verified`/`n_mismatch`/`n_unverifiable`
/// + `verdicts[]`) the emitter writes via
/// `conversation::emit::sidecars::write_claim_verification`. Each `verdicts[]`
/// row is the same `{claim_id, status, supported_by}` shape
/// [`project_verdict_rows`] produces (and [`build_sink_doc`] carries), so the
/// audit-proof C-graph projection and the UI read both surfaces identically.
///
/// **Aggregation + idempotency.** The plaintext is a single flat report with no
/// per-task keying, but every verdict `claim_id` embeds its task
/// (`<task_id>#claim-<i>`). We therefore read-modify-write by `claim_id`
/// prefix: drop any rows belonging to THIS `task_id`, append this task's fresh
/// rows, and recompute the four counts from the merged verdict set. Finalizing
/// multiple tasks accumulates; re-finalizing the same task REPLACES its rows
/// (never double-counts). The whole file is rewritten atomically each call, so
/// the counts always equal the row tallies.
///
/// **Ablation.** Under `ECAA_ABLATE_CLAIM_CONSISTENCY` this task contributes
/// ZERO rows (mirroring [`build_sink_doc`]'s suppression of the signed sink and
/// the emit-time stub), so the A-vs-B′ contrast measures enforcement presence.
///
/// Verdict rows are written in deterministic order: existing other-task rows
/// in their on-disk order, then this task's rows in `report.verdicts` order.
pub fn refresh_plaintext_sidecar(
    package_root: &Path,
    task_id: &str,
    report: &ClaimVerificationReport,
) -> std::io::Result<PathBuf> {
    use crate::ablation::{AblationFlag, AblationFlagExt};

    let path = package_root.join(PLAINTEXT_SIDECAR_REL);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Read the existing flat report (the emit-time stub, or a prior task's
    // refresh). Missing/unparsable → start from no rows; this is a best-effort
    // operator view, not the trust surface.
    let prior_rows: Vec<Value> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|v| v.get("verdicts").and_then(Value::as_array).cloned())
        .unwrap_or_default();

    // Drop any verdict rows belonging to THIS task (idempotent replace), keying
    // on the `<task_id>#claim-<i>` claim_id convention.
    let this_task_prefix = format!("{task_id}#");
    let mut merged: Vec<Value> = prior_rows
        .into_iter()
        .filter(|row| {
            row.get("claim_id")
                .and_then(Value::as_str)
                .map(|id| !id.starts_with(&this_task_prefix))
                .unwrap_or(true)
        })
        .collect();

    // Append this task's fresh rows (suppressed under the ablation flag).
    if !AblationFlag::ClaimConsistency.is_active() {
        merged.extend(project_verdict_rows(report, task_id, package_root));
    }

    // Recompute the counts from the merged row set so the counts always
    // match the rows on disk.
    let mut n_verified = 0u64;
    let mut n_mismatch = 0u64;
    let mut n_unverifiable = 0u64;
    let mut n_suspicious = 0u64;
    for row in &merged {
        match row.get("status").and_then(Value::as_str) {
            Some("verified") => n_verified += 1,
            Some("mismatch") => n_mismatch += 1,
            Some("suspicious") => n_suspicious += 1,
            // "unverifiable" (checked-but-undeterminable) and "pending"
            // (never-adjudicated) both roll into the single flat
            // `n_unverifiable` tally here; anything else is defensively
            // counted the same way.
            _ => n_unverifiable += 1,
        }
    }
    let n_checked = merged.len() as u64;
    let class_counts = class_counts_from_rows(&merged);

    let doc = json!({
        "schema_version": "1",
        "n_checked": n_checked,
        "n_verified": n_verified,
        "n_unverifiable": n_unverifiable,
        "n_mismatch": n_mismatch,
        "n_suspicious": n_suspicious,
        "class_counts": class_counts,
        "verifier_version": crate::claim_verifier::CLAIM_VERIFIER_VERSION,
        "verdicts": merged,
    });
    let body = serde_json::to_vec_pretty(&doc).map_err(std::io::Error::other)?;
    std::fs::write(&path, body)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim_contract::ClaimContract;
    use crate::claim_extractor::Claim;
    use crate::claim_verifier::{ClaimStrength, ClaimVerdict};

    fn claim(entity: &str, table: Option<&str>) -> Claim {
        Claim {
            entity: entity.to_string(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: table.map(|t| t.to_string()),
            excerpt: String::new(),
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
        }
    }

    fn verdict(c: Claim, status: ClaimStatus) -> ClaimVerdict {
        ClaimVerdict {
            claim: c,
            status,
            strength: ClaimStrength::default(),
            audit: None,
        }
    }

    #[test]
    fn signed_sink_paths_agree() {
        // The package-root-relative and runtime-relative sink consts must stay
        // in lockstep so the writer and the loader never point at different files.
        assert_eq!(
            SIGNED_SINK_REL,
            format!("runtime/{SIGNED_SINK_UNDER_RUNTIME}"),
            "SIGNED_SINK_REL must be `runtime/` + SIGNED_SINK_UNDER_RUNTIME"
        );
    }

    #[test]
    fn suspicious_status_is_soft_counted_separately_and_projects_with_evidence() {
        // Foundation contract for the new Suspicious verdict: it increments its
        // OWN counter (not n_mismatch), does not trip the session block, and
        // projects to the "suspicious" wire string carrying its cited table so
        // the audit-proof supported_by floor is satisfied without exemption.
        use crate::claim_verifier::ClaimStatus;
        let mut report = ClaimVerificationReport::empty();
        report.push(verdict(
            claim("FOOBAR2", Some("results/tables/de.csv")),
            ClaimStatus::Suspicious {
                reason: "entity absent from cited table; fabricated/untested".into(),
            },
        ));
        assert_eq!(report.n_suspicious, 1);
        assert_eq!(
            report.n_mismatch, 0,
            "Suspicious must NOT count as mismatch"
        );
        assert_eq!(report.n_verified, 0);
        assert_eq!(report.n_unverifiable, 0);
        assert!(report.has_suspicious());
        assert!(
            !report.has_mismatch(),
            "Suspicious must not trip the session-blocking mismatch gate"
        );
        let rows = project_verdict_rows(&report, "diff_expr", std::path::Path::new("."));
        assert_eq!(rows[0]["status"], json!("suspicious"));
        assert_eq!(
            rows[0]["supported_by"],
            json!(["results/tables/de.csv"]),
            "Suspicious carries its cited table (a `/`-bearing ref is kept verbatim) \
             so the supported_by floor passes"
        );
        assert_eq!(rows[0]["checked_against"], json!(["results/tables/de.csv"]));
        assert_eq!(
            rows[0]["verdict_detail"],
            json!("entity absent from cited table; fabricated/untested")
        );
    }

    #[test]
    fn verified_claim_projects_with_supported_by() {
        let report = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 1,
            n_mismatch: 0,
            n_unverifiable: 0,
            n_pending: 0,
            n_suspicious: 0,
            verdicts: vec![verdict(
                claim("TP53", Some("results/tables/de.csv")),
                ClaimStatus::Verified,
            )],
            runtime_decision_log_path: None,
        };
        let rows = project_verdict_rows(&report, "diff_expr", std::path::Path::new("."));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["claim_id"], json!("diff_expr#claim-0"));
        assert_eq!(rows[0]["status"], json!("verified"));
        assert_eq!(rows[0]["supported_by"], json!(["results/tables/de.csv"]));
        assert_eq!(rows[0]["checked_against"], json!(["results/tables/de.csv"]));
        assert_eq!(rows[0]["contradicts"], json!([]));
        assert_eq!(rows[0]["attempted_sources"], json!([]));
    }

    #[test]
    fn unverifiable_projects_unverifiable_empty() {
        let report = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 0,
            n_mismatch: 0,
            n_unverifiable: 1,
            n_pending: 0,
            n_suspicious: 0,
            verdicts: vec![verdict(
                claim("BRCA1", None),
                ClaimStatus::Unverifiable {
                    reason: "no table".into(),
                },
            )],
            runtime_decision_log_path: None,
        };
        let rows = project_verdict_rows(&report, "diff_expr", std::path::Path::new("."));
        // Unverifiable now carries its own external label (distinct from the
        // never-adjudicated `Pending` → "pending"); it stays evidence-free.
        assert_eq!(rows[0]["status"], json!("unverifiable"));
        assert_eq!(rows[0]["supported_by"], json!([]));
        assert_eq!(rows[0]["checked_against"], json!([]));
        assert_eq!(rows[0]["attempted_sources"], json!([]));
        assert_eq!(rows[0]["verdict_detail"], json!("no table"));
    }

    #[test]
    fn pending_projects_pending_empty() {
        let report = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 0,
            n_mismatch: 0,
            n_unverifiable: 0,
            n_pending: 1,
            n_suspicious: 0,
            verdicts: vec![verdict(
                claim("BRCA1", None),
                ClaimStatus::Pending {
                    reason: "no adjudication site".into(),
                },
            )],
            runtime_decision_log_path: None,
        };
        let rows = project_verdict_rows(&report, "diff_expr", std::path::Path::new("."));
        assert_eq!(rows[0]["status"], json!("pending"));
        assert_eq!(rows[0]["supported_by"], json!([]));
        assert_eq!(rows[0]["checked_against"], json!([]));
        assert_eq!(rows[0]["attempted_sources"], json!([]));
        assert_eq!(rows[0]["verdict_detail"], json!("no adjudication site"));
    }

    #[test]
    fn mismatch_projects_comparison_and_contradiction_links() {
        let report = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 0,
            n_mismatch: 1,
            n_unverifiable: 0,
            n_pending: 0,
            n_suspicious: 0,
            verdicts: vec![verdict(
                claim("IL6", Some("results/tables/de.csv")),
                ClaimStatus::Mismatch {
                    detail: "sign flip".into(),
                },
            )],
            runtime_decision_log_path: None,
        };
        let rows = project_verdict_rows(&report, "diff_expr", std::path::Path::new("."));
        assert_eq!(rows[0]["status"], json!("mismatch"));
        assert_eq!(rows[0]["supported_by"], json!(["results/tables/de.csv"]));
        assert_eq!(rows[0]["checked_against"], json!(["results/tables/de.csv"]));
        assert_eq!(rows[0]["contradicts"], json!(["results/tables/de.csv"]));
        assert_eq!(rows[0]["verdict_detail"], json!("sign flip"));
    }

    #[test]
    fn evidence_ref_resolves_upstream_basename_to_producing_task_dir() {
        // A terminal reporting stage cites an UPSTREAM table by basename; the
        // evidence @id must point at the PRODUCING task's dir, not the claiming
        // task's dir (which would dangle — the finding #2 defect).
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let producer = root.join("runtime/outputs/differential_expression");
        std::fs::create_dir_all(&producer).expect("mk producer");
        std::fs::write(producer.join("de_results.tsv"), b"gene\tlog2fc\n").expect("write table");
        // The claiming task has no such file of its own.
        std::fs::create_dir_all(root.join("runtime/outputs/final_reporting")).expect("mk claimer");

        let report = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 1,
            n_mismatch: 0,
            n_unverifiable: 0,
            n_pending: 0,
            n_suspicious: 0,
            verdicts: vec![verdict(
                claim("KLF15", Some("de_results.tsv")),
                ClaimStatus::Verified,
            )],
            runtime_decision_log_path: None,
        };
        let rows = project_verdict_rows(&report, "final_reporting", root);
        assert_eq!(
            rows[0]["supported_by"],
            json!(["runtime/outputs/differential_expression/de_results.tsv"]),
            "upstream basename must resolve to the producing task dir, not the claimer"
        );
    }

    #[test]
    fn evidence_ref_prefers_same_task_then_falls_back_when_ambiguous() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        // Same-task file present → keep the claiming task's own dir.
        let own = root.join("runtime/outputs/reporting");
        std::fs::create_dir_all(&own).expect("mk own");
        std::fs::write(own.join("summary.tsv"), b"x").expect("write own");
        // A different basename lives, ambiguously, in TWO producer dirs.
        for t in ["a_stage", "b_stage"] {
            let p = root.join(format!("runtime/outputs/{t}"));
            std::fs::create_dir_all(&p).expect("mk producer");
            std::fs::write(p.join("dup.tsv"), b"x").expect("write dup");
        }
        let mk = |basename: &str| ClaimVerificationReport {
            n_checked: 1,
            n_verified: 1,
            n_mismatch: 0,
            n_unverifiable: 0,
            n_pending: 0,
            n_suspicious: 0,
            verdicts: vec![verdict(claim("G", Some(basename)), ClaimStatus::Verified)],
            runtime_decision_log_path: None,
        };
        // same-task wins even though nothing else has summary.tsv
        let same = project_verdict_rows(&mk("summary.tsv"), "reporting", root);
        assert_eq!(
            same[0]["supported_by"],
            json!(["runtime/outputs/reporting/summary.tsv"])
        );
        // ambiguous basename (two producers, none is the claimer) → stable
        // claiming-task fallback rather than an arbitrary guess.
        let ambiguous = project_verdict_rows(&mk("dup.tsv"), "reporting", root);
        assert_eq!(
            ambiguous[0]["supported_by"],
            json!(["runtime/outputs/reporting/dup.tsv"])
        );
    }

    #[test]
    // build_sink_doc reads the global ECAA_ABLATE_CLAIM_CONSISTENCY flag, so it
    // must serialize against the ablation tests that set/unset it.
    #[serial_test::serial]
    fn sink_doc_carries_counts_version_and_verdicts() {
        let report = ClaimVerificationReport {
            n_checked: 2,
            n_verified: 1,
            n_mismatch: 1,
            n_unverifiable: 0,
            n_pending: 0,
            n_suspicious: 0,
            verdicts: vec![
                verdict(
                    claim("TP53", Some("results/tables/de.csv")),
                    ClaimStatus::Verified,
                ),
                verdict(
                    claim("IL6", Some("results/tables/de.csv")),
                    ClaimStatus::Mismatch {
                        detail: "sign flip".into(),
                    },
                ),
            ],
            runtime_decision_log_path: None,
        };
        let doc = build_sink_doc(&report, "diff_expr", None, std::path::Path::new("."));
        assert_eq!(doc["schema_version"], json!("1"));
        assert_eq!(doc["n_checked"], json!(2));
        assert_eq!(doc["n_mismatch"], json!(1));
        assert_eq!(doc["ecaa_version"], json!("0.2"));
        assert_eq!(doc["min_reader_version"], json!("0.2"));
        assert_eq!(doc["source"], json!("runtime-verifier"));
        assert_eq!(doc["task_id"], json!("diff_expr"));
        let verdicts = doc["verdicts"].as_array().unwrap();
        assert_eq!(verdicts.len(), 2);
        assert_eq!(verdicts[0]["status"], json!("verified"));
    }

    #[test]
    #[serial_test::serial]
    fn sink_doc_carries_coverage_when_present() {
        use crate::coverage::{CoverageResult, EntityCoverage};
        use std::collections::BTreeMap;
        let report = ClaimVerificationReport::empty();
        let mut per_entity = BTreeMap::new();
        per_entity.insert(
            "differential_expression".to_string(),
            EntityCoverage::Absent,
        );
        let cov = CoverageResult {
            required_total: 1,
            required_addressed: 0,
            required_unverifiable: 0,
            required_absent: 1,
            per_entity,
        };
        let doc = build_sink_doc(&report, "diff_expr", Some(&cov), std::path::Path::new("."));
        assert_eq!(doc["coverage"]["required_absent"], json!(1));
        assert_eq!(doc["coverage"]["required_total"], json!(1));
        // Absent the coverage arg, no coverage key is written (Phase-1 shape).
        let bare = build_sink_doc(&report, "diff_expr", None, std::path::Path::new("."));
        assert!(bare.get("coverage").is_none());
    }

    #[test]
    #[serial_test::serial]
    fn persisted_sink_verifies_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let report = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 1,
            n_mismatch: 0,
            n_unverifiable: 0,
            n_pending: 0,
            n_suspicious: 0,
            verdicts: vec![verdict(
                claim("TP53", Some("results/tables/de.csv")),
                ClaimStatus::Verified,
            )],
            runtime_decision_log_path: None,
        };
        let writer = AuditWriter::for_session();

        let path =
            persist_signed_verdicts(dir.path(), "diff_expr", &report, None, &writer).unwrap();
        assert_eq!(
            path,
            dir.path()
                .join("runtime/verification-reports/claim-verification.signed.json")
        );

        // The on-disk line is a single signed JSON row; verify_row strips _mac.
        let line = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(line.trim_end()).unwrap();
        assert!(parsed.get("_mac").is_some(), "sink must be signed");
        let inner = writer.verify_row(&parsed).expect("valid HMAC");
        assert_eq!(inner["verdicts"].as_array().unwrap().len(), 1);
        assert_eq!(inner["source"], json!("runtime-verifier"));
    }

    #[test]
    fn re_finalize_replaces_task_row_not_appends() {
        // Re-finalizing the same task must REPLACE its signed row, never leave a
        // stale earlier row behind (the append-only bug let the audit-proof
        // loader read pre-correction verdicts). Other tasks' rows — and their
        // independent signatures — must survive verbatim.
        let dir = tempfile::tempdir().unwrap();
        let writer = AuditWriter::for_session();
        let mk = |entity: &str, status: ClaimStatus| ClaimVerificationReport {
            n_checked: 1,
            n_verified: matches!(status, ClaimStatus::Verified) as usize,
            n_mismatch: matches!(status, ClaimStatus::Mismatch { .. }) as usize,
            n_unverifiable: matches!(status, ClaimStatus::Unverifiable { .. }) as usize,
            n_pending: matches!(status, ClaimStatus::Pending { .. }) as usize,
            n_suspicious: matches!(status, ClaimStatus::Suspicious { .. }) as usize,
            verdicts: vec![verdict(
                claim(entity, Some("results/tables/de.csv")),
                status,
            )],
            runtime_decision_log_path: None,
        };

        // Task A finalized once; task B once; then task A RE-finalized with a
        // different verdict (mismatch → verified).
        let path = persist_signed_verdicts(
            dir.path(),
            "task_a",
            &mk("IL6", ClaimStatus::Mismatch { detail: "x".into() }),
            None,
            &writer,
        )
        .unwrap();
        persist_signed_verdicts(
            dir.path(),
            "task_b",
            &mk("TP53", ClaimStatus::Verified),
            None,
            &writer,
        )
        .unwrap();
        persist_signed_verdicts(
            dir.path(),
            "task_a",
            &mk("IL6", ClaimStatus::Verified),
            None,
            &writer,
        )
        .unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
        // Exactly one row per task — no stale duplicate for task_a.
        assert_eq!(lines.len(), 2, "expected one row per task, got: {body}");
        let mut by_task: std::collections::BTreeMap<String, Value> = Default::default();
        for l in &lines {
            let parsed: Value = serde_json::from_str(l).unwrap();
            // Every surviving row must still carry a valid HMAC.
            let inner = writer
                .verify_row(&parsed)
                .expect("valid HMAC after rewrite");
            by_task.insert(inner["task_id"].as_str().unwrap().to_string(), inner);
        }
        // task_a's surviving row is the LATEST (verified), not the stale mismatch.
        assert_eq!(by_task["task_a"]["n_verified"], json!(1));
        assert_eq!(by_task["task_a"]["n_mismatch"], json!(0));
        assert_eq!(by_task["task_b"]["n_verified"], json!(1));
    }

    #[test]
    #[serial_test::serial]
    fn ablated_sink_doc_is_explicitly_empty_and_marked() {
        let env = crate::ablation::AblationFlag::ClaimConsistency.env_var();
        std::env::set_var(env, "1");
        let report = ClaimVerificationReport {
            n_checked: 2,
            n_verified: 2,
            n_mismatch: 0,
            n_unverifiable: 0,
            n_pending: 0,
            n_suspicious: 0,
            verdicts: vec![
                verdict(
                    claim("TP53", Some("results/tables/de.csv")),
                    ClaimStatus::Verified,
                ),
                verdict(
                    claim("IL6", Some("results/tables/de.csv")),
                    ClaimStatus::Verified,
                ),
            ],
            runtime_decision_log_path: None,
        };
        let doc = build_sink_doc(&report, "diff_expr", None, std::path::Path::new("."));
        std::env::remove_var(env);
        // Site 1: under ClaimConsistency the populated sink is suppressed —
        // the doc carries ZERO verdicts and an explicit ablation marker that
        // is distinct from the legacy emit-time stub (which has no such field).
        assert_eq!(doc["ablated"], json!(true));
        assert_eq!(doc["verdicts"].as_array().unwrap().len(), 0);
        assert_eq!(doc["n_verified"], json!(0));
        assert_eq!(doc["n_checked"], json!(0));
        assert_eq!(doc["source"], json!("runtime-verifier"));
    }

    #[test]
    #[serial_test::serial]
    fn ablated_sink_doc_suppresses_coverage_block() {
        use crate::coverage::{CoverageResult, EntityCoverage};
        use std::collections::BTreeMap;
        let env = crate::ablation::AblationFlag::ClaimConsistency.env_var();
        std::env::set_var(env, "1");
        let report = ClaimVerificationReport::empty();
        let mut per_entity = BTreeMap::new();
        per_entity.insert(
            "differential_expression".to_string(),
            EntityCoverage::Absent,
        );
        let cov = CoverageResult {
            required_total: 1,
            required_addressed: 0,
            required_unverifiable: 0,
            required_absent: 1,
            per_entity,
        };
        let doc = build_sink_doc(&report, "diff_expr", Some(&cov), std::path::Path::new("."));
        std::env::remove_var(env);
        // The recall floor reads the coverage block from the signed sink; the
        // ablated arm must not carry it, else the floor sees real content and
        // the contrast reduces to a status flip.
        assert_eq!(doc["ablated"], json!(true));
        assert!(doc.get("coverage").is_none());
    }

    fn verified_report(entity: &str) -> ClaimVerificationReport {
        ClaimVerificationReport {
            n_checked: 1,
            n_verified: 1,
            n_mismatch: 0,
            n_unverifiable: 0,
            n_pending: 0,
            n_suspicious: 0,
            verdicts: vec![verdict(
                claim(entity, Some("results/tables/de.csv")),
                ClaimStatus::Verified,
            )],
            runtime_decision_log_path: None,
        }
    }

    fn read_plaintext(root: &Path) -> Value {
        let raw = std::fs::read_to_string(root.join(PLAINTEXT_SIDECAR_REL)).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    // These read the global ECAA_ABLATE_CLAIM_CONSISTENCY flag, so they must
    // serialize against the ablation test (and each other) to avoid observing
    // a mid-window flag flip from a parallel test in the same binary.
    #[test]
    #[serial_test::serial]
    fn refresh_plaintext_populates_n_checked_and_verdicts() {
        let dir = tempfile::tempdir().unwrap();
        refresh_plaintext_sidecar(dir.path(), "task_a", &verified_report("TP53")).unwrap();
        let doc = read_plaintext(dir.path());
        assert_eq!(doc["schema_version"], json!("1"));
        assert_eq!(doc["n_checked"], json!(1));
        assert_eq!(doc["n_verified"], json!(1));
        assert_eq!(doc["verdicts"].as_array().unwrap().len(), 1);
        assert_eq!(doc["verdicts"][0]["claim_id"], json!("task_a#claim-0"));
    }

    #[test]
    #[serial_test::serial]
    fn refresh_plaintext_aggregates_across_tasks() {
        let dir = tempfile::tempdir().unwrap();
        refresh_plaintext_sidecar(dir.path(), "task_a", &verified_report("TP53")).unwrap();
        refresh_plaintext_sidecar(dir.path(), "task_b", &verified_report("IL6")).unwrap();
        let doc = read_plaintext(dir.path());
        // Both tasks accumulate; counts reflect the union.
        assert_eq!(doc["n_checked"], json!(2));
        assert_eq!(doc["n_verified"], json!(2));
        let ids: Vec<&str> = doc["verdicts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["claim_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["task_a#claim-0", "task_b#claim-0"]);
    }

    #[test]
    #[serial_test::serial]
    fn refresh_plaintext_is_idempotent_per_task() {
        let dir = tempfile::tempdir().unwrap();
        // Finalize task_a twice — its rows must be REPLACED, not doubled.
        refresh_plaintext_sidecar(dir.path(), "task_a", &verified_report("TP53")).unwrap();
        refresh_plaintext_sidecar(dir.path(), "task_b", &verified_report("IL6")).unwrap();
        refresh_plaintext_sidecar(dir.path(), "task_a", &verified_report("TP53")).unwrap();
        let doc = read_plaintext(dir.path());
        assert_eq!(
            doc["n_checked"],
            json!(2),
            "re-finalizing task_a must not double-count"
        );
        assert_eq!(doc["verdicts"].as_array().unwrap().len(), 2);
    }

    #[test]
    #[serial_test::serial]
    fn refresh_plaintext_suppresses_rows_under_ablation() {
        let env = crate::ablation::AblationFlag::ClaimConsistency.env_var();
        std::env::set_var(env, "1");
        let dir = tempfile::tempdir().unwrap();
        let res = refresh_plaintext_sidecar(dir.path(), "task_a", &verified_report("TP53"));
        std::env::remove_var(env);
        res.unwrap();
        let doc = read_plaintext(dir.path());
        // Under the claim-consistency ablation this task contributes zero rows,
        // mirroring the signed-sink suppression (Site 1).
        assert_eq!(doc["n_checked"], json!(0));
        assert_eq!(doc["verdicts"].as_array().unwrap().len(), 0);
    }

    #[test]
    #[serial_test::serial]
    fn unablated_sink_doc_has_no_ablated_marker() {
        // Belt-and-braces: with the flag off, the marker is false and verdicts
        // populate — proving the contrast is enforcement, not enum.
        let env = crate::ablation::AblationFlag::ClaimConsistency.env_var();
        std::env::remove_var(env);
        let report = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 1,
            n_mismatch: 0,
            n_unverifiable: 0,
            n_pending: 0,
            n_suspicious: 0,
            verdicts: vec![verdict(
                claim("TP53", Some("results/tables/de.csv")),
                ClaimStatus::Verified,
            )],
            runtime_decision_log_path: None,
        };
        let doc = build_sink_doc(&report, "diff_expr", None, std::path::Path::new("."));
        assert_eq!(doc["ablated"], json!(false));
        assert_eq!(doc["verdicts"].as_array().unwrap().len(), 1);
    }
}
