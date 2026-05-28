//! `ToolErrorEnvelope` — the structured capture of a task-execution
//! failure that crosses the harness/server boundary.
//!
//! Each backend (Local, AWS, SLURM) writes an envelope to
//! `runtime/outputs/<task_id>/error.json` whenever an agent invocation
//! exits non-zero or the backend's job-control surface reports a
//! terminal failure. The server's `/progress` handler reads it and
//! upgrades `BlockerKind::AgentError` to `BlockerKind::ToolError` so
//! the chat surface can route to a typed remediation card.
//!
//! Schema is intentionally flat and bounded — every list field is
//! capped so a runaway stderr can't bloat the WAL or the SSE event
//! stream.

use crate::ids::{StageId, TaskId};
use std::collections::BTreeMap;

// `ToolErrorEnvelope` lives at `crates/ecaa-types/src/error_envelope.rs`
// so the canonical `BlockerKind` binding can stand alone without
// pulling the synthesis pipeline. The wire-shape struct is re-exported
// here for backward compatibility with `scripps_workflow_core::error_envelope::ToolErrorEnvelope`
// consumers; envelope synthesis (`synthesize`, `classify_error`,
// `EnvelopeInput`, the line-tail bound constants) stays in core because
// it is harness-side glue.
pub use scripps_workflow_ecaa_types::error_envelope::ToolErrorEnvelope;

/// Maximum lines retained in `stderr_tail`. Most real-world failures
/// have their causal frame within the last ~30 lines; 50 leaves
/// margin for verbose tools (R, conda) without unbounded growth.
pub const STDERR_TAIL_LIMIT: usize = 50;

/// Maximum lines retained in `stdout_tail`. Stdout rarely carries the
/// causal frame for a failure but is occasionally useful for context
/// (tool versions printed at startup, last progress line, etc.).
pub const STDOUT_TAIL_LIMIT: usize = 20;

/// Maximum frames retained in `traceback`. Python tracebacks beyond
/// ~30 frames are typically deep stdlib chains that don't add signal.
pub const TRACEBACK_FRAME_LIMIT: usize = 30;

/// Maximum length of the one-line `message` field. Keeps the payload
/// renderable as a single chip in the BlockerCard header.
pub const MESSAGE_MAX_LEN: usize = 200;

/// Inputs to envelope synthesis. Caller fills the pieces it has and
/// passes `None` for the rest; the synthesizer computes the
/// derived fields ([`classify_error`], message truncation, list bounds).
#[derive(Debug, Clone, Default)]
pub struct EnvelopeInput<'a> {
    /// Task id.
    pub task_id: TaskId,
    pub stage_id: StageId,
    pub library: Option<String>,
    /// Library version.
    pub library_version: Option<String>,
    /// Stderr.
    pub stderr: &'a str,
    /// Stdout.
    pub stdout: &'a str,
    /// Exit code.
    pub exit_code: Option<i32>,
    /// Signal.
    pub signal: Option<String>,
    /// Wallclock secs.
    pub wallclock_secs: Option<u64>,
    /// Peak memory mb.
    pub peak_memory_mb: Option<u64>,
    /// Input summary.
    pub input_summary: BTreeMap<String, serde_json::Value>,
    /// Executor.
    pub executor: String,
    /// Executor context.
    pub executor_context: BTreeMap<String, String>,
    /// Captured at.
    pub captured_at: String,
    /// Attempt.
    pub attempt: u32,
}

/// Build a [`ToolErrorEnvelope`] from raw stderr/stdout + side-channel
/// metadata. Same function for every backend; backends only differ in
/// how they collect the inputs.
pub fn synthesize(input: EnvelopeInput<'_>) -> ToolErrorEnvelope {
    let stderr_lines: Vec<&str> = input.stderr.lines().collect();
    let stdout_lines: Vec<&str> = input.stdout.lines().collect();
    let stderr_tail: Vec<String> = tail_lines(&stderr_lines, STDERR_TAIL_LIMIT);
    let stdout_tail: Vec<String> = tail_lines(&stdout_lines, STDOUT_TAIL_LIMIT);
    let traceback = extract_python_traceback(&stderr_lines);
    let error_class = classify_error(
        &stderr_lines,
        input.exit_code,
        input.signal.as_deref(),
        traceback.is_some(),
    );
    let message = pick_message(&stderr_lines, &error_class);

    ToolErrorEnvelope {
        task_id: input.task_id.to_string(),
        stage_id: input.stage_id.into(),
        library: input.library,
        library_version: input.library_version,
        error_class,
        message,
        stderr_tail,
        stdout_tail,
        traceback,
        exit_code: input.exit_code,
        signal: input.signal,
        wallclock_secs: input.wallclock_secs,
        peak_memory_mb: input.peak_memory_mb,
        input_summary: input.input_summary,
        executor: input.executor,
        executor_context: input.executor_context,
        captured_at: input.captured_at,
        attempt: input.attempt,
        schema_version: 1,
    }
}

