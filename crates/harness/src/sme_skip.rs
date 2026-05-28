//! SME-acknowledged skip detection.
//!
//! When the SME picks "skip with deviation" (or similar) on a BlockerCard,
//! the agent should be permitted to emit an empty or single-row sentinel
//! artifact without the silent-completion guard re-blocking the task.
//!
//! The authoritative signal is `runtime/outputs/<task_id>/sme-decisions.json`
//! (the structured-decision re-entry file written by
//! `POST /api/chat/session/:id/task/:task_id/sme-selection`). This module
//! reads that file and classifies the chosen options into a `SmeSkipIntent`.
//!
//! Detection is deliberately permissive across the closed set of skip
//! option ids — the BlockerCard's option vocabulary is allowed to grow,
//! and any chosen option whose id matches one of the canonical skip
//! prefixes counts as an SME-acknowledged skip.

use std::path::Path;

/// Canonical skip-option id prefixes / exact matches.
///
/// The BlockerCard surfaces these as user-facing options on stages that
/// cannot make progress without further data. When the SME chooses one,
/// the harness should accept a sentinel-form completion rather than
/// looping on the empty-result guard.
const SKIP_OPTION_IDS: &[&str] = &[
    "emit_skip_sentinel_row",
    "mark_task_failed_documented_deviation",
    "drop_stage_from_workflow",
    "skip_with_deviation",
    "skip_with_documented_deviation",
];

/// Classification of the SME's chosen action on the blocker for this task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmeSkipIntent {
    /// No skip selection found (or no `sme-decisions.json` present). The
    /// guards should run their normal, strict path.
    None,
    /// SME explicitly authorized a sentinel completion. The guards should
    /// accept a 0- or 1-row sentinel artifact without re-blocking.
    EmitSentinel,
    /// SME explicitly marked the task as a documented deviation / failure.
    /// The task should not be re-blocked by guards.
    MarkFailedDeviation,
    /// SME requested dropping the stage entirely. The harness's guard
    /// must not re-block; the upstream amendment path handles removal.
    DropStage,
}

impl SmeSkipIntent {
    /// True when the SME's selection authorizes a sentinel / empty result.
    pub fn is_skip(&self) -> bool {
        !matches!(self, SmeSkipIntent::None)
    }
}

/// Read `runtime/outputs/<task_id>/sme-decisions.json` and classify the
/// chosen options. Returns `SmeSkipIntent::None` when the file is absent,
/// unreadable, or contains no skip-marked option ids.
///
/// The file shape (written by `crates/server/src/chat_routes/tasks/blocker.rs`):
/// ```json
/// {
///   "task_id": "...",
///   "timestamp": "...",
///   "decisions": [ { "id": "...", "chosen": "<option_id>" }, ... ],
///   "rationale": "..."
/// }
/// ```
pub fn detect_intent(package_root: &Path, task_id: &str) -> SmeSkipIntent {
    let p = package_root
        .join("runtime/outputs")
        .join(task_id)
        .join("sme-decisions.json");
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(_) => return SmeSkipIntent::None,
    };
    let v: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return SmeSkipIntent::None,
    };
    let Some(decisions) = v.get("decisions").and_then(|d| d.as_array()) else {
        return SmeSkipIntent::None;
    };
    for d in decisions {
        let Some(chosen) = d.get("chosen").and_then(|c| c.as_str()) else {
            continue;
        };
        if !SKIP_OPTION_IDS.contains(&chosen) {
            continue;
        }
        return match chosen {
            "drop_stage_from_workflow" => SmeSkipIntent::DropStage,
            "mark_task_failed_documented_deviation" => SmeSkipIntent::MarkFailedDeviation,
            _ => SmeSkipIntent::EmitSentinel,
        };
    }
    SmeSkipIntent::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_sme(dir: &Path, task_id: &str, body: &str) {
        let p = dir
            .join("runtime/outputs")
            .join(task_id)
            .join("sme-decisions.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }

    #[test]
    fn no_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(
            detect_intent(tmp.path(), "review_prior_work"),
            SmeSkipIntent::None
        );
    }

    #[test]
    fn malformed_file_returns_none() {
        let tmp = TempDir::new().unwrap();
        write_sme(tmp.path(), "review_prior_work", "not json");
        assert_eq!(
            detect_intent(tmp.path(), "review_prior_work"),
            SmeSkipIntent::None
        );
    }

    #[test]
    fn no_decisions_returns_none() {
        let tmp = TempDir::new().unwrap();
        write_sme(tmp.path(), "review_prior_work", r#"{"decisions":[]}"#);
        assert_eq!(
            detect_intent(tmp.path(), "review_prior_work"),
            SmeSkipIntent::None
        );
    }

    #[test]
    fn non_skip_choice_returns_none() {
        let tmp = TempDir::new().unwrap();
        write_sme(
            tmp.path(),
            "review_prior_work",
            r#"{"decisions":[{"id":"q1","chosen":"supply_local_corpus"}]}"#,
        );
        assert_eq!(
            detect_intent(tmp.path(), "review_prior_work"),
            SmeSkipIntent::None
        );
    }

    #[test]
    fn emit_sentinel_choice_detected() {
        let tmp = TempDir::new().unwrap();
        write_sme(
            tmp.path(),
            "review_prior_work",
            r#"{"decisions":[{"id":"q1","chosen":"emit_skip_sentinel_row"}]}"#,
        );
        assert_eq!(
            detect_intent(tmp.path(), "review_prior_work"),
            SmeSkipIntent::EmitSentinel
        );
    }

    #[test]
    fn mark_failed_deviation_detected() {
        let tmp = TempDir::new().unwrap();
        write_sme(
            tmp.path(),
            "review_prior_work",
            r#"{"decisions":[{"id":"q1","chosen":"mark_task_failed_documented_deviation"}]}"#,
        );
        assert_eq!(
            detect_intent(tmp.path(), "review_prior_work"),
            SmeSkipIntent::MarkFailedDeviation
        );
    }

    #[test]
    fn drop_stage_detected() {
        let tmp = TempDir::new().unwrap();
        write_sme(
            tmp.path(),
            "review_prior_work",
            r#"{"decisions":[{"id":"q1","chosen":"drop_stage_from_workflow"}]}"#,
        );
        assert_eq!(
            detect_intent(tmp.path(), "review_prior_work"),
            SmeSkipIntent::DropStage
        );
    }

    #[test]
    fn skip_with_deviation_alias_detected() {
        let tmp = TempDir::new().unwrap();
        write_sme(
            tmp.path(),
            "review_prior_work",
            r#"{"decisions":[{"id":"q1","chosen":"skip_with_deviation"}]}"#,
        );
        assert_eq!(
            detect_intent(tmp.path(), "review_prior_work"),
            SmeSkipIntent::EmitSentinel
        );
    }

    #[test]
    fn first_skip_wins_when_multiple_decisions() {
        let tmp = TempDir::new().unwrap();
        write_sme(
            tmp.path(),
            "review_prior_work",
            r#"{"decisions":[{"id":"q1","chosen":"supply_local_corpus"},{"id":"q2","chosen":"emit_skip_sentinel_row"}]}"#,
        );
        assert_eq!(
            detect_intent(tmp.path(), "review_prior_work"),
            SmeSkipIntent::EmitSentinel
        );
    }

    #[test]
    fn is_skip_helper() {
        assert!(!SmeSkipIntent::None.is_skip());
        assert!(SmeSkipIntent::EmitSentinel.is_skip());
        assert!(SmeSkipIntent::MarkFailedDeviation.is_skip());
        assert!(SmeSkipIntent::DropStage.is_skip());
    }
}
