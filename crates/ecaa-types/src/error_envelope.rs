//! `ToolErrorEnvelope` — structured capture of a task-execution failure
//! that crosses the harness/server boundary.
//!
//! The full envelope synthesis pipeline (`synthesize`, `classify_error`,
//! `EnvelopeInput`, the line-tail bound constants) stays in
//! `crates/core/src/error_envelope.rs`. Only the wire-shape struct is
//! lifted here because it appears directly in a `BlockerKind::ToolError`
//! variant payload — extracting the struct lets the canonical
//! `BlockerKind` binding stand alone.
//!
//! Re-exported from `scripps_workflow_core::error_envelope` for backward
//! compatibility with existing call sites.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Structured failure envelope written by every backend. The shape is
/// stable across `local | aws | slurm` so the server's blocker-mapper
/// and the remediation proposer can consume one type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ToolErrorEnvelope {
    /// Task that failed.
    pub task_id: String,
    /// Stage class the task implements (e.g. "alignment",
    /// "differential_expression"). Used by the proposer to consult
    /// the relevant taxonomy snippet.
    pub stage_id: String,
    /// Library / binary the proposer should reason about
    /// (e.g. "STAR", "DESeq2", "cellranger"). `None` when the
    /// failure precedes any tool invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub library: Option<String>,
    /// Library version when extractable. The proposer uses this to
    /// suggest version-specific remediations (e.g. known-bug downgrades).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub library_version: Option<String>,
    /// Coarse error class. Stable string vocabulary so the UI can
    /// show a typed chip and the proposer can match without parsing
    /// the full traceback. See `classify_error` for the heuristics.
    pub error_class: String,
    /// One-line summary for the BlockerCard header.
    /// Bounded to `MESSAGE_MAX_LEN` chars at synthesis time.
    pub message: String,
    /// Tail of stderr, oldest first. Bounded at synthesis time.
    pub stderr_tail: Vec<String>,
    /// Tail of stdout, oldest first. Bounded at synthesis time.
    pub stdout_tail: Vec<String>,
    /// Python traceback frames when the failure was a Python
    /// exception. Bounded at synthesis time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub traceback: Option<Vec<String>>,
    /// Process exit code when the task ran to completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exit_code: Option<i32>,
    /// Signal name when the process was terminated by signal
    /// (SIGKILL = OOM, SIGTERM = wallclock, SIGSEGV = crash).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub signal: Option<String>,
    /// Wall-clock seconds consumed before the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub wallclock_secs: Option<u64>,
    /// Peak resident-set memory observed in MiB. Local: VmHWM from
    /// `/proc/<pid>/status`. AWS: CloudWatch agent. SLURM: sacct MaxRSS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub peak_memory_mb: Option<u64>,
    /// Hash-summary of inputs at the time of failure: shapes, sizes,
    /// key column names. Never carries raw data — strictly metadata.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(type = "Record<string, unknown>")]
    pub input_summary: BTreeMap<String, serde_json::Value>,
    /// Backend that ran the task. Matches `Executor::name()`.
    pub executor: String,
    /// Backend-native context: instance_type for AWS, partition for
    /// SLURM, host for local.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub executor_context: BTreeMap<String, String>,
    /// Wall-clock RFC 3339 timestamp at capture.
    pub captured_at: String,
    /// Monotonic per-task attempt number. Lets the proposer see
    /// prior remediation outcomes via the overrides history.
    pub attempt: u32,
    /// Schema version. `1` is the only value today; bump on any
    /// breaking change to the on-disk shape.
    #[serde(default = "envelope_schema_v1")]
    pub schema_version: u32,
}

fn envelope_schema_v1() -> u32 {
    1
}
