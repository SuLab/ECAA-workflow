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
    let report = ecaa_workflow_core::cross_version_diff::diff_packages(
        &parent_path,
        output_dir,
        &diff_cfg,
    )
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
