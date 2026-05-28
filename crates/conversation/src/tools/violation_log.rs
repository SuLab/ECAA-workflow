//! Vocabulary-violation log emitter for Tier 17.x (Bucket C -- boundary
//! enforcement).
//!
//! Every time the dispatcher rejects a tool call for one of the four
//! boundary-violation reasons, callers invoke [`emit`] to append a JSON
//! Row to `<runtime_root>/vocabulary-violations.jsonl`. The file is
//! created on first write and appended atomically (one `writeln!` per
//! call).
//!
//! ## Design rules
//!
//! - **Never panics.** All I/O errors are routed to [`tracing::warn!`]
//!   and the function returns normally so chat behaviour is never
//!   regressed by a log write failure.
//! - **No `unwrap` / `expect`.** Follows project discipline.
//! - **Sync-only.** `violation_log` lives in `crates/conversation` but
//!   the emission path must be callable from sync handler code (the
//!   `emit_package` rejection in `tools/emit.rs` runs inside an async
//!   context but the write itself is a plain blocking `fs` call -- that
//!   is acceptable for a single `writeln!` against a local file).

use serde::Serialize;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// One dispatcher rejection event. Serialised as a single JSON line into
/// `<runtime_root>/vocabulary-violations.jsonl`.
#[derive(Debug, Serialize)]
pub(crate) struct VocabularyViolation {
    /// Session that generated the violation.
    pub session_id: String,
    /// Wall-clock milliseconds since Unix epoch at the moment of
    /// rejection (not the session start).
    pub timestamp_ms: u64,
    /// Category of the boundary violation.
    pub kind: ViolationKind,
    /// Model id string (e.g. `claude-sonnet-4-6`) that generated the
    /// rejected call.
    pub model: String,
    /// Tool name the LLM attempted to call, when known (None for
    /// `UnknownTool` where the name didn't deserialize to a `Tool`
    /// variant).
    pub tool_name: Option<String>,
    /// Human-readable summary of *why* this specific call was rejected
    /// (not the full error -- just the deciding predicate).
    pub detail: String,
}

/// The four categories of dispatcher boundary violations tracked by
/// Tier 17.x.
#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ViolationKind {
    /// The LLM supplied a stage id that does not exist in the current
    /// DAG (e.g. a hallucinated `align_reads_v2` when only `align_reads`
    /// Exists). Corresponds to the `ToolError::ValidationFailure` /
    /// `ToolError::unknown_stage()` paths in the tool handlers.
    InventedStageId,
    /// An alone-in-turn tool (e.g. `emit_package`, `amend_stage_method`)
    /// appeared alongside >= 1 other tool in the same assistant turn batch.
    BatchedHighImpactTools,
    /// The LLM recommended a specific method unprompted, violating the
    /// Tool-method-neutrality rule in `prompt_role.txt`. This variant is
    /// populated by the Tier 17.3 quarterly audit path, not the live
    /// dispatcher (the dispatcher cannot detect unprompted method
    /// recommendations at call-parse time).
    ///
    /// `#[allow(dead_code)]` -- populated by the Tier 17.3 quarterly
    /// side-call audit, not the live dispatcher.
    #[allow(dead_code)]
    UnpromptedMethodRecommendation,
    /// `emit_package` was called before `session.user_confirmed == true`
    /// (i.e. before the SME clicked the Confirm button in the UI).
    PrematureEmitAttempt,
    /// The LLM sent a `tool_use` block whose `name` field did not
    /// deserialize to any known [`Tool`](super::Tool) variant.
    ///
    /// # Wiring note
    ///
    /// Unknown tool names fail deserialization in
    /// `crates/conversation/src/anthropic/client.rs::tool_from_api` and
    /// `accumulator.rs::StreamAccumulator::finalize` -- before the
    /// dispatcher is reached. Callers in the service layer that catch a
    /// parse failure containing "deserializing tool_use payload" should
    /// call `violation_log::emit` with this kind and whatever session
    /// context is available at that point.
    ///
    /// `#[allow(dead_code)]` -- wired from the anthropic/ layer, not tools/.
    #[allow(dead_code)]
    UnknownTool,
}

/// Append `violation` as a JSON line to
/// `<runtime_root>/vocabulary-violations.jsonl`.
///
/// Creates the file on first write (and any missing parent directories).
/// All I/O errors are swallowed and emitted as [`tracing::warn!`] so
/// callers never observe a failure -- chat behaviour must not regress on
/// a log write error.
pub(crate) fn emit(violation: &VocabularyViolation, runtime_root: &Path) {
    let json = match serde_json::to_string(violation) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                session_id = %violation.session_id,
                kind = ?violation.kind,
                error = %e,
                "violation_log: serde_json serialization failed; violation not written"
            );
            return;
        }
    };

    if let Err(e) = std::fs::create_dir_all(runtime_root) {
        tracing::warn!(
            session_id = %violation.session_id,
            runtime_root = %runtime_root.display(),
            error = %e,
            "violation_log: could not create runtime dir; violation not written"
        );
        return;
    }

    let path = runtime_root.join("vocabulary-violations.jsonl");
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{}", json)?;
        Ok(())
    })();

    if let Err(e) = result {
        tracing::warn!(
            session_id = %violation.session_id,
            path = %path.display(),
            error = %e,
            "violation_log: could not write vocabulary-violations.jsonl; violation not written"
        );
    }
}

/// Current Unix timestamp in milliseconds. Falls back to 0 on
/// `SystemTime` errors (which are essentially impossible on a sane OS
/// but this path must never panic).
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
