//! Sync file-IO helpers for emit-time telemetry rows.
//!
//! Both routines write to `<pkg>/runtime/*.jsonl` files; both are sync
//! (called from the emit path which is itself sync) and best-effort
//! (`std::io::Result` returned to the caller, which uniformly wraps in
//! `let _ =...`). Neither touches `MetricsStore` — they take an
//! already-rendered `&SessionMetrics` snapshot and serialize a subset
//! of its fields per the tier-runner contract.

use super::session_metrics::SessionMetrics;
use crate::session::SessionId;

/// Append a single
/// cost-ledger row to `<pkg>/runtime/cost-ledger.jsonl`.
///
/// Sync I/O. Best-effort: if the parent directory doesn't exist, this
/// returns the IO error to the caller; emit callers wrap the call in
/// `let _ =...` so a ledger-write failure never aborts the emit. The
/// row carries the four cost buckets the operational eval plan tracks
/// (`chat / agent / scorer / side_call`) plus their sum
/// (`total_cost_usd`) and an ISO-8601 emit timestamp.
///
/// One row per emit. Amendments and re-emits append rather than
/// overwriting so the ledger doubles as a per-session cost history.
/// Tier 14 reads the file and sums `total_cost_usd` across all rows.
pub fn write_cost_ledger_row(
    pkg_runtime_dir: &std::path::Path,
    session_id: SessionId,
    metrics: &SessionMetrics,
) -> std::io::Result<()> {
    use std::io::Write;
    let total = metrics.chat_cost_usd
        + metrics.agent_cost_usd
        + metrics.scorer_cost_usd
        + metrics.side_call_cost_usd;
    let row = serde_json::json!({
        "session_id": session_id.to_string(),
        "emitted_at": scripps_workflow_core::time_helpers::now_rfc3339(),
        "chat_cost_usd": metrics.chat_cost_usd,
        "agent_cost_usd": metrics.agent_cost_usd,
        "scorer_cost_usd": metrics.scorer_cost_usd,
        "side_call_cost_usd": metrics.side_call_cost_usd,
        "total_cost_usd": total,
    });
    std::fs::create_dir_all(pkg_runtime_dir)?;
    let path = pkg_runtime_dir.join("cost-ledger.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(f, "{}", row)?;
    Ok(())
}

/// Append a single SME-experience row to
/// `<pkg>/runtime/session-metrics.jsonl`.
///
/// Sync I/O. Best-effort: a write failure returns the `io::Error` to the
/// Caller; emit callers wrap the call in `let _ =...` so a file-write
/// failure never aborts the emit. The row carries the four fields the
/// Tier 16.2–16.4 eval runners read from this file:
///
/// - `followup_count` — SME clarification turns before confirmation
/// - `amendment_count` — post-emission method amendments
/// - `blockers_encountered` — typed blocker events with recovery outcome
/// - `is_ambiguous` — whether the session's intake was low-confidence
///
/// Plus the three timestamp/id fields the aggregator always needs
/// (`session_id`, `created_at_ms`, `emitted_at_ms`), the total turn count,
/// and a `schema_version` guard.
///
/// The file is appended, not overwritten, so re-emits (amendments) append a
/// new row with updated counts rather than clobbering the first. The eval
/// runner's `load_session_metrics` / `load_session_metrics_file` helpers
/// accept multi-row JSONL and return the last row per session_id for
/// point-in-time analysis, or all rows for history.
pub fn write_session_metrics_row(
    pkg_runtime_dir: &std::path::Path,
    session_id: SessionId,
    created_at_ms: u64,
    metrics: &SessionMetrics,
) -> std::io::Result<()> {
    use std::io::Write;
    let emitted_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    // Serialize `blockers_encountered` as a JSON array inline so the row
    // is self-contained. serde_json::to_value is infallible for our type.
    let blockers_val = serde_json::to_value(&metrics.blockers_encountered)
        .unwrap_or(serde_json::Value::Array(vec![]));
    let row = serde_json::json!({
        "session_id": session_id.to_string(),
        "created_at_ms": created_at_ms,
        "emitted_at_ms": emitted_at_ms,
        "turn_count": metrics.turn_count,
        "followup_count": metrics.followup_count,
        "amendment_count": metrics.amendment_count,
        "blockers_encountered": blockers_val,
        "is_ambiguous": metrics.is_ambiguous,
        "schema_version": 1u32,
    });
    std::fs::create_dir_all(pkg_runtime_dir)?;
    let path = pkg_runtime_dir.join("session-metrics.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(f, "{}", row)?;
    Ok(())
}
