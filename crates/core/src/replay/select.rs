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
/// 2. Task id starts with a known non-compute prefix.
/// 3. Task id is `reporting`, `final_reporting`, or ends with `_reporting`.
fn is_excluded(id: &str, shim_excludes: &[String]) -> Option<&'static str> {
    if shim_excludes.iter().any(|s| s == id) {
        return Some("declared non-deterministic in determinism-shim.json");
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

        let tables: Vec<String> = std::fs::read_dir(&task_path)
            .map(|r| {
                r.filter_map(|f| f.ok())
                    .map(|f| f.file_name().to_string_lossy().to_string())
                    .filter(|n| n.ends_with(".tsv") || n.ends_with(".csv"))
                    .collect()
            })
            .unwrap_or_default();

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

    #[test]
    fn selects_compute_excludes_validate_literature_reporting() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mk = |id: &str, script: bool, table: Option<&str>| {
            let d = root.join("runtime/outputs").join(id);
            if script {
                std::fs::create_dir_all(d.join("scripts")).unwrap();
                std::fs::write(d.join("scripts/01.R"), "1\n").unwrap();
            }
            std::fs::create_dir_all(&d).unwrap();
            if let Some(t) = table {
                std::fs::write(d.join(t), "a\tb\n").unwrap();
            }
        };
        mk("differential_expression", true, Some("de_results.tsv"));
        mk("validate_differential_expression", true, Some("checks.tsv"));
        mk("contextualize_findings_with_literature", true, Some("matrix.csv"));
        mk("reporting", true, None);

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        assert_eq!(
            sel.iter().map(|t| t.task_id.as_str()).collect::<Vec<_>>(),
            ["differential_expression"]
        );
        let sk: Vec<_> = skipped.iter().map(|s| s.task.as_str()).collect();
        assert!(sk.contains(&"validate_differential_expression"));
        assert!(sk.contains(&"contextualize_findings_with_literature"));
        assert!(sk.contains(&"reporting"));
    }
}
