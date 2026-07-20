//! Server-side required-artifact guard (CV-4).
//!
//! The harness `run_loop` re-blocks a `TaskState::Completed` task whose
//! declared `required_artifacts` are missing or empty (via
//! `harness::required_artifacts::verify_required_artifacts`). But the
//! server's own task-completion write paths — the authoritative
//! `POST /task/:task_id/state` finalize route and the `task_completed`
//! progress event — set `Completed` WITHOUT re-running that guard, so a
//! killed stage that self-reports `status:"completed"` while never
//! writing its declared artifacts could still escape into the deposit via
//! the server path.
//!
//! This module closes that gap on the server side: before a `Completed`
//! write is accepted, the task's declared `required_artifacts` are
//! verified to exist and be non-empty on disk. When any are missing the
//! completion is refused and demoted to a `[missing_artifact]` blocker —
//! the byte-identical marker the harness silent-completion guard emits,
//! which `core::blocker` upgrades to `BlockerKind::MissingArtifact`.
//!
//! Implemented server-locally (keyed off the same `Task.required_artifacts`)
//! rather than depending on the harness *binary* crate: the server does
//! not link the harness, and adding that dependency would couple the two
//! and churn a crate a parallel unit is editing. The declared-artifact
//! existence check is small and fully covered by the unit tests below.

use ecaa_workflow_conversation::session::Session;
use ecaa_workflow_core::blocker::BlockerKind;
use ecaa_workflow_core::dag::{BlockedRecord, RequiredArtifact, TaskState};
use std::path::Path;

use super::{runtime_outputs_for_task, safe_relative_join};

/// Return the declared `required_artifacts` for `task_id` that are
/// missing/empty on disk under `<package>/runtime/outputs/<task_id>/`.
///
/// An empty vec means the completion may proceed. That is also the result
/// when there is nothing to verify against — no emitted package, the DAG
/// cannot be derived, the task is unknown, or the task declares no
/// required artifacts — so the guard never blocks a legitimately
/// artifact-free task.
pub(crate) fn missing_declared_artifacts(session: &Session, task_id: &str) -> Vec<String> {
    let Some(pkg) = session.emitted_package_path.as_ref() else {
        return Vec::new();
    };
    let Some(dag) = session.current_dag() else {
        return Vec::new();
    };
    let Some(task) = dag.tasks.get(task_id) else {
        return Vec::new();
    };
    if task.required_artifacts.is_empty() {
        return Vec::new();
    }
    missing_under_root(pkg, task_id, &task.required_artifacts)
}

/// Pure disk check: which of `required` are missing, not a regular file,
/// empty, or smaller than their declared `min_size_bytes`, under
/// `<pkg>/runtime/outputs/<task_id>/`. Split out from
/// [`missing_declared_artifacts`] so the unit tests can exercise the
/// filesystem logic without constructing a `Session`.
///
/// Artifact paths are path-jailed (via `runtime_outputs_for_task` +
/// `safe_relative_join`): if the task output dir cannot be resolved
/// (package root missing / traversal) every declared artifact counts as
/// missing, and an individual artifact path that escapes the jail counts
/// as missing — a completion is never accepted on an unverifiable path.
pub(crate) fn missing_under_root(
    pkg: &Path,
    task_id: &str,
    required: &[RequiredArtifact],
) -> Vec<String> {
    let base = match runtime_outputs_for_task(pkg, task_id) {
        Ok(b) => b,
        // Package root doesn't exist / task_id can't be jailed: nothing
        // is verifiable, so treat every declared artifact as missing.
        Err(_) => return required.iter().map(|a| a.path.clone()).collect(),
    };
    let mut missing = Vec::new();
    // Canonical jail root for symlink-escape detection. Absent (the task
    // output dir doesn't exist yet) => every declared artifact fails the
    // `metadata` probe below and is already counted missing.
    let base_canon = base.canonicalize().ok();
    for entry in required {
        let full = match safe_relative_join(&base, Path::new(&entry.path)) {
            Ok(f) => f,
            // Absolute or `..`-bearing declaration: never satisfiable.
            Err(_) => {
                missing.push(entry.path.clone());
                continue;
            }
        };
        match std::fs::metadata(&full) {
            Err(_) => missing.push(entry.path.clone()),
            Ok(meta) => {
                if !meta.is_file() {
                    missing.push(entry.path.clone());
                    continue;
                }
                // Reject symlink escape: `std::fs::metadata` (and
                // `safe_relative_join`, which only rejects lexical
                // `..`/absolute in the DECLARED path) would otherwise let a
                // declared artifact that is a symlink to a valid non-empty
                // file OUTSIDE the task output jail (e.g. /etc/hosts)
                // satisfy the guard. Mirror the harness guard: the
                // resolved artifact's canonical path must stay under the
                // canonical task-output root. Any canonicalize failure or
                // escape => treat as missing (fail-closed).
                match (base_canon.as_ref(), full.canonicalize().ok()) {
                    (Some(root), Some(full_canon)) if full_canon.starts_with(root) => {}
                    _ => {
                        missing.push(entry.path.clone());
                        continue;
                    }
                }
                let min = entry.min_size_bytes.unwrap_or(0);
                if meta.len() == 0 || meta.len() < min {
                    missing.push(entry.path.clone());
                }
            }
        }
    }
    missing
}

