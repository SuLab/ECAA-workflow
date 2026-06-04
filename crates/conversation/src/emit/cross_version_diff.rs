//! Cross-version diff emission. When a session is branched from an
//! emitted parent package, compute a per-row concordance report over the
//! two packages' `results/tables/*.{csv,tsv}` and write it to
//! `runtime/cross-version-diff.json` alongside per-table CSVs. The
//! resulting table names are returned so `ro_crate::patch_ro_crate_metadata`
//! can register each CSV as its own CreativeWork.
//!
//! Also appends a `DecisionType::CrossVersionDiff` record to the
//! session's in-memory `decisions` vec so the subsequent
//! `audit_log::write_decision_log` call persists it alongside the other
//! records.

use crate::session::Session;
use anyhow::{Context, Result};
use std::path::Path;

pub(super) async fn write_cross_version_diff(
    session: &mut Session,
    output_dir: &Path,
) -> Result<Vec<String>> {
    use std::fmt::Write;
    // Resolve the parent package path from EITHER source under
    // unified EmissionLineage. Branch sessions carry the parent via
    // `session.lineage.parent_emitted_package_path`; amend re-emissions
    // carry it via `session.pending_amendment.parent_package_path`
    // (captured at AmendStart-time before `emit_package` overwrites
    // `session.emitted_package_path` with the child path). Both
    // sources must fire the diff; without dual resolution, the IVD
    // v1→v5 amend chain produces zero concordance reports.
    let parent_path: std::path::PathBuf = match (
        session
            .lineage
            .as_ref()
            .and_then(|l| l.parent_emitted_package_path.clone()),
        session
            .pending_amendment
            .as_ref()
            .map(|a| a.parent_package_path.clone()),
    ) {
        (Some(p), _) => p,
        (None, Some(p)) => p,
        (None, None) => return Ok(Vec::new()),
    };
    if !parent_path.exists() {
        return Ok(Vec::new());
    }

    // 2. Load policy for the diff config. The taxonomy doesn't carry
    // crossVersionDiff config directly today, so fall back to the
    // interpretation-policy.json inside the freshly-emitted child
    // package (which is written by core::emitter for each policy).
    let policy_json = load_interpretation_policy(output_dir)
        .await
        .unwrap_or_else(|_| serde_json::Value::Object(Default::default()));
    let diff_cfg =
        ecaa_workflow_core::cross_version_diff::CrossVersionConfig::from_policy(&policy_json);

    // 3. Run the diff (sync, pure Rust).
    let report =
        ecaa_workflow_core::cross_version_diff::diff_packages(&parent_path, output_dir, &diff_cfg)
            .context("cross_version_diff::diff_packages")?;

    // 4. Write the JSON report + per-table CSVs.
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let json_path = runtime.join("cross-version-diff.json");
    let body = serde_json::to_vec_pretty(&report)?;
    // Atomic-write so concurrent GET /cross-version-diff readers
    // never observe partial bytes mid-write.
    crate::persistence::atomic_write_bytes_to(&json_path, &body)
        .await
        .with_context(|| format!("writing {}", json_path.display()))?;

    let mut table_names: Vec<String> = Vec::new();
    for table in &report.tables {
        let safe = sanitize_filename(&table.table_name);
        let csv_name = format!("cross-version-diff-{}.csv", safe);
        let csv_path = runtime.join(&csv_name);
        let mut csv = String::from(
            "entity,classification,parent_effect,child_effect,parent_pvalue_raw,parent_pvalue_adjusted,child_pvalue_raw,child_pvalue_adjusted,correlation_contribution\n",
        );
        // Pre-allocate based on row count (~200 bytes typical row).
        csv.reserve(table.rows.len() * 200);
        for row in &table.rows {
            let fmt_opt = |v: Option<f64>| v.map(|x| x.to_string()).unwrap_or_default();
            let classification = serde_json::to_string(&row.classification).unwrap_or_default();
            let classification = classification.trim_matches('"');
            writeln!(
                csv,
                "{},{},{},{},{},{},{},{},{}",
                row.entity,
                classification,
                fmt_opt(row.parent_effect),
                fmt_opt(row.child_effect),
                fmt_opt(row.parent_pvalue_raw),
                fmt_opt(row.parent_pvalue_adjusted),
                fmt_opt(row.child_pvalue_raw),
                fmt_opt(row.child_pvalue_adjusted),
                row.effect_correlation_contribution,
            )
            .expect("writing to String never fails");
        }
        // Same atomicity story as the JSON write above — the
        // per-table CSVs are served by /cross-version-diff/:table_name
        // and must never expose a half-written body to a poller.
        crate::persistence::atomic_write_bytes_to(&csv_path, csv.as_bytes()).await?;
        table_names.push(csv_name);
    }

    // 5. Append the DecisionRecord so it lands in decisions.jsonl.
    let n_discordant: usize = report.tables.iter().map(|t| t.n_discordant).sum();
    session
        .decisions
        .push(ecaa_workflow_core::decision_log::DecisionRecord::new(
            session.id.to_string(),
            ecaa_workflow_core::decision_log::DecisionType::CrossVersionDiff {
                parent_package: report.parent_package.clone(),
                child_package: report.child_package.clone(),
                overall_concordance: report.overall_concordance,
                n_discordant,
            },
            ecaa_workflow_core::decision_log::DecisionActor::Harness,
            None,
        ));

    Ok(table_names)
}

