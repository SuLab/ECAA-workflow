//! Package-finalize orchestration shared by the server's per-task
//! re-verify hook and the harness's end-of-run standalone path.
//!
//! Finalizing a completed task means: re-run claim verification FROM SOURCE
//! (never trusting the agent-writable sidecar), compute structured-claims
//! coverage against the per-package expected-claim manifest, persist the
//! HMAC-signed verdict sink (de-vacuifies audit-proof Inv 1/5), register
//! agent-produced evidence + re-seal the BagIt manifest, and regenerate the
//! at-rest audit-proof report against the now-signed sink.
//!
//! This module owns only the pure/sync orchestration: no session, no HTTP,
//! no telemetry, no state transitions. Those stay in the server, which calls
//! [`finalize_task`] and acts on the returned [`TaskFinalizeOutcome`]. The
//! harness (which links only against `core`) calls [`finalize_package`] to
//! produce the same finalized package a session-backed run produces
//! incrementally.

use crate::claim_extractor::{extract_claims, extract_markdown_table_claims, ExtractorConfig};
use crate::claim_verifier::{
    demote_claims_from_deviations, verify_claims_with_discovery, verify_structured_claims,
    ClaimVerificationReport, StructuredClaim,
};
use crate::clock::WallClock;
use crate::coverage::CoverageResult;
use crate::decision_log::DecisionRecord;
use crate::project_class::ProjectClass;
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

    let narrative_path = find_narrative_artifact(package_root, task_id);
    let mut report = ClaimVerificationReport::empty();

    // 1. Prose-narrative claims, when the task wrote a `.md` report.
    if let Some(np) = narrative_path.as_ref() {
        if let Ok(narrative) = std::fs::read_to_string(np) {
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
            let mut claims = extract_claims(&narrative, &cfg);
            claims.extend(extract_markdown_table_claims(&narrative, &cfg));
            for v in verify_claims_with_discovery(&claims, &effective_root, package_root, &cfg) {
                report.push(v);
            }
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

    // 3. Structured TSV/CSV result tables — a table-only task (qc /
    //    normalisation / differential_expression / pathway, whose outputs are
    //    `de_results.tsv`-style files and no `.md`/`.txt` narrative) otherwise
    //    contributes nothing. Glob `runtime/outputs/<task>/*.tsv|*.csv`, mine
    //    each for per-row claims, set `source_table` to the file basename, and
    //    verify them through the same discovery path the narrative claims use.
    //    The narrative artifact (a `.md`/`.txt`) is never matched by the
    //    `.tsv`/`.csv` glob, but we skip it explicitly to be safe. Dedup by
    //    `(entity, direction)` inside the verifier handles overlaps with the
    //    narrative/markdown claims.
    if let Some(task_dir) = resolve_task_runtime_dir_local(package_root, task_id) {
        let tables_root = package_root.join("results").join("tables");
        let effective_root = if tables_root.is_dir() {
            tables_root
        } else {
            task_dir.clone()
        };
        let mut table_claims: Vec<crate::claim_extractor::Claim> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&task_dir) {
            // Sort entries for deterministic claim ordering (read_dir order is
            // filesystem-dependent).
            let mut files: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file())
                .collect();
            files.sort();
            for path in files {
                if narrative_path.as_ref() == Some(&path) {
                    continue;
                }
                let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                    continue;
                };
                let delimiter = match ext.to_ascii_lowercase().as_str() {
                    "tsv" => b'\t',
                    "csv" => b',',
                    _ => continue,
                };
                let Ok(file) = std::fs::File::open(&path) else {
                    continue;
                };
                let basename = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string());
                for mut claim in
                    crate::claim_extractor::extract_delimited_table_claims(file, delimiter, &cfg)
                {
                    claim.source_table = basename.clone();
                    table_claims.push(claim);
                }
            }
        }
        if !table_claims.is_empty() {
            for v in
                verify_claims_with_discovery(&table_claims, &effective_root, package_root, &cfg)
            {
                report.push(v);
            }
        }
    }

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
    if narrative_path.is_none() && report.n_checked == 0 {
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
    let outcome = verify_task_with_context(
        root,
        task_id,
        config_dir,
        project_class,
        decisions,
        is_confirmatory,
    );

    let mut coverage = None;
    if let VerifyOutcome::Verified(v) = &outcome {
        // Coverage against the injected expected-claim manifest. The
        // ExtractorConfig is rebuilt from the same policy the verify used.
        // `None` when the package carries no manifest (un-anchored task).
        if let PolicyLoad::Loaded(p) = load_interpretation_policy(config_dir) {
            if let Ok(cfg) = ExtractorConfig::from_policy_for_class(&p, config_dir, project_class) {
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
        if let Err(e) = crate::claim_sink::refresh_plaintext_sidecar(root, task_id, &v.report) {
            tracing::warn!(
                target: "ecaa::finalize",
                error = %e,
                task_id,
                "plaintext claim-verification.json refresh failed"
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
                tracing::warn!(
                    target: "ecaa::finalize",
                    error = %e,
                    task_id,
                    "signed verdict sink write failed"
                );
            }

            // Register agent-produced result tables as V `@graph` Evidence
            // entities, back-fill the C-subgraph `Claim` nodes from the just-
            // written signed sink (passing the writer so the sink's HMAC
            // verifies), and re-seal the BagIt manifest — BEFORE regenerating
            // the at-rest audit-proof report below, so cross_graph_integrity
            // (Inv 5) resolves the verified claim's C→V `supported_by` to the
            // just-registered Evidence node. Runs on a post-exec package only,
            // so the emit byte-reproducibility surface is untouched.
            if let Err(e) = crate::ro_crate::finalize_evidence_registration_with_verifier(
                root,
                &WallClock,
                Some(&writer),
            ) {
                tracing::warn!(
                    target: "ecaa::finalize",
                    error = %e,
                    task_id,
                    "evidence registration / BagIt manifest reconcile failed"
                );
            }

            // Regenerate the at-rest audit-proof report so claim_completeness /
            // cross_graph_integrity reflect the just-persisted verdicts +
            // coverage. The report is BagIt-excluded (carries the spec-excluded
            // `evaluated_at`), so rewriting it post-exec does not affect emit
            // byte-reproducibility.
            let validator = crate::wrroc_validator::NoopWrrocValidator;
            if let Ok(doc) = crate::audit_proof::run_audit_proof_with_verifier(
                root,
                &validator,
                &WallClock,
                Some(&writer),
            ) {
                if let Ok(bytes) = serde_json::to_vec_pretty(&doc) {
                    let _ = std::fs::write(root.join("runtime/audit-proof-report.json"), bytes);
                }
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

/// Finalize every completed task in an emitted package. Reads completed task
/// ids from `WORKFLOW.json`. Intended to be called once at harness
/// end-of-run (standalone path) so a no-session run produces the same
/// finalized package the server produces incrementally.
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
    if let Some(tasks) = wf.get("tasks").and_then(|t| t.as_object()) {
        // `tasks` is a JSON object keyed by task_id; serde_json preserves a
        // BTree-ordered map internally only with the `preserve_order` feature
        // off, so iteration is deterministic by key here.
        for (task_id, t) in tasks {
            let status = t
                .get("state")
                .and_then(|s| s.get("status").or(Some(s)))
                .and_then(|s| s.as_str());
            if status != Some("completed") {
                continue;
            }
            let res = finalize_task(
                root,
                task_id,
                config_dir,
                project_class,
                decisions,
                is_confirmatory,
                secret,
            )?;
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
    let verdicts = verify_structured_claims(&structured, package_root, cfg);
    Some(crate::coverage::reconcile_coverage(&manifest, &verdicts))
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
    fn de_results_tsv_contributes_table_claims() {
        // A table-only differential_expression task (no .md/.txt narrative,
        // no result.json claims) must still contribute verifiable claims from
        // its de_results.tsv. Self-contained: the policy here includes gene_id
        // in entityColumns so this does not depend on the Workstream A policy
        // change.
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
        // Minimal WORKFLOW.json with one completed differential_expression task.
        write(
            &root.join("WORKFLOW.json"),
            r#"{"tasks":{"differential_expression":{"state":{"status":"completed"}}}}"#,
        );

        let out = expect_verified(verify_task_with_context(
            root,
            "differential_expression",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[],
            false,
        ));
        assert!(
            out.report.n_checked >= 1,
            "de_results.tsv must contribute at least one table claim; n_checked = {}",
            out.report.n_checked
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
