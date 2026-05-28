//! session / task result readers.
//!
//! `get_session_state` returns a DAG progress summary + session state;
//! `get_task_result` returns the structured result for one task (or a
//! `PreconditionFailure` when the task hasn't finished yet).

use crate::errors::{ToolError, ToolResult};
use crate::session::Session;
use scripps_workflow_core::dag::TaskState;
use serde_json::Value;

/// §3.4 — soft cap on the size of any single text field surfaced back
/// to the LLM. Longer values are truncated to `NARRATIVE_EXCERPT_MAX`
/// plus a single-character ellipsis. The full payload remains
/// available via `GET /api/chat/session/:id/task/:task_id/result`.
const NARRATIVE_EXCERPT_MAX: usize = 300;

/// §3.4 — maximum number of top-level keys echoed into the summary
/// envelope. A few headline numbers are what the LLM needs to answer
/// the SME's question; long narratives are what replays on every
/// subsequent user turn if we don't trim here.
const SUMMARY_MAX_METRIC_KEYS: usize = 10;

pub(super) fn get_session_state(session: &Session) -> ToolResult {
    let dag_summary = session.current_dag().map(|d| {
        let (completed, ready, blocked, pending) = d.progress();
        let unresolved_discovery: Vec<&str> = d
            .tasks
            .iter()
            // Typed role via `derive_role_from_id`.
            .filter(|(id, t)| {
                scripps_workflow_core::taxonomy::derive_role_from_id(id.as_str()).is_discovery()
                    && matches!(t.state, TaskState::Pending | TaskState::Ready)
            })
            .map(|(id, _)| id.as_str())
            .collect();
        serde_json::json!({
            "task_count": d.tasks.len(),
            "completed": completed,
            "ready": ready,
            "blocked": blocked,
            "pending": pending,
            "unresolved_discovery_tasks": unresolved_discovery,
        })
    });

    // SME-supplied data inputs registered via the Inputs inspector
    // tab. The LLM sees a compact summary (no per-file manifest, no
    // sha256s — those are runtime concerns the agent reads from
    // CONTEXT.md). When `inputs` is non-empty, the LLM should
    // acknowledge the SME's data source rather than asking for
    // accessions, and the data_acquisition stage's discovery layer
    // will prefer `sme_supplied_*` methods over public-repo fetchers.
    let inputs_summary: Vec<serde_json::Value> = session
        .inputs
        .iter()
        .map(|i| {
            let total_bytes: u64 = i.files.iter().map(|f| f.size_bytes).sum();
            serde_json::json!({
                "input_id": i.input_id,
                "label": i.label,
                "kind": i.kind,
                "n_files": i.files.len(),
                "total_bytes": total_bytes,
            })
        })
        .collect();

    // Path-hint summary (e2e #13). When the path-hint extractor
    // pulled filesystem paths from SME prose that resolve under the
    // allowlist, surface them so the LLM can offer to register them.
    // We send the verbatim mention, the canonicalized root the SME
    // would actually be registering, the file extension that matched,
    // and whether the original mention pointed at a file (so the LLM
    // can phrase the suggestion naturally: "I see you mentioned the
    // file X — want me to register its directory Y so the agent can
    // read it?").
    let pending_input_hints: Vec<serde_json::Value> = session
        .pending_input_hints
        .iter()
        .map(|h| {
            serde_json::json!({
                "raw_mention": h.raw_mention,
                "canonical_root": h.canonical_root,
                "matched_extension": h.matched_extension,
                "file_mention": h.file_mention,
                "file_relpath": h.file_relpath,
            })
        })
        .collect();

    let body = serde_json::json!({
        "state": session.state,
        "intake_methods": session.intake_methods,
        "classification": session.classification,
        // `is_confirmed()` replaces the legacy `user_confirmed` bool.
        // The wire field name stays the same so the LLM tool-result
        // contract is unchanged; the underlying gate is a three-way
        // (token-present + pending-emission-set +
        // summary-hash-matches) check.
        "user_confirmed": session.is_confirmed(),
        // The LLM reads its class from here; it does not author it.
        // Closed enum, snake_case on the wire.
        "project_class": session.project_class,
        "dag": dag_summary,
        "intake_prose_len": session.intake_prose.len(),
        "inputs": inputs_summary,
        "pending_input_hints": pending_input_hints,
    });
    ToolResult::ok(body)
}

