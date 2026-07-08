// crates/core/src/replay/select.rs
//
// Deterministic compute-task selector for the replay path.
//
// Given a package directory, identifies which tasks are deterministic compute
// (eligible for re-execution) and which to skip with a reason.

use std::path::{Path, PathBuf};
use crate::replay::report::SkippedStage;

/// A task eligible for deterministic re-execution.
pub struct ComputeTask {
    pub task_id: String,
    pub scripts_dir: PathBuf,
    pub result_tables: Vec<String>,
}

// Task id prefixes that denote non-compute stages.
const EXCLUDE_PREFIX: &[&str] = &[
    "discover_",
    "validate_",
    "survey_",
    "contextualize_",
    "review_",
];

/// Returns `Some(reason)` when a task id should be excluded from re-execution.
///
/// Exclusion is checked in order:
/// 1. Task appears in `runtime/determinism-shim.json` `non_deterministic_stages`.
/// 2. Task is the data-ingestion stage (`data_acquisition`).
/// 3. Task id starts with a known non-compute prefix.
/// 4. Task id is `reporting`, `final_reporting`, or ends with `_reporting`.
fn is_excluded(id: &str, shim_excludes: &[String]) -> Option<&'static str> {
    if shim_excludes.iter().any(|s| s == id) {
        return Some("declared non-deterministic in determinism-shim.json");
    }
    // Data ingestion reads the original external inputs (a host path outside
    // the package); an offline hermetic replay cannot reach that source, so
    // re-running it always fails. Its staged inputs are byte-compared anyway.
    if id == "data_acquisition" {
        return Some("data-ingestion stage (external source not reproducible offline)");
    }
    if EXCLUDE_PREFIX.iter().any(|p| id.starts_with(p)) {
        return Some("discovery/validation/literature stage");
    }
    if id == "reporting" || id == "final_reporting" || id.ends_with("_reporting") {
        return Some("reporting stage");
    }
    None
}