fn tail_lines(lines: &[&str], n: usize) -> Vec<String> {
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

/// Pull the last contiguous Python traceback out of stderr.
/// Returns `None` when the failure isn't a Python exception.
fn extract_python_traceback(stderr_lines: &[&str]) -> Option<Vec<String>> {
    let mut last_start = None;
    for (i, line) in stderr_lines.iter().enumerate() {
        if line.starts_with("Traceback (most recent call last):") {
            last_start = Some(i);
        }
    }
    let start = last_start?;
    let frames: Vec<String> = stderr_lines[start..]
        .iter()
        .take(TRACEBACK_FRAME_LIMIT)
        .map(|s| s.to_string())
        .collect();
    Some(frames)
}

/// Coarse string classification. Order matters — most-specific
/// patterns first. The proposer can also re-classify on the full
/// stderr when needed; this is a fast-path label for the UI chip
/// and a hint to the proposer.
pub fn classify_error(
    stderr_lines: &[&str],
    exit_code: Option<i32>,
    signal: Option<&str>,
    has_python_traceback: bool,
) -> String {
    let stderr_blob: String = stderr_lines.join("\n");
    let blob_lower = stderr_blob.to_ascii_lowercase();

    if matches!(signal, Some(s) if s.eq_ignore_ascii_case("SIGKILL"))
        || blob_lower.contains("out of memory")
        || blob_lower.contains("memoryerror")
        || blob_lower.contains("cannot allocate memory")
        || blob_lower.contains("std::bad_alloc")
        || blob_lower.contains("killed")
            && (blob_lower.contains("oom") || blob_lower.contains("memory"))
    {
        return "OOM".to_string();
    }
    if matches!(signal, Some(s) if s.eq_ignore_ascii_case("SIGSEGV")) {
        return "SegFault".to_string();
    }
    if matches!(signal, Some(s) if s.eq_ignore_ascii_case("SIGTERM"))
        || blob_lower.contains("wallclock")
        || blob_lower.contains("time limit")
        || blob_lower.contains("timeout")
    {
        return "WallclockExceeded".to_string();
    }
    if blob_lower.contains("no space left on device") || blob_lower.contains("disk full") {
        return "DiskFull".to_string();
    }
    if blob_lower.contains("permission denied") {
        return "PermissionDenied".to_string();
    }
    if blob_lower.contains("could not resolve host")
        || blob_lower.contains("connection refused")
        || blob_lower.contains("network is unreachable")
        || blob_lower.contains("ssl handshake")
        || blob_lower.contains("503 service unavailable")
        || blob_lower.contains("rate limit")
    {
        return "Network".to_string();
    }
    if blob_lower.contains("modulenotfounderror")
        || blob_lower.contains("importerror")
        || blob_lower.contains("there is no package called")
        || blob_lower.contains("command not found")
    {
        return "MissingDependency".to_string();
    }
    if blob_lower.contains("singular matrix")
        || blob_lower.contains("did not converge")
        || blob_lower.contains("convergence failed")
        || blob_lower.contains("nan ")
        || blob_lower.contains("inf ")
    {
        return "NumericalInstability".to_string();
    }
    if has_python_traceback {
        if let Some(class) = stderr_lines
            .iter()
            .rev()
            .find_map(|l| l.split(':').next().filter(|s| s.ends_with("Error")))
        {
            return class.to_string();
        }
        return "PythonException".to_string();
    }
    match exit_code {
        Some(0) => "Success".to_string(),
        Some(code) => format!("NonZeroExit({})", code),
        None => "UnknownFailure".to_string(),
    }
}

fn pick_message(stderr_lines: &[&str], error_class: &str) -> String {
    let candidate = stderr_lines
        .iter()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| format!("{} (no stderr captured)", error_class));
    if candidate.chars().count() <= MESSAGE_MAX_LEN {
        candidate
    } else {
        let truncated: String = candidate.chars().take(MESSAGE_MAX_LEN).collect();
        format!("{}…", truncated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(stderr: &'static str) -> EnvelopeInput<'static> {
        EnvelopeInput {
            task_id: "t1".into(),
            stage_id: "alignment".into(),
            stderr,
            executor: "local".into(),
            captured_at: "2026-05-04T00:00:00Z".into(),
            ..Default::default()
        }
    }

    #[test]
    fn synthesize_roundtrips_serde() {
        let env = synthesize(input("MemoryError: Unable to allocate 32 GiB"));
        let json = serde_json::to_string(&env).unwrap();
        let back: ToolErrorEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
        assert_eq!(env.error_class, "OOM");
        assert_eq!(env.schema_version, 1);
    }

    #[test]
    fn classifies_oom_from_signal() {
        let cls = classify_error(&[], Some(137), Some("SIGKILL"), false);
        assert_eq!(cls, "OOM");
    }

    #[test]
    fn classifies_oom_from_message() {
        let cls = classify_error(
            &["pandas internal error", "MemoryError: out of memory"],
            Some(1),
            None,
            true,
        );
        assert_eq!(cls, "OOM");
    }

    #[test]
    fn classifies_wallclock_from_signal() {
        let cls = classify_error(&[], Some(124), Some("SIGTERM"), false);
        assert_eq!(cls, "WallclockExceeded");
    }

    #[test]
    fn classifies_missing_dependency() {
        let cls = classify_error(
            &["ModuleNotFoundError: No module named 'pydeseq2'"],
            Some(1),
            None,
            true,
        );
        assert_eq!(cls, "MissingDependency");
    }

    #[test]
    fn classifies_network() {
        let cls = classify_error(
            &["urllib.error.HTTPError: 503 Service Unavailable"],
            Some(1),
            None,
            true,
        );
        assert_eq!(cls, "Network");
    }

    #[test]
    fn classifies_disk_full() {
        let cls = classify_error(
            &["IOError: [Errno 28] No space left on device"],
            Some(1),
            None,
            true,
        );
        assert_eq!(cls, "DiskFull");
    }

    #[test]
    fn classifies_numerical_instability() {
        let cls = classify_error(&["LinAlgError: Singular matrix"], Some(1), None, true);
        assert_eq!(cls, "NumericalInstability");
    }

    #[test]
    fn classifies_python_exception_class_name() {
        let cls = classify_error(
            &[
                "Traceback (most recent call last):",
                "  File 'x.py' line 1",
                "ValueError: y has only one unique value",
            ],
            Some(1),
            None,
            true,
        );
        assert_eq!(cls, "ValueError");
    }

    #[test]
    fn classifies_nonzero_exit() {
        let cls = classify_error(&["weird tool noise"], Some(42), None, false);
        assert_eq!(cls, "NonZeroExit(42)");
    }

    #[test]
    fn extracts_python_traceback() {
        let lines = vec![
            "starting up",
            "Traceback (most recent call last):",
            "  File 'a.py', line 10, in <module>",
            "ValueError: bad input",
        ];
        let tb = extract_python_traceback(&lines).expect("traceback");
        assert_eq!(tb.len(), 3);
        assert!(tb[0].starts_with("Traceback"));
    }

    #[test]
    fn picks_only_last_traceback_when_multiple() {
        let lines = vec![
            "Traceback (most recent call last):",
            "  File 'first.py', line 1, in <module>",
            "FirstError: ignored",
            "retrying...",
            "Traceback (most recent call last):",
            "  File 'second.py', line 5, in <module>",
            "SecondError: real",
        ];
        let tb = extract_python_traceback(&lines).expect("tb");
        assert!(tb.iter().any(|l| l.contains("second.py")));
        assert!(!tb.iter().any(|l| l.contains("first.py")));
    }

    #[test]
    fn stderr_tail_is_bounded() {
        let big: String = (0..200)
            .map(|i| format!("line-{}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let env = synthesize(EnvelopeInput {
            task_id: "t".into(),
            stage_id: "s".into(),
            stderr: Box::leak(big.into_boxed_str()),
            executor: "local".into(),
            captured_at: "now".into(),
            ..Default::default()
        });
        assert_eq!(env.stderr_tail.len(), STDERR_TAIL_LIMIT);
        assert_eq!(env.stderr_tail.last().unwrap(), "line-199");
    }

    #[test]
    fn message_truncation() {
        let long = "x".repeat(500);
        let env = synthesize(EnvelopeInput {
            task_id: "t".into(),
            stage_id: "s".into(),
            stderr: Box::leak(long.into_boxed_str()),
            executor: "local".into(),
            captured_at: "now".into(),
            ..Default::default()
        });
        assert!(env.message.chars().count() <= MESSAGE_MAX_LEN + 1);
        assert!(env.message.ends_with('…'));
    }
}