/// return the structured result for a completed task.
/// The session DAG is the source of truth for this phase; /// artifact-index expansion via the server endpoint. Pending/Running
/// tasks return `PreconditionFailure` so the LLM can tell the SME the
/// task hasn't finished yet.
pub(super) fn get_task_result(session: &Session, task_id: &str) -> ToolResult {
    let Some(dag) = session.current_dag() else {
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: "no DAG on this session yet — nothing to fetch a result for".into(),
            hint: "Make sure append_intake_prose has run and the DAG is built.".into(),
        });
    };
    let Some(task) = dag.tasks.get(task_id) else {
        let candidates: Vec<String> = dag.tasks.keys().take(8).map(|id| id.to_string()).collect();
        return ToolResult::err(ToolError::ValidationFailure {
            reason: format!("task '{}' not found in DAG", task_id),
            valid_alternatives: candidates,
            hint: "Use one of the task ids returned by get_session_state.".into(),
        });
    };

    match &task.state {
        TaskState::Completed { result } => {
            // §3.4 — return a summarized envelope rather than the full
            // result JSON. The full payload (potentially kilobytes of
            // narrative + tables) is still available via
            // `GET /api/chat/session/:id/task/:task_id/result`, which
            // the UI's ResultReviewTurnCard already calls. Replaying
            // kilobytes of narrative through every subsequent user
            // turn is the dominant bloat source.
            let summary = summarize_result(result, session.id, task_id);
            ToolResult::ok(serde_json::json!({
                "task_id": task_id,
                "status": "completed",
                "description": task.description,
                "kind": task.kind,
                "summary": summary,
            }))
        }
        TaskState::Failed { reason } => ToolResult::ok(serde_json::json!({
            "task_id": task_id,
            "status": "failed",
            "description": task.description,
            "kind": task.kind,
            "reason": reason,
        })),
        TaskState::Blocked { record } => ToolResult::ok(serde_json::json!({
            "task_id": task_id,
            "status": "blocked",
            "description": task.description,
            "kind": task.kind,
            "record": record,
        })),
        TaskState::Pending | TaskState::Ready | TaskState::Running { .. } => {
            let state_label = match &task.state {
                TaskState::Pending => "pending",
                TaskState::Ready => "ready",
                TaskState::Running { .. } => "running",
                _ => "unknown",
            };
            ToolResult::err(ToolError::PreconditionFailure {
                reason: format!(
                    "task '{}' is still {} — no result yet",
                    task_id, state_label
                ),
                hint: "Wait for the harness to finish the task, then ask again.".into(),
            })
        }
    }
}

