//! Killed-completion status reconciliation (CV-4).
//!
//! A stage's recorded status must reflect the FINAL dispatch attempt's
//! process outcome, not the agent's self-reported `result.json` status.
//! The wall-clock reaper (`executor/local.rs`) and the orphan reaper
//! (`dispatch_wal.rs`) already re-block a *live* dispatch, and the
//! silent-completion guard in `main.rs` already re-blocks a
//! `TaskState::Completed` task whose declared `required_artifacts` are
//! missing (via `required_artifacts::verify_required_artifacts`). The
//! gap this module closes: that guard is gated on `TaskState::Completed`
//! and never runs when a wall-clock-killed task self-reports
//! `status:"completed"` in `result.json` while the harness left its
//! graph state non-`Completed` (Running/Ready). This reconciles the
//! self-report against the on-disk process record (`error.json`) and the
//! declared artifacts, and routes the reconciled state through the
//! EXISTING `verify_required_artifacts` guard.
//!
//! The reconciliation is deliberately conservative: it does NOT force
//! "any kill in history ⇒ failed". A stage killed on an earlier attempt
//! then retried successfully (its completion timestamp is strictly newer
//! than the kill's) keeps its `completed` status.

use ecaa_workflow_core::dag::RequiredArtifact;
use std::path::Path;

use crate::required_artifacts::verify_required_artifacts;

/// The reconciled verdict for a task that claims completion (either its
/// DAG state is `Completed` or its `result.json` self-reports
/// `status:"completed"`).
///
/// Not `#[non_exhaustive]`: this is an internal harness control enum
/// (consumed only by the silent-completion guard in the harness binary),
/// never serialized and never crossing the ts-rs / RO-Crate / API
/// boundary — so the guard's `match` is kept exhaustive on purpose to
/// force a compile error if a future verdict variant is added.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompletionVerdict {
    /// The recorded `completed` status stands.
    Stands,
    /// The recorded status must be demoted from `completed`. Carries the
    /// typed re-block reason: a `[missing_artifact]` marker when declared
    /// artifacts are absent (the server promotes it to
    /// `BlockerKind::MissingArtifact`), else a `[killed_incomplete]`
    /// marker when the final dispatch attempt was a kill.
    Demote(String),
}

/// Reconcile a self-reported completion against the declared required
/// artifacts and the final dispatch attempt's process outcome.
///
/// Order of precedence for the demotion reason:
///   1. `[missing_artifact]` — a declared `required_artifacts` path is
///      missing/empty (or its declaration is invalid). This is the
///      existing guard's reason string, kept byte-identical so the
///      server's blocker mapper still recognises it.
///   2. `[killed_incomplete]` — no artifacts missing, but the final
///      dispatch attempt was a kill that no later successful completion
///      superseded.
///
/// Otherwise the completion `Stands`.
pub fn verdict_for(
    package_root: &Path,
    task_id: &str,
    required: &[RequiredArtifact],
) -> CompletionVerdict {
    match verify_required_artifacts(package_root, task_id, required) {
        Ok(missing) if !missing.is_empty() => {
            CompletionVerdict::Demote(missing_artifact_reason(task_id, &missing))
        }
        Ok(_) => {
            if completion_contradicted_by_kill(package_root, task_id) {
                CompletionVerdict::Demote(killed_incomplete_reason(task_id))
            } else {
                CompletionVerdict::Stands
            }
        }
        Err(e) => CompletionVerdict::Demote(format!(
            "[missing_artifact] task={} paths=<invalid> — required artifact declaration is invalid: {}",
            task_id, e
        )),
    }
}

/// The exact reason string the silent-completion guard emits for a
/// missing/empty required artifact (kept here so `verdict_for` and the
/// inline guard produce byte-identical reasons).
fn missing_artifact_reason(task_id: &str, missing: &[String]) -> String {
    format!(
        "[missing_artifact] task={} paths={} — agent marked completed but required artifacts are missing or empty.",
        task_id,
        missing.join(","),
    )
}

fn killed_incomplete_reason(task_id: &str) -> String {
    format!(
        "[killed_incomplete] task={} — agent marked completed but the final dispatch attempt was killed (see runtime/outputs/{}/error.json); recorded status demoted from completed.",
        task_id, task_id
    )
}