/// Write `runtime/ed-cf-delta.json` when this emission has a lineage
/// parent. Reads BOTH packages' `runtime/ed-cf-self-assessment.json`,
/// computes `EdCfDelta::between(parent, child)`, and writes the delta.
/// Best-effort: a missing parent (or missing parent assessment) returns
/// `Ok(())` with no file — never fails emit. Uses the SAME parent-path
/// resolution as `write_cross_version_diff` (branch lineage OR pending
/// amendment). Excluded from the byte-diff baseline.
pub(super) async fn write_ed_cf_delta(session: &Session, output_dir: &Path) -> Result<()> {
    let parent_path: std::path::PathBuf = match (
        session
            .lineage
            .as_ref()
            .and_then(|l| l.parent_emitted_package_path.clone()),
        session
            .pending_amendment
            .as_ref()
            .map(|a| a.parent_package_path.clone()),
    ) {
        (Some(p), _) => p,
        (None, Some(p)) => p,
        (None, None) => return Ok(()),
    };

    let parent = match read_ed_cf_assessment(&parent_path).await {
        Some(a) => a,
        None => return Ok(()), // parent has no assessment; nothing to diff
    };
    let child = match read_ed_cf_assessment(output_dir).await {
        Some(a) => a,
        None => return Ok(()), // child assessment not emitted; skip
    };

    let delta = ecaa_workflow_core::rubric_self_assessment::EdCfDelta::between(&parent, &child);
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let body = serde_json::to_vec_pretty(&delta)?;
    let path = runtime.join("ed-cf-delta.json");
    crate::persistence::atomic_write_bytes_to(&path, &body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Read + parse a package's `runtime/ed-cf-self-assessment.json` into an
/// `EdCfSelfAssessment`. Returns `None` when the file is absent or
/// unparseable (best-effort — the delta is informational).
async fn read_ed_cf_assessment(
    package: &Path,
) -> Option<ecaa_workflow_core::rubric_self_assessment::EdCfSelfAssessment> {
    let p = package.join("runtime/ed-cf-self-assessment.json");
    let bytes = tokio::fs::read(&p).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write `runtime/coverage-statement.json` when the session carries a
/// not-fully-covered `coverage_confidence` (CC1-4). This durably records
/// the catalog-coverage gap outside the UI so a reviewer reading the
/// emitted package sees which modalities fell outside the validated
/// catalog. Fully-covered (or absent) coverage writes no file — a clean
/// package carries no gap statement. Excluded from the byte-diff baseline.
pub(super) async fn write_coverage_statement(session: &Session, output_dir: &Path) -> Result<()> {
    let Some(cov) = session.coverage_confidence.as_ref() else {
        return Ok(());
    };
    if cov.fully_covered {
        return Ok(());
    }
    let runtime = output_dir.join("runtime");
    tokio::fs::create_dir_all(&runtime).await?;
    let body = serde_json::to_vec_pretty(cov)?;
    let path = runtime.join("coverage-statement.json");
    crate::persistence::atomic_write_bytes_to(&path, &body)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

async fn load_interpretation_policy(package: &Path) -> Result<serde_json::Value> {
    let p = package.join("policies/interpretation-policy.json");
    let bytes = tokio::fs::read(&p)
        .await
        .with_context(|| format!("reading {}", p.display()))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod ed_cf_delta_tests {
    use super::*;
    use crate::session::Session;
    use ecaa_workflow_core::rubric_self_assessment::{AssessmentInputs, EdCfSelfAssessment};

    async fn write_assessment(dir: &Path, a: &EdCfSelfAssessment) {
        let runtime = dir.join("runtime");
        tokio::fs::create_dir_all(&runtime).await.unwrap();
        let body = serde_json::to_vec_pretty(a).unwrap();
        tokio::fs::write(runtime.join("ed-cf-self-assessment.json"), body)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn writes_delta_when_parent_assessment_present() {
        let parent_dir = tempfile::tempdir().unwrap();
        let child_dir = tempfile::tempdir().unwrap();

        // Parent lacks the LLM-assisted-authoring ED mechanism; child has it.
        let parent_assessment = EdCfSelfAssessment::from_inputs(&{
            let mut i = AssessmentInputs::from_package_facts(93, 21, 22, 6);
            i.llm_assisted_authoring_present = false;
            i
        });
        let child_assessment =
            EdCfSelfAssessment::from_inputs(&AssessmentInputs::from_package_facts(93, 21, 22, 6));
        write_assessment(parent_dir.path(), &parent_assessment).await;
        write_assessment(child_dir.path(), &child_assessment).await;

        // Seed a child session whose lineage points at the parent dir.
        let mut session = Session::new(false);
        let mut lineage = session.lineage.clone().unwrap_or_else(|| {
            // Build a minimal lineage from a fresh parent branch.
            let parent = Session::new(false);
            Session::branch_from(&parent, false)
                .lineage
                .expect("branch sets lineage")
        });
        lineage.parent_emitted_package_path = Some(parent_dir.path().to_path_buf());
        session.lineage = Some(lineage);

        write_ed_cf_delta(&session, child_dir.path()).await.unwrap();

        let delta_path = child_dir.path().join("runtime/ed-cf-delta.json");
        assert!(delta_path.exists(), "ed-cf-delta.json must be written");
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&delta_path).unwrap()).unwrap();
        assert!(
            v["extensibility_delta"].as_f64().unwrap() > 0.0,
            "child gained an ED mechanism → positive delta"
        );
    }

    #[tokio::test]
    async fn no_delta_without_lineage_parent() {
        let child_dir = tempfile::tempdir().unwrap();
        let child_assessment =
            EdCfSelfAssessment::from_inputs(&AssessmentInputs::from_package_facts(93, 21, 22, 6));
        write_assessment(child_dir.path(), &child_assessment).await;
        let session = Session::new(false); // no lineage, no pending amendment
        write_ed_cf_delta(&session, child_dir.path()).await.unwrap();
        assert!(
            !child_dir.path().join("runtime/ed-cf-delta.json").exists(),
            "no parent → no delta file"
        );
    }

    #[tokio::test]
    async fn coverage_statement_written_only_when_not_fully_covered() {
        use crate::session::state::CoverageConfidence;
        use ecaa_workflow_core::workflow_contracts::outcome::{ComposeOutcome, GapReport};
        use ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag;

        // Not fully covered → file written with the uncovered modality.
        let dir = tempfile::tempdir().unwrap();
        let outcome = ComposeOutcome::PartialDag {
            dag: WorkflowDag::default(),
            unresolved_gaps: vec![GapReport {
                id: "unsatisfiable_modality:cytof".into(),
                statement: "no satisfier".into(),
                missing_port: None,
                suggestions: vec![],
            }],
        };
        let mut session = Session::new(false);
        session.coverage_confidence = Some(CoverageConfidence::from_outcome(&outcome));
        write_coverage_statement(&session, dir.path())
            .await
            .unwrap();
        let path = dir.path().join("runtime/coverage-statement.json");
        assert!(path.exists(), "partial coverage must write a statement");
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(v["uncovered_modalities"][0], "cytof");

        // Fully covered → no file.
        let dir2 = tempfile::tempdir().unwrap();
        let full = ComposeOutcome::ValidatedExecutableDag {
            dag: WorkflowDag::default(),
            report: Default::default(),
        };
        let mut s2 = Session::new(false);
        s2.coverage_confidence = Some(CoverageConfidence::from_outcome(&full));
        write_coverage_statement(&s2, dir2.path()).await.unwrap();
        assert!(
            !dir2.path().join("runtime/coverage-statement.json").exists(),
            "fully-covered package writes no coverage statement"
        );
    }
}