/// Select deterministic compute tasks from a downloaded ECAA package.
///
/// A task is selected when ALL of the following hold:
/// - `runtime/outputs/<id>/scripts/` exists and contains ≥1 `.R`/`.py`/`.sh` file.
/// - ≥1 `.tsv` or `.csv` file exists directly under `runtime/outputs/<id>/`.
/// - The task id is not excluded (shim or prefix/suffix rule).
///
/// Tasks that have scripts but are excluded → `SkippedStage` with reason.
/// Tasks that have scripts but no result table → `SkippedStage` with reason
/// "no result table produced".
///
/// Results are returned in deterministic (lexicographic) order.
pub fn select_compute_tasks(
    pkg: &Path,
) -> std::io::Result<(Vec<ComputeTask>, Vec<SkippedStage>)> {
    // Read optional determinism shim.
    let shim_excludes: Vec<String> = std::fs::read_to_string(
        pkg.join("runtime/determinism-shim.json"),
    )
    .ok()
    .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
    .and_then(|v| v.get("non_deterministic_stages").cloned())
    .and_then(|v| serde_json::from_value(v).ok())
    .unwrap_or_default();

    let outputs = pkg.join("runtime/outputs");
    let mut sel: Vec<ComputeTask> = vec![];
    let mut skipped: Vec<SkippedStage> = vec![];

    if !outputs.is_dir() {
        return Ok((sel, skipped));
    }

    // Collect + sort for deterministic ordering.
    let mut dirs: Vec<_> = std::fs::read_dir(&outputs)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    dirs.sort_by_key(|e| e.file_name());

    for e in dirs {
        let id = e.file_name().to_string_lossy().to_string();
        let task_path = e.path();
        let scripts = task_path.join("scripts");

        // Must have ≥1 script; tasks without scripts are silently ignored.
        let has_script = scripts.is_dir()
            && std::fs::read_dir(&scripts)
                .map(|mut r| {
                    r.any(|f| {
                        f.as_ref()
                            .map(|f| {
                                let n = f.file_name();
                                let n = n.to_string_lossy();
                                n.ends_with(".R")
                                    || n.ends_with(".py")
                                    || n.ends_with(".sh")
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);

        if !has_script {
            continue;
        }

        if let Some(reason) = is_excluded(&id, &shim_excludes) {
            skipped.push(SkippedStage { task: id, reason: reason.into() });
            continue;
        }

        // Finding 3: differentiate an unreadable output dir from "no table".
        // We propagate the error via `?` so the caller knows something is wrong
        // with the filesystem rather than silently treating it as "no table".
        // This is safe: `read_dir` failing here means we cannot enumerate
        // outputs at all — surfacing the error is more conservative than
        // silently skipping a task that may actually be selectable.
        let mut tables: Vec<String> = std::fs::read_dir(&task_path)?
            .filter_map(|f| f.ok())
            .map(|f| f.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tsv") || n.ends_with(".csv"))
            .collect();

        // Finding 4: sort for reproducible downstream iteration.
        tables.sort();

        if tables.is_empty() {
            skipped.push(SkippedStage {
                task: id,
                reason: "no result table produced".into(),
            });
        } else {
            sel.push(ComputeTask {
                task_id: id,
                scripts_dir: scripts,
                result_tables: tables,
            });
        }
    }

    Ok((sel, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a task directory under `root/runtime/outputs/<id>`.
    /// If `script` is true, adds `scripts/01.R`. If `table` is Some, writes
    /// that filename directly under the task dir.
    fn mk(root: &Path, id: &str, script: bool, table: Option<&str>) {
        let d = root.join("runtime/outputs").join(id);
        if script {
            std::fs::create_dir_all(d.join("scripts")).unwrap();
            std::fs::write(d.join("scripts/01.R"), "1\n").unwrap();
        }
        std::fs::create_dir_all(&d).unwrap();
        if let Some(t) = table {
            std::fs::write(d.join(t), "a\tb\n").unwrap();
        }
    }

    /// `data_acquisition` is a data-INGESTION stage: its script reads the
    /// original external SME inputs (a host path outside the package) and
    /// stages them in. Offline replay cannot reproduce that — the source is
    /// absent and not mounted into the hermetic container — so it must be
    /// SKIPPED, not run (running it fails with FileNotFoundError and
    /// spuriously marks the package's re-execution FAILED). Its staged inputs
    /// (`data/…`) are still byte-compared by the comparator regardless.
    #[test]
    fn excludes_data_acquisition_ingestion_stage() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk(root, "data_acquisition", true, Some("cohort_manifest.tsv"));
        mk(root, "differential_expression", true, Some("de_results.tsv"));

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        assert_eq!(
            sel.iter().map(|t| t.task_id.as_str()).collect::<Vec<_>>(),
            ["differential_expression"],
            "data_acquisition must not be selected for re-execution"
        );
        let da = skipped
            .iter()
            .find(|s| s.task == "data_acquisition")
            .expect("data_acquisition must be skipped");
        assert!(
            da.reason.contains("ingestion"),
            "skip reason should identify it as a data-ingestion stage; got: '{}'",
            da.reason
        );
    }

    #[test]
    fn selects_compute_excludes_validate_literature_reporting() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk(root, "differential_expression", true, Some("de_results.tsv"));
        mk(root, "validate_differential_expression", true, Some("checks.tsv"));
        mk(root, "contextualize_findings_with_literature", true, Some("matrix.csv"));
        mk(root, "reporting", true, None);

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        assert_eq!(
            sel.iter().map(|t| t.task_id.as_str()).collect::<Vec<_>>(),
            ["differential_expression"]
        );
        let sk_ids: Vec<_> = skipped.iter().map(|s| s.task.as_str()).collect();
        assert!(sk_ids.contains(&"validate_differential_expression"));
        assert!(sk_ids.contains(&"contextualize_findings_with_literature"));
        assert!(sk_ids.contains(&"reporting"));

        // Finding 5: assert the skip reason for the reporting case.
        let reporting_reason = skipped
            .iter()
            .find(|s| s.task == "reporting")
            .map(|s| s.reason.as_str())
            .unwrap_or("");
        assert_eq!(reporting_reason, "reporting stage",
            "reporting task should carry the reporting-exclusion reason, not '{}'",
            reporting_reason);
    }

    // Finding 1: shim exclusion path must be exercised.
    // Creates a task that would otherwise be selected (script + de_results.tsv)
    // and declares it in a determinism-shim.json; asserts it is excluded with
    // a reason that mentions the shim file.
    #[test]
    fn shim_exclusion_takes_priority() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk(root, "differential_expression", true, Some("de_results.tsv"));

        // Write the shim declaring differential_expression non-deterministic.
        let shim_dir = root.join("runtime");
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::write(
            shim_dir.join("determinism-shim.json"),
            r#"{"non_deterministic_stages":["differential_expression"]}"#,
        )
        .unwrap();

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        assert!(sel.is_empty(), "shim-excluded task must not be selected");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].task, "differential_expression");
        assert!(
            skipped[0].reason.contains("determinism-shim"),
            "skip reason should mention the shim file, got: '{}'",
            skipped[0].reason
        );
    }

    // Finding 2: "no result table" skip path for a non-excluded task.
    // `reporting` in the main test hits the *exclusion* branch, so we need a
    // task with a non-excluded name (compute_something) that has a script but
    // no .tsv/.csv to reach the no-table branch.
    #[test]
    fn no_result_table_skips_non_excluded_task() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // script present, no table → should land in skipped with "no result table produced"
        mk(root, "compute_something", true, None);

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        assert!(sel.is_empty(), "task with no table must not be selected");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].task, "compute_something");
        assert_eq!(
            skipped[0].reason, "no result table produced",
            "expected 'no result table produced', got: '{}'",
            skipped[0].reason
        );
    }
}