/// True when a task's self-reported completion is contradicted by the
/// FINAL dispatch attempt being a kill.
///
/// Reads the harness-written tool-error envelope
/// (`runtime/outputs/<task_id>/error.json`) and compares its capture
/// time against the completion time recorded by the agent
/// (`result.json.completed_at`, falling back to
/// `agent-code.json.completed_at`).
///
/// Returns `false` (completion stands) when:
///   * no `error.json` exists (no kill on record), or
///   * the envelope records a clean exit (`exit_code == 0`), or
///   * a completion timestamp is present and strictly newer than the
///     kill timestamp (killed on an earlier attempt, retried
///     successfully), or
///   * the timestamps cannot be established — insufficient evidence to
///     override a self-report, so the artifact guard remains the
///     backstop rather than risk a false demotion.
///
/// Returns `true` (contradicted) only when a kill is on record AND a
/// completion timestamp is present that did NOT supersede it.
pub fn completion_contradicted_by_kill(package_root: &Path, task_id: &str) -> bool {
    let out = package_root.join("runtime/outputs").join(task_id);
    let raw = match std::fs::read_to_string(out.join("error.json")) {
        Ok(r) => r,
        Err(_) => return false, // no kill on record
    };
    let env: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return false,
    };
    // A clean exit recorded in error.json is not a kill.
    if env.get("exit_code").and_then(serde_json::Value::as_i64) == Some(0) {
        return false;
    }
    let kill_ts = env
        .get("captured_at")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_ts);
    let completion_ts = completion_timestamp(&out);
    match (kill_ts, completion_ts) {
        // Kill at/after the recorded completion → the completion did not
        // supersede it → contradicted.
        (Some(kill), Some(done)) => done <= kill,
        // Insufficient timestamp evidence → do not demote on the kill
        // alone; the required-artifact guard is the backstop.
        _ => false,
    }
}

/// True when `result.json` in the given task output dir self-reports
/// `status:"completed"`. Used to extend the reconciliation past the
/// DAG-`Completed`-only trigger.
pub fn result_json_reports_completed(package_root: &Path, task_id: &str) -> bool {
    let p = package_root
        .join("runtime/outputs")
        .join(task_id)
        .join("result.json");
    let raw = match std::fs::read_to_string(&p) {
        Ok(r) => r,
        Err(_) => return false,
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| {
            v.get("status")
                .and_then(serde_json::Value::as_str)
                .map(|s| s.eq_ignore_ascii_case("completed"))
        })
        .unwrap_or(false)
}

/// The best available completion timestamp for a task: `result.json`'s
/// `completed_at`, falling back to `agent-code.json`'s `completed_at`.
fn completion_timestamp(task_out: &Path) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    let from = |file: &str| -> Option<chrono::DateTime<chrono::FixedOffset>> {
        let raw = std::fs::read_to_string(task_out.join(file)).ok()?;
        let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
        v.get("completed_at")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_ts)
    };
    from("result.json").or_else(|| from("agent-code.json"))
}