/// The demotion reason string for a missing/empty required artifact.
/// Kept byte-identical to the harness silent-completion guard's
/// `missing_artifact_reason` so `core::blocker`'s `[missing_artifact]`
/// mapper upgrades it to `BlockerKind::MissingArtifact` on both paths.
pub(crate) fn missing_artifact_reason(task_id: &str, missing: &[String]) -> String {
    format!(
        "[missing_artifact] task={} paths={} — agent marked completed but required artifacts are missing or empty.",
        task_id,
        missing.join(","),
    )
}

/// Build the `Blocked` task state a refused completion is demoted to.
pub(crate) fn demoted_blocked_state(task_id: &str, missing: &[String]) -> TaskState {
    TaskState::Blocked {
        record: BlockedRecord {
            reason: missing_artifact_reason(task_id, missing),
            attempts: vec![],
        },
    }
}

/// The typed blocker kind matching [`demoted_blocked_state`], used when a
/// demoted completion also drives the session state machine to `Blocked`.
pub(crate) fn missing_artifact_blocker(task_id: &str, missing: &[String]) -> BlockerKind {
    BlockerKind::MissingArtifact {
        task_id: task_id.to_string(),
        missing_paths: missing.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn req(path: &str, min: Option<u64>) -> RequiredArtifact {
        RequiredArtifact {
            path: path.to_string(),
            min_size_bytes: min,
            schema_ref: None,
            validation_obligations: Vec::new(),
        }
    }

    fn write(pkg: &Path, task_id: &str, rel: &str, bytes: &[u8]) {
        let full = pkg.join("runtime/outputs").join(task_id).join(rel);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, bytes).unwrap();
    }

    #[test]
    fn present_nonempty_artifact_is_not_missing() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path();
        write(pkg, "t1", "results/de.tsv", b"gene\tlog2fc\n");
        let missing = missing_under_root(pkg, "t1", &[req("results/de.tsv", None)]);
        assert!(missing.is_empty(), "present non-empty artifact: {missing:?}");
    }

    #[test]
    fn absent_artifact_is_missing() {
        let tmp = TempDir::new().unwrap();
        // task output dir exists but the declared file does not.
        std::fs::create_dir_all(tmp.path().join("runtime/outputs/t1")).unwrap();
        let missing = missing_under_root(tmp.path(), "t1", &[req("results/de.tsv", None)]);
        assert_eq!(missing, vec!["results/de.tsv".to_string()]);
    }

    #[test]
    fn empty_artifact_is_missing() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "t1", "results/de.tsv", b"");
        let missing = missing_under_root(tmp.path(), "t1", &[req("results/de.tsv", None)]);
        assert_eq!(missing, vec!["results/de.tsv".to_string()]);
    }

    #[test]
    fn below_min_size_is_missing() {
        let tmp = TempDir::new().unwrap();
        write(tmp.path(), "t1", "results/de.tsv", b"tiny");
        let missing = missing_under_root(tmp.path(), "t1", &[req("results/de.tsv", Some(1024))]);
        assert_eq!(missing, vec!["results/de.tsv".to_string()]);
    }

    #[test]
    fn directory_is_not_a_valid_artifact() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("runtime/outputs/t1/results/de.tsv")).unwrap();
        let missing = missing_under_root(tmp.path(), "t1", &[req("results/de.tsv", None)]);
        assert_eq!(missing, vec!["results/de.tsv".to_string()]);
    }

    #[test]
    fn missing_task_dir_makes_every_artifact_missing() {
        let tmp = TempDir::new().unwrap();
        // pkg exists, but no runtime/outputs/<task_id> for this task.
        let missing = missing_under_root(
            tmp.path(),
            "never_ran",
            &[req("a.txt", None), req("b.txt", None)],
        );
        assert_eq!(missing, vec!["a.txt".to_string(), "b.txt".to_string()]);
    }

    #[test]
    fn parent_traversal_declaration_is_missing() {
        let tmp = TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("runtime/outputs/t1")).unwrap();
        // A `..`-bearing declared path is never satisfiable → missing.
        let missing = missing_under_root(tmp.path(), "t1", &[req("../escape.txt", None)]);
        assert_eq!(missing, vec!["../escape.txt".to_string()]);
    }

    /// A declared required_artifact that is a symlink whose target is a
    /// valid non-empty file OUTSIDE the task output jail must be treated as
    /// missing — otherwise an agent could satisfy a phantom artifact by
    /// symlinking to, e.g., /etc/hosts. Mirrors the harness canonicalize +
    /// range-check guard.
    #[cfg(unix)]
    #[test]
    fn symlink_escaping_artifact_is_missing() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path().join("pkg");
        let task_dir = pkg.join("runtime/outputs/t1/results");
        std::fs::create_dir_all(&task_dir).unwrap();
        // A valid, non-empty file OUTSIDE the task output jail.
        let outside = tmp.path().join("outside_secret.txt");
        std::fs::write(&outside, b"127.0.0.1 localhost\n").unwrap();
        // The declared artifact resolves through a symlink to that file.
        std::os::unix::fs::symlink(&outside, task_dir.join("de.tsv")).unwrap();
        let missing = missing_under_root(&pkg, "t1", &[req("results/de.tsv", None)]);
        assert_eq!(
            missing,
            vec!["results/de.tsv".to_string()],
            "a symlink escaping the task output jail must be treated as missing"
        );
    }

    /// A symlink whose target is a valid non-empty file INSIDE the jail is
    /// still a legitimate artifact — canonicalization must not over-reject.
    #[cfg(unix)]
    #[test]
    fn symlink_within_jail_is_accepted() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path().join("pkg");
        let results = pkg.join("runtime/outputs/t1/results");
        std::fs::create_dir_all(&results).unwrap();
        let real = results.join("de.real.tsv");
        std::fs::write(&real, b"gene\tlog2fc\n").unwrap();
        std::os::unix::fs::symlink(&real, results.join("de.tsv")).unwrap();
        let missing = missing_under_root(&pkg, "t1", &[req("results/de.tsv", None)]);
        assert!(
            missing.is_empty(),
            "an in-jail symlink to a real non-empty file is a valid artifact: {missing:?}"
        );
    }

    #[test]
    fn reason_string_matches_harness_marker_shape() {
        let r = missing_artifact_reason("de", &["results/de.tsv".into(), "reports/de.md".into()]);
        assert_eq!(
            r,
            "[missing_artifact] task=de paths=results/de.tsv,reports/de.md — agent marked completed but required artifacts are missing or empty."
        );
    }
}
