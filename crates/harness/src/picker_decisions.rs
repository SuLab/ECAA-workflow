// Picker-decision audit trail.
//
// Each harness iteration that examined at least one Ready task and refused one
// or more of them appends one JSON-lines record per examined task to
// `<package_root>/runtime/picker-decisions.jsonl`.  The file is append-only
// so it accumulates across restarts.  Operators use it to answer "why isn't
// the harness picking up tasks?" without bumping log levels or grepping
// tracing output.
//
// Schema (each line is a self-contained JSON object):
//   {
//     "ts":        "<RFC-3339 timestamp>",
//     "iteration": <0-based loop counter>,
//     "task_id":   "<stage id>",
//     "decision":  "accepted | slot_exhausted | sme_review_required |
//                   sandbox_refused | network_refused | safety_refused",
//     "reason":    "<short detail; empty string for accepted>"
//   }
//
// Write discipline: the caller collects records for one iteration, then calls
// `append_picker_decisions` only when at least one record has
// `decision != "accepted"`.  If every examined task was accepted the function
// is not called and no bytes are written — zero log noise for the happy path.
//
// Failure discipline: file-write errors are logged via `tracing::warn!` and
// swallowed.  The picker must never fail because of this file.

use std::io::Write as _;
use std::path::Path;

/// One record per Ready task examined by the picker in a single iteration.
#[derive(Debug, serde::Serialize)]
pub(crate) struct PickerDecisionRecord {
    /// RFC-3339 UTC timestamp of the iteration.
    pub ts: String,
    /// 0-based harness loop counter.
    pub iteration: usize,
    /// Task stage id.
    pub task_id: String,
    /// Outcome of the picker evaluation for this task.
    pub decision: &'static str,
    /// Short detail explaining a refusal.  Empty for `accepted`.
    pub reason: String,
}

/// Append `records` to `<package_root>/runtime/picker-decisions.jsonl`.
///
/// Best-effort: creates the `runtime/` directory if absent, opens (or creates)
/// the file in append mode, and writes one JSON line per record followed by a
/// newline.  On any error emits `tracing::warn!` and returns — the caller must
/// not propagate the error.
pub(crate) fn append_picker_decisions(package_root: &Path, records: &[PickerDecisionRecord]) {
    if records.is_empty() {
        return;
    }
    let path = package_root.join("runtime").join("picker-decisions.jsonl");
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            tracing::warn!(
                target: "picker-decisions",
                path = %path.display(),
                error = %e,
                "could not create runtime/ dir for picker-decisions.jsonl"
            );
            return;
        }
    }
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(
                target: "picker-decisions",
                path = %path.display(),
                error = %e,
                "could not open picker-decisions.jsonl for append"
            );
            return;
        }
    };
    for rec in records {
        match serde_json::to_string(rec) {
            Ok(line) => {
                if let Err(e) = file.write_all(line.as_bytes()) {
                    tracing::warn!(
                        target: "picker-decisions",
                        task_id = %rec.task_id,
                        error = %e,
                        "write error on picker-decisions.jsonl"
                    );
                    return;
                }
                if let Err(e) = file.write_all(b"\n") {
                    tracing::warn!(
                        target: "picker-decisions",
                        task_id = %rec.task_id,
                        error = %e,
                        "write error (newline) on picker-decisions.jsonl"
                    );
                    return;
                }
            }
            Err(e) => {
                tracing::warn!(
                    target: "picker-decisions",
                    task_id = %rec.task_id,
                    error = %e,
                    "serde_json serialization error for picker-decisions record"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufRead as _;

    #[test]
    fn append_writes_jsonl_and_creates_runtime_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        // runtime/ does not exist yet — append must create it.
        let records = vec![
            PickerDecisionRecord {
                ts: "2026-01-01T00:00:00Z".into(),
                iteration: 0,
                task_id: "align_reads".into(),
                decision: "sandbox_refused",
                reason: "UnpinnedContainer (node=align_reads)".into(),
            },
            PickerDecisionRecord {
                ts: "2026-01-01T00:00:00Z".into(),
                iteration: 0,
                task_id: "qc_reads".into(),
                decision: "accepted",
                reason: String::new(),
            },
        ];
        append_picker_decisions(pkg, &records);
        let path = pkg.join("runtime/picker-decisions.jsonl");
        assert!(path.exists(), "picker-decisions.jsonl must be created");
        let file = std::fs::File::open(&path).unwrap();
        let lines: Vec<String> = std::io::BufReader::new(file)
            .lines()
            .map(|l| l.unwrap())
            .collect();
        assert_eq!(lines.len(), 2, "two records, two lines");
        let obj0: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(obj0["decision"], "sandbox_refused");
        assert_eq!(obj0["task_id"], "align_reads");
        assert_eq!(obj0["iteration"], 0);
        let obj1: serde_json::Value = serde_json::from_str(&lines[1]).unwrap();
        assert_eq!(obj1["decision"], "accepted");
    }

    #[test]
    fn append_is_noop_on_empty_records() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        append_picker_decisions(pkg, &[]);
        assert!(
            !pkg.join("runtime/picker-decisions.jsonl").exists(),
            "no file created for empty record set"
        );
    }

    #[test]
    fn append_accumulates_across_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime")).unwrap();
        let rec = |iteration: usize, task_id: &str, decision: &'static str| PickerDecisionRecord {
            ts: "2026-01-01T00:00:00Z".into(),
            iteration,
            task_id: task_id.into(),
            decision,
            reason: String::new(),
        };
        append_picker_decisions(pkg, &[rec(0, "a", "slot_exhausted")]);
        append_picker_decisions(pkg, &[rec(1, "a", "slot_exhausted")]);
        let path = pkg.join("runtime/picker-decisions.jsonl");
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), 2);
    }
}