/// Parse an RFC 3339 timestamp. Tolerates both `...Z` and
/// `...+00:00` / fractional-second forms.
fn parse_ts(s: &str) -> Option<chrono::DateTime<chrono::FixedOffset>> {
    chrono::DateTime::parse_from_rfc3339(s.trim()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::dag::RequiredArtifact;

    fn required(path: &str) -> RequiredArtifact {
        RequiredArtifact {
            path: path.to_string(),
            schema_ref: None,
            min_size_bytes: None,
            validation_obligations: vec![],
        }
    }

    fn write(p: &Path, body: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn no_error_json_means_completion_stands() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/de");
        write(&out.join("result.json"), r#"{"status":"completed","completed_at":"2026-07-18T18:00:00Z"}"#);
        assert!(!completion_contradicted_by_kill(pkg, "de"));
        assert_eq!(verdict_for(pkg, "de", &[]), CompletionVerdict::Stands);
    }

    #[test]
    fn kill_after_completion_is_contradicted() {
        // Agent self-reported completed, then the final attempt was
        // wall-clock killed (kill newer than the completion) — no retry.
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/review_prior_work");
        write(&out.join("result.json"), r#"{"status":"completed","completed_at":"2026-07-18T17:59:00Z"}"#);
        write(
            &out.join("error.json"),
            r#"{"error_class":"WallclockExceeded","exit_code":137,"captured_at":"2026-07-18T18:00:00.5+00:00","attempt":1}"#,
        );
        assert!(completion_contradicted_by_kill(pkg, "review_prior_work"));
    }

    #[test]
    fn kill_then_successful_retry_stays_completed() {
        // Killed at attempt 1 (error.json), retried successfully — the
        // completion timestamp is strictly newer than the kill. Must NOT
        // be demoted, and with the artifact present the verdict Stands.
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/survey_method_landscape");
        write(&out.join("result.json"), r#"{"status":"completed","completed_at":"2026-07-18T18:01:37Z"}"#);
        write(
            &out.join("error.json"),
            r#"{"error_class":"WallclockExceeded","exit_code":137,"captured_at":"2026-07-18T17:52:53.190526951+00:00","attempt":1}"#,
        );
        write(&out.join("landscape.csv"), "col\n1\n");
        assert!(!completion_contradicted_by_kill(pkg, "survey_method_landscape"));
        assert_eq!(
            verdict_for(pkg, "survey_method_landscape", &[required("landscape.csv")]),
            CompletionVerdict::Stands
        );
    }

    #[test]
    fn killed_completion_with_missing_artifact_demotes_via_missing_artifact() {
        // The flagship case: killed (exit 137), result.json completed,
        // and the declared required artifact was never written.
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/review_prior_work");
        write(&out.join("result.json"), r#"{"status":"completed","completed_at":"2026-07-18T17:59:00Z"}"#);
        write(
            &out.join("error.json"),
            r#"{"error_class":"WallclockExceeded","exit_code":137,"captured_at":"2026-07-18T18:00:00Z","attempt":1}"#,
        );
        let verdict = verdict_for(pkg, "review_prior_work", &[required("prior_claims_matrix.csv")]);
        match verdict {
            CompletionVerdict::Demote(reason) => {
                assert!(reason.contains("[missing_artifact]"), "reason: {reason}");
                assert!(reason.contains("prior_claims_matrix.csv"), "reason: {reason}");
            }
            other => panic!("expected Demote([missing_artifact]), got {other:?}"),
        }
    }

    #[test]
    fn killed_completion_with_artifact_present_demotes_via_killed_incomplete() {
        // Killed final attempt but the declared artifact happens to
        // exist → the status is still demoted (the process outcome was a
        // kill), with the killed_incomplete reason.
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/review_prior_work");
        write(&out.join("result.json"), r#"{"status":"completed","completed_at":"2026-07-18T17:59:00Z"}"#);
        write(
            &out.join("error.json"),
            r#"{"error_class":"WallclockExceeded","exit_code":137,"captured_at":"2026-07-18T18:00:00Z","attempt":1}"#,
        );
        write(&out.join("prior_claims_matrix.csv"), "pmid,quote\n1,x\n");
        let verdict = verdict_for(pkg, "review_prior_work", &[required("prior_claims_matrix.csv")]);
        match verdict {
            CompletionVerdict::Demote(reason) => {
                assert!(reason.contains("[killed_incomplete]"), "reason: {reason}");
            }
            other => panic!("expected Demote([killed_incomplete]), got {other:?}"),
        }
    }

    #[test]
    fn clean_exit_recorded_in_error_json_is_not_a_kill() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/de");
        write(&out.join("result.json"), r#"{"status":"completed","completed_at":"2026-07-18T17:00:00Z"}"#);
        write(
            &out.join("error.json"),
            r#"{"error_class":"None","exit_code":0,"captured_at":"2026-07-18T18:00:00Z","attempt":1}"#,
        );
        assert!(!completion_contradicted_by_kill(pkg, "de"));
    }

    #[test]
    fn result_json_completed_detection() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let out = pkg.join("runtime/outputs/de");
        write(&out.join("result.json"), r#"{"status":"completed"}"#);
        assert!(result_json_reports_completed(pkg, "de"));
        let out2 = pkg.join("runtime/outputs/other");
        write(&out2.join("result.json"), r#"{"status":"blocked"}"#);
        assert!(!result_json_reports_completed(pkg, "other"));
        assert!(!result_json_reports_completed(pkg, "absent"));
    }
}
