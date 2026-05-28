//! Per-task LLM-generated code capture — written by `agent-claude.sh`
//! to `runtime/outputs/<task_id>/agent-code.json` at execution time.
//!
//! ## Byte-reproducibility note
//! This file is explicitly **excluded** from the byte-diff reproducibility
//! baseline. It captures LLM-stochastic content (prompt, response text,
//! executed code) and wall-clock timestamps, both of which vary between
//! runs even with identical inputs. The `make verify-reproducibility`
//! target skips `agent-code.json` files by design.
//!
//! ## Field truthfulness
//! The `prompt` and `executed_code` fields are populated by
//! `agent-claude.sh`, which can capture them faithfully from the Claude
//! Code CLI invocation. The `response_text` field is reserved for future
//! expansion: Claude Code's `--output-format=json` terminal blob does not
//! separate the assistant's natural-language commentary from the executed
//! code in a machine-readable way at the time of writing. When the field
//! cannot be populated truthfully it is set to an empty string.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Captures the LLM interaction that produced the code executed for a
/// single harness task. Written to
/// `runtime/outputs/<task_id>/agent-code.json` by the agent wrapper
/// script; read by the per-task result endpoint for display in the UI.
///
/// Timestamps are RFC 3339 strings (e.g. `"2026-05-22T10:00:00Z"`)
/// to match the existing DAG/task timestamp conventions used throughout
/// this codebase and to remain compatible with ts-rs's `String` mapping.
///
/// This type is `#[non_exhaustive]` per workspace convention for all
/// ts-rs-exported types that cross the API boundary — adding a field is
/// a minor (non-breaking) change.
#[non_exhaustive]
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct AgentCodeRecord {
    /// The prompt that was sent to the LLM. For `agent-claude.sh` this is
    /// the concatenation of `PROMPT.md`, the package location block, and
    /// the task-execution body — the full string passed via `-p`.
    pub prompt: String,

    /// The assistant's natural-language response text, separated from
    /// the executed code. Currently empty (`""`) because Claude Code's
    /// `--output-format=json` terminal blob does not separate narrative
    /// from code in a machine-readable way; reserved for future
    /// expansion when the CLI exposes a transcript format that does.
    pub response_text: String,

    /// The code that was executed during the task. For `agent-claude.sh`
    /// this is parsed from the agent log using heuristics (shebang
    /// detection, code-fence patterns). May be empty when no parsable
    /// code block was found.
    pub executed_code: String,

    /// Programming language inferred from the executed code.
    /// One of `"Python"`, `"R"`, `"Bash"`, or `"unknown"`.
    pub language: String,

    /// RFC 3339 wall-clock time when the agent wrapper script started
    /// the Claude Code invocation for this task.
    pub started_at: String,

    /// RFC 3339 wall-clock time when the Claude Code invocation exited
    /// (or the wrapper captured the log).
    pub completed_at: String,
}

impl AgentCodeRecord {
    /// Construct a minimal record with the given prompt and timestamps.
    /// All other fields default to empty / "unknown".
    pub fn new(prompt: String, started_at: String, completed_at: String) -> Self {
        AgentCodeRecord {
            prompt,
            response_text: String::new(),
            executed_code: String::new(),
            language: "unknown".to_string(),
            started_at,
            completed_at,
        }
    }
}
