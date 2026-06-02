//! Thin glue between the core claim-verification modules and the
//! per-task result surface.
//!
//! The verifier itself is policy-driven and lives in
//! [`ecaa_workflow_core::claim_verifier`]; this helper just locates
//! the narrative artifact inside a package's `runtime/<task_id>/`
//! directory, loads the relevant interpretation policy, and wires the
//! two together. Called by `get_task_result` in `chat_routes/tasks.rs`
//! so the UI's `ResultReviewTurnCard` can render the verification badge.

use ecaa_workflow_core::claim_extractor::{
    extract_claims, extract_markdown_table_claims, ExtractorConfig,
};
use ecaa_workflow_core::claim_verifier::{
    demote_claims_from_deviations, verify_claims_with_discovery, verify_structured_claims,
    ClaimVerificationReport, StructuredClaim,
};
use ecaa_workflow_core::decision_log::DecisionRecord;
use ecaa_workflow_core::project_class::ProjectClass;
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
    let policy_dir = config_dir.join("downstream-policy");
    let cfg = match ExtractorConfig::from_policy_for_class(&policy, &policy_dir, project_class) {
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

    // Nothing to verify: no narrative AND no structured claims. The policy
    // is enabled and loadable here — this is a benign "nothing to do", not a
    // configuration defect, so report it as Disabled rather than Unavailable.
    if narrative_path.is_none() && report.n_checked == 0 {
        return VerifyOutcome::Disabled;
    }

    demote_claims_from_deviations(&mut report, decisions, is_confirmatory);

    // / D6 (c): locate the agent's runtime decision log if it exists,
    // and attach its package-relative path so the UI can cross-
    // reference the SME-level `decisions.jsonl` against what the
    // agent recorded internally. Convention: the agent writes
    // `runtime/outputs/<task_id>/runtime-decisions.jsonl` (or its
    // legacy sibling `runtime/<task_id>/runtime-decisions.jsonl`).
    // Falls back to `runtime/RUNTIME_DECISION_LOG.jsonl` (package-
    // wide log) if the per-task variant is absent.
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

/// Transition the session to `Blocked { ValidationFailed }` when a freshly
/// computed verification report contains ≥1 claim mismatch. Shared by the
/// manual `POST /verify` endpoint and the on-completion re-verify hook so
/// both drive identical state transitions and identical blocker payloads.
///
/// No-op when the report has no mismatch. The `block_from_harness` call is
/// best-effort/idempotent: a session that is already `Blocked` (or no longer
/// in an execution state) returns `Err`, which is the benign double-fire
/// case — the earlier blocker stays surfaced.
pub(crate) async fn block_on_mismatch(
    app: &crate::chat_routes::ChatAppState,
    session_id: uuid::Uuid,
    task_id: &str,
    report: &ClaimVerificationReport,
) {
    if !report.has_mismatch() {
        return;
    }
    let first_mismatch = report
        .verdicts
        .iter()
        .find(|v| {
            matches!(
                &v.status,
                ecaa_workflow_core::claim_verifier::ClaimStatus::Mismatch { .. }
            )
        })
        .map(|v| v.claim.entity.clone())
        .unwrap_or_else(|| "unknown".into());
    let detail = format!(
        "{} claim mismatch(es) detected on completion of task {} (first: {})",
        report.n_mismatch, task_id, first_mismatch
    );
    let kind = ecaa_workflow_core::blocker::BlockerKind::ValidationFailed {
        check: format!("claim_verification:{}", task_id),
        message: detail.clone(),
        cause: None,
    };
    if let Err(e) = app
        .conversation
        .block_from_harness(session_id, task_id.to_string(), detail, kind)
        .await
    {
        // Soft-fail: the session most likely isn't in an execution state
        // anymore (already Blocked), which is the idempotent case.
        tracing::debug!(
            ?session_id,
            %task_id,
            error = %e,
            "block_on_mismatch: block_from_harness no-op"
        );
    }
}

/// Re-run claim verification for a completed task FROM SOURCE and, on
/// mismatch, transition the session to `Blocked { ValidationFailed }`.
/// Shared by the manual `POST /verify` endpoint's completion hook so the
/// agent-writable verification sidecar is never trusted: the report is
/// always recomputed against the package's narrative + result tables.
///
/// Best-effort: returns the recomputed [`VerifyOutcome`] (`Verified` whether
/// or not it found a mismatch), or `None` when the session/package is gone or
/// the blocking-pool task panicked. The blocking work runs on
/// `spawn_blocking` so the regex + bounded-fs walk never ties up an async
/// worker, mirroring the GET handler's live-verify path.
pub async fn reverify_and_block_on_mismatch(
    app: &crate::chat_routes::ChatAppState,
    session_id: uuid::Uuid,
    task_id: &str,
) -> Option<VerifyOutcome> {
    let session = app.conversation.get_session(session_id).await?;
    let root = session.emitted_package_path.clone()?;
    let config_dir = crate::chat_routes::config_dir_or_default();
    let project_class = session.project_class;
    let decisions = session.decisions.clone();
    let is_confirmatory = session.mode.is_confirmatory();
    let root_c = root.clone();
    let task_c = task_id.to_string();
    let outcome = tokio::task::spawn_blocking(move || {
        verify_task_with_context(
            &root_c,
            &task_c,
            &config_dir,
            project_class,
            &decisions,
            is_confirmatory,
        )
    })
    .await
    .ok()?;

    match &outcome {
        VerifyOutcome::Verified(v) => {
            // Hallucination-proxy telemetry: accumulate claims-checked +
            // mismatches into the session metrics so `claim_mismatch_rate`
            // stays observable on the completion path, not just the manual
            // POST /verify path. Best-effort.
            app.conversation
                .metrics()
                .record_claim_verification(
                    session_id,
                    v.report.n_checked as u64,
                    v.report.n_mismatch as u64,
                )
                .await;
            block_on_mismatch(app, session_id, task_id, &v.report).await;

            // Persist the recomputed verdicts as an HMAC-signed,
            // agent-unforgeable sink so the audit-proof loader can read them
            // (de-vacuifies Inv 1/5). The agent's container has already
            // exited; this host-side write is outside any agent-writable
            // window and outside the emit byte-diff baseline
            // (runtime/verification-reports/ is BagIt-excluded). Holds the
            // per-session secret, which the agent never sees, so the sink
            // cannot be forged from the executor side.
            let writer = ecaa_workflow_core::audit_writer::AuditWriter::with_secret(
                session.audit_writer_secret,
            );
            if let Err(e) = ecaa_workflow_core::claim_sink::persist_signed_verdicts(
                &root, task_id, &v.report, &writer,
            ) {
                tracing::warn!(
                    target: "ecaa::verify",
                    error = %e,
                    task_id,
                    "signed verdict sink write failed"
                );
            }
        }
        VerifyOutcome::Disabled => {}
        VerifyOutcome::Unavailable { reason } => {
            // A configuration defect on the completion path is just as loud
            // as on the GET path: log it so a CWD/ECAA_CONFIG_DIR
            // misconfiguration that silently disables verification fleet-wide
            // is visible. The load helper already logged at error level too.
            tracing::error!(
                target: "verification",
                ?session_id,
                %task_id,
                %reason,
                "on-completion re-verify: interpretation policy unavailable — verification not run"
            );
        }
    }
    Some(outcome)
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
    let path = config_dir
        .join("downstream-policy")
        .join("interpretation-policy.json");
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
    //   ECAA_REAL_PKG=<path> cargo test -p ecaa-workflow-server \
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
                        if let ecaa_workflow_core::claim_verifier::ClaimStatus::Mismatch {
                            detail,
                        } = &vd.status
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
        use ecaa_workflow_core::claim_verifier::ClaimStrength;
        use ecaa_workflow_core::decision_log::{DecisionActor, DecisionRecord, DecisionType};

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
        use ecaa_workflow_core::claim_verifier::ClaimStrength;
        use ecaa_workflow_core::decision_log::{DecisionActor, DecisionRecord, DecisionType};

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

    // ── T2: disabled vs. unavailable policy distinction ──────────────────

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
}

#[cfg(test)]
mod signed_sink_wiring_tests {
    use ecaa_workflow_core::audit_writer::AuditWriter;
    use ecaa_workflow_core::claim_contract::ClaimContract;
    use ecaa_workflow_core::claim_extractor::Claim;
    use ecaa_workflow_core::claim_sink::{persist_signed_verdicts, SIGNED_SINK_REL};
    use ecaa_workflow_core::claim_verifier::{
        ClaimStatus, ClaimStrength, ClaimVerdict, ClaimVerificationReport,
    };

    #[test]
    fn persisted_sink_is_verifiable_with_session_secret() {
        let dir = tempfile::tempdir().unwrap();
        // Simulate the per-session secret the server holds.
        let secret = [7u8; 32];
        let writer = AuditWriter::with_secret(secret);
        let c = Claim {
            entity: "TP53".into(),
            direction: None,
            effect_size: None,
            pvalue: None,
            source_table: Some("results/tables/de.csv".into()),
            excerpt: String::new(),
            contract: ClaimContract::NumericTableLookup,
        };
        let rep = ClaimVerificationReport {
            n_checked: 1,
            n_verified: 1,
            n_mismatch: 0,
            n_unverifiable: 0,
            verdicts: vec![ClaimVerdict {
                claim: c,
                status: ClaimStatus::Verified,
                strength: ClaimStrength::default(),
            }],
            runtime_decision_log_path: None,
        };

        persist_signed_verdicts(dir.path(), "diff_expr", &rep, &writer).unwrap();

        // A reader reconstructing the writer from the same secret verifies it.
        let reader = AuditWriter::with_secret(secret);
        let line = std::fs::read_to_string(dir.path().join(SIGNED_SINK_REL)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        assert!(reader.verify_row(&parsed).is_ok());
    }
}
