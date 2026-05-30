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

/// Class-aware + confirmatory-aware task verifier. Picks the
/// `interpretation-policy.<class>.json` overlay,
/// runs the verifier, and then demotes claims whose supporting stage
/// lineage contains a `PostHocDeviation` record. Returns `None` for
/// any task without a narrative artifact or when the policy lacks a
/// `verifiableEntities` block — both treated as "nothing to verify"
/// rather than errors so the endpoint stays cheap in the common case.
pub fn verify_task_with_context(
    package_root: &Path,
    task_id: &str,
    config_dir: &Path,
    project_class: ProjectClass,
    decisions: &[DecisionRecord],
    is_confirmatory: bool,
) -> Option<TaskVerification> {
    let policy = load_interpretation_policy(config_dir)?;
    let policy_dir = config_dir.join("downstream-policy");
    let cfg = ExtractorConfig::from_policy_for_class(&policy, &policy_dir, project_class).ok()?;

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

    // Nothing to verify: no narrative AND no structured claims.
    if narrative_path.is_none() && report.n_checked == 0 {
        return None;
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

    Some(TaskVerification {
        narrative_path,
        report,
    })
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

fn load_interpretation_policy(config_dir: &Path) -> Option<serde_json::Value> {
    let path = config_dir
        .join("downstream-policy")
        .join("interpretation-policy.json");
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
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
                None => {}
                Some(v) => {
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

        let out = verify_task_with_context(
            pkg.path(),
            "task_interp",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[],
            false,
        )
        .unwrap();
        assert_eq!(out.report.n_verified, 1, "{:?}", out.report.verdicts);
        assert_eq!(out.report.n_mismatch, 0);
    }

    #[test]
    fn returns_none_when_no_narrative_artifact() {
        let pkg = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        scaffold_config_dir(cfg.path());
        // Empty runtime dir — no report.md
        fs::create_dir_all(pkg.path().join("runtime").join("t1")).unwrap();
        assert!(verify_task_with_context(
            pkg.path(),
            "t1",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[],
            false,
        )
        .is_none());
    }

    #[test]
    fn returns_none_when_policy_missing() {
        let pkg = tempdir().unwrap();
        let cfg = tempdir().unwrap();
        // No config/downstream-policy/interpretation-policy.json
        let task_dir = pkg.path().join("runtime").join("task_interp");
        write(&task_dir.join("report.md"), "ACAN was upregulated.\n");
        assert!(verify_task_with_context(
            pkg.path(),
            "task_interp",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[],
            false,
        )
        .is_none());
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
        let out = verify_task_with_context(
            pkg.path(),
            "task_interp",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[deviation],
            true,
        )
        .unwrap();
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
        let out = verify_task_with_context(
            pkg.path(),
            "task_interp",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[deviation],
            false,
        )
        .unwrap();
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

        let out = verify_task_with_context(
            pkg.path(),
            "task_interp",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[],
            false,
        )
        .unwrap();
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

        let out = verify_task_with_context(
            pkg.path(),
            "task_interp",
            cfg.path(),
            ProjectClass::Bioinformatics,
            &[],
            false,
        )
        .unwrap();
        assert!(out.report.has_mismatch(), "{:?}", out.report.verdicts);
    }
}