/// §3.4 — turn a full task-result JSON into a compact summary envelope.
/// Conventionally-named fields are pulled into headline slots; unknown
/// fields are either passed through (small primitives), referenced by
/// name (large strings/arrays), or elided entirely.
///
/// Output shape:
/// ```json
/// {
/// "key_metrics": { /* first N primitive fields */ },
/// "narrative_excerpt": "...truncated...",
/// "artifact_paths": ["runtime/outputs/.../file.csv",...],
/// "table_row_counts": { "de_results": 1240,... },
/// "full_result_available_via": "GET /api/chat/session/:id/task/:task_id/result"
/// }
/// ```
fn summarize_result(result: &Value, session_id: uuid::Uuid, task_id: &str) -> Value {
    let mut key_metrics = serde_json::Map::new();
    let mut artifact_paths: Vec<Value> = Vec::new();
    let mut table_row_counts = serde_json::Map::new();
    let mut narrative_excerpt: Option<String> = None;

    if let Some(obj) = result.as_object() {
        for (k, v) in obj {
            // Narrative-like fields: keep a trimmed excerpt.
            if (k == "narrative" || k == "narrative_markdown" || k == "report" || k == "summary")
                && v.is_string()
            {
                if let Some(s) = v.as_str() {
                    narrative_excerpt = Some(truncate_str(s, NARRATIVE_EXCERPT_MAX));
                }
                continue;
            }

            // Artifact-like fields: expose paths, not contents.
            if (k == "artifacts" || k == "artifact_paths" || k == "outputs" || k == "files")
                && v.is_array()
            {
                if let Some(arr) = v.as_array() {
                    for item in arr.iter().take(20) {
                        artifact_paths.push(item.clone());
                    }
                }
                continue;
            }

            // Table-like fields: record row count, skip content.
            if (k == "tables" || k.ends_with("_rows") || k.ends_with("_table")) && v.is_array() {
                if let Some(arr) = v.as_array() {
                    table_row_counts.insert(k.clone(), Value::Number((arr.len() as u64).into()));
                }
                continue;
            }

            // Primitive metrics passthrough, capped.
            if (v.is_number() || v.is_boolean()) && key_metrics.len() < SUMMARY_MAX_METRIC_KEYS {
                key_metrics.insert(k.clone(), v.clone());
                continue;
            }

            // Short strings pass through too.
            if let Some(s) = v.as_str() {
                if s.len() <= 80 && key_metrics.len() < SUMMARY_MAX_METRIC_KEYS {
                    key_metrics.insert(k.clone(), v.clone());
                    continue;
                }
            }
        }
    }

    let mut envelope = serde_json::Map::new();
    if !key_metrics.is_empty() {
        envelope.insert("key_metrics".into(), Value::Object(key_metrics));
    }
    if let Some(excerpt) = narrative_excerpt {
        envelope.insert("narrative_excerpt".into(), Value::String(excerpt));
    }
    if !artifact_paths.is_empty() {
        envelope.insert("artifact_paths".into(), Value::Array(artifact_paths));
    }
    if !table_row_counts.is_empty() {
        envelope.insert("table_row_counts".into(), Value::Object(table_row_counts));
    }
    envelope.insert(
        "full_result_available_via".into(),
        Value::String(format!(
            "GET /api/chat/session/{}/task/{}/result",
            session_id, task_id
        )),
    );
    Value::Object(envelope)
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Avoid slicing a multi-byte UTF-8 boundary.
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summary_extracts_narrative_artifacts_metrics() {
        let v = json!({
            "method": "DESeq2",
            "n_de_genes": 1247,
            "alpha": 0.05,
            "narrative_markdown": "# Result\n\nWe identified 1,247 DE genes at FDR < 0.05...",
            "artifacts": ["runtime/outputs/de/results.csv", "runtime/outputs/de/volcano.png"],
            "tables": [{"gene": "BRCA1"}, {"gene": "TP53"}],
        });
        let sid = uuid::Uuid::new_v4();
        let out = summarize_result(&v, sid, "de_analysis");
        assert!(out["key_metrics"]["method"].is_string());
        assert_eq!(out["key_metrics"]["n_de_genes"], 1247);
        assert!(out["narrative_excerpt"]
            .as_str()
            .unwrap()
            .starts_with("# Result"));
        assert_eq!(out["artifact_paths"].as_array().unwrap().len(), 2);
        assert_eq!(out["table_row_counts"]["tables"], 2);
        assert!(out["full_result_available_via"]
            .as_str()
            .unwrap()
            .contains("/api/chat/session/"));
    }

    #[test]
    fn narrative_is_truncated_to_excerpt_length() {
        let long = "x".repeat(1000);
        let v = json!({
            "narrative": long,
        });
        let sid = uuid::Uuid::new_v4();
        let out = summarize_result(&v, sid, "t");
        let excerpt = out["narrative_excerpt"].as_str().unwrap();
        assert!(excerpt.chars().count() <= NARRATIVE_EXCERPT_MAX + 1); // +1 for ellipsis
        assert!(excerpt.ends_with('…'));
    }

    #[test]
    fn summary_is_smaller_than_raw_result_for_realistic_payload() {
        // §3.4 target: the summary envelope shrinks a typical compute
        // task result by >50%. If this regresses the summarizer is
        // passing through content it shouldn't.
        let v = json!({
            "narrative_markdown": "# Analysis\n".to_string() + &"lorem ipsum ".repeat(200),
            "method": "Seurat v5",
            "n_cells": 12345,
            "n_genes": 2134,
            "tables": (0..100).map(|i| json!({"gene": format!("G{}", i), "padj": 0.01})).collect::<Vec<_>>(),
            "artifacts": ["a.csv", "b.csv", "c.csv"],
        });
        let sid = uuid::Uuid::new_v4();
        let out = summarize_result(&v, sid, "t");
        let raw_len = v.to_string().len();
        let summary_len = out.to_string().len();
        assert!(
            (summary_len as f64) / (raw_len as f64) < 0.5,
            "summary should be <50% of raw; got {}/{} = {:.0}%",
            summary_len,
            raw_len,
            100.0 * summary_len as f64 / raw_len as f64
        );
    }

    #[test]
    fn non_object_result_produces_reference_only_envelope() {
        // A non-object result (e.g., a bare string or number — unusual
        // but legal) yields an envelope with just the REST reference.
        let v = json!("some string");
        let sid = uuid::Uuid::new_v4();
        let out = summarize_result(&v, sid, "t");
        let obj = out.as_object().unwrap();
        assert_eq!(obj.len(), 1);
        assert!(obj.contains_key("full_result_available_via"));
    }
}
