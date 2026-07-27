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

/// Value recorded in the cost-ledger row's `clock` field when a genuine
/// run epoch was available and `emitted_at` carries it.
pub const CLOCK_RUN_EPOCH: &str = "run_epoch";

/// Value recorded in the cost-ledger row's `clock` field when no genuine
/// run epoch was available; `emitted_at` is then `null`.
pub const CLOCK_UNSET: &str = "unset";

/// Resolve the honest `(emitted_at, clock)` pair for a cost-ledger row.
///
/// `run_epoch_clock` (and therefore every emit-path timestamp) falls back
/// to the [`RUN_EPOCH_BASE`][base] FLOOR — exactly `2026-01-01T00:00:00Z` —
/// when `SOURCE_DATE_EPOCH` is unset, unparseable, or outside the genuine
/// run window. That floor is a deterministic sentinel, not a time: writing
/// it into `emitted_at` claims a January emit date for a run that happened
/// whenever it happened, which is a provenance statement the package cannot
/// back.
///
/// So this splits the two cases the clock deliberately collapses:
///
/// * a genuine in-window `SOURCE_DATE_EPOCH` → `emitted_at` = that instant,
///   `clock` = [`CLOCK_RUN_EPOCH`];
/// * no genuine run epoch → `emitted_at` = `null`, `clock` = [`CLOCK_UNSET`].
///
/// The clock itself is untouched: the in-window predicate mirrors
/// [`run_epoch_clock_from`][from], and the timestamp (when present) still
/// comes from that clock, so the row stays a pure function of
/// `SOURCE_DATE_EPOCH` and the ledger remains byte-reproducible.
///
/// [base]: ecaa_workflow_core::clock::RUN_EPOCH_BASE
/// [from]: ecaa_workflow_core::clock::run_epoch_clock_from
fn resolve_emitted_at(source_date_epoch: Option<&str>) -> (serde_json::Value, &'static str) {
    use ecaa_workflow_core::clock::{
        run_epoch_clock_from, Clock as _, RUN_EPOCH_BASE, RUN_WINDOW_END,
    };
    let genuine = source_date_epoch
        .and_then(|s| s.trim().parse::<i64>().ok())
        .is_some_and(|s| (RUN_EPOCH_BASE..RUN_WINDOW_END).contains(&s));
    if genuine {
        let at = run_epoch_clock_from(source_date_epoch).now_rfc3339();
        (serde_json::Value::String(at), CLOCK_RUN_EPOCH)
    } else {
        (serde_json::Value::Null, CLOCK_UNSET)
    }
}

/// Append a single
/// cost-ledger row to `<pkg>/runtime/cost-ledger.jsonl`.
///
/// Sync I/O. Best-effort: if the parent directory doesn't exist, this
/// returns the IO error to the caller; emit callers wrap the call in
/// `let _ =...` so a ledger-write failure never aborts the emit. The
/// row carries the four cost buckets the operational eval plan tracks
/// (`chat / agent / scorer / side_call`) plus their sum
/// (`total_cost_usd`), a metered-vs-unmetered label for the agent bucket,
/// and a DETERMINISTIC (run-epoch) emit timestamp.
///
/// **Metering (DR-9):** `chat`/`scorer`/`side_call` are API-metered per-token;
/// `agent_cost_usd` is metered only under `ECAA_AGENT_BILLING=api`. By default
/// agent code-gen bills against a Claude Max/Pro SUBSCRIPTION, so a `$0.00`
/// `agent_cost_usd` means UNMETERED, not free — `agent_billing_mode` +
/// `agent_cost_metered` label it so a reviewer never reads the zero as
/// "the agent cost nothing".
///
/// **Determinism + timestamp honesty:** `emitted_at` uses the run-epoch
/// clock (matching `dateCreated` / `Bagging-Date`), NOT the wall clock, so
/// re-emitting / re-exporting the same package does not churn the ledger row
/// (and the deposit `export` reseal that folds this file into the manifest
/// stays byte-reproducible). When no genuine run epoch was available the
/// clock returns its `2026-01-01T00:00:00Z` sentinel FLOOR; the row records
/// that as `emitted_at: null` + `clock: "unset"` rather than as a misleading
/// January date. See [`resolve_emitted_at`].
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
    // Agent billing mode: SUBSCRIPTION by default (agent-claude.sh), API only
    // when explicitly overridden. Anything other than "api" is subscription.
    let agent_billing_mode = match std::env::var("ECAA_AGENT_BILLING") {
        Ok(v) if v.eq_ignore_ascii_case("api") => "api",
        _ => "subscription",
    };
    let agent_cost_metered = agent_billing_mode == "api";
    // Bound the env read so the `&str` outlives the borrow handed to
    // `resolve_emitted_at`.
    let source_date_epoch = std::env::var("SOURCE_DATE_EPOCH").ok();
    let (emitted_at, clock) = resolve_emitted_at(source_date_epoch.as_deref());
    let row = serde_json::json!({
        "session_id": session_id.to_string(),
        "emitted_at": emitted_at,
        "clock": clock,
        "chat_cost_usd": metrics.chat_cost_usd,
        "agent_cost_usd": metrics.agent_cost_usd,
        "scorer_cost_usd": metrics.scorer_cost_usd,
        "side_call_cost_usd": metrics.side_call_cost_usd,
        "total_cost_usd": total,
        "agent_billing_mode": agent_billing_mode,
        "agent_cost_metered": agent_cost_metered,
        "cost_metering_note": "chat/scorer/side_call are API-metered per-token; \
            agent_cost_usd is metered only under ECAA_AGENT_BILLING=api. Under \
            subscription billing a $0.00 agent_cost_usd means UNMETERED (Claude \
            Max/Pro), not free.",
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
        // Product metrics. `time_to_emit_ms` is computed inline from the
        // two timestamps in this same row so it is always present and
        // correct regardless of `record_emit` call ordering; the rate
        // fields come from the snapshot.
        "time_to_emit_ms": emitted_at_ms.saturating_sub(created_at_ms),
        "tasks_succeeded": metrics.tasks_succeeded,
        "tasks_failed": metrics.tasks_failed,
        "task_success_rate": metrics.task_success_rate,
        "claims_checked": metrics.claims_checked,
        "claim_mismatches": metrics.claim_mismatches,
        "claim_mismatch_rate": metrics.claim_mismatch_rate,
        "method_recommendation_requests": metrics.method_recommendation_requests,
        "schema_version": 2u32,
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

#[cfg(test)]
mod emitted_at_tests {
    use super::{resolve_emitted_at, CLOCK_RUN_EPOCH, CLOCK_UNSET};
    use ecaa_workflow_core::clock::{RUN_EPOCH_BASE, RUN_WINDOW_END};

    /// A genuine in-window run epoch is recorded as a real RFC-3339 instant
    /// and labeled `run_epoch`.
    #[test]
    fn genuine_run_epoch_is_recorded_as_a_timestamp() {
        // 2026-07-25T00:00:00Z — inside `[RUN_EPOCH_BASE, RUN_WINDOW_END)`.
        let epoch = 1_784_937_600i64;
        assert!((RUN_EPOCH_BASE..RUN_WINDOW_END).contains(&epoch));

        let (at, clock) = resolve_emitted_at(Some(&epoch.to_string()));
        assert_eq!(clock, CLOCK_RUN_EPOCH);
        let s = at.as_str().expect("emitted_at must be a string");
        assert!(s.starts_with("2026-07-25T"), "unexpected instant: {s}");
        assert!(
            !s.starts_with("2026-01-01T00:00:00"),
            "a genuine epoch must not collapse onto the sentinel floor: {s}"
        );
    }

    /// Every "no genuine run epoch" input — unset, blank, unparseable, below
    /// the floor, at/after the window end — records `null` + `unset` rather
    /// than the clock's `2026-01-01T00:00:00Z` sentinel.
    #[test]
    fn absent_or_out_of_window_run_epoch_is_recorded_as_null() {
        let below = (RUN_EPOCH_BASE - 1).to_string();
        let after = RUN_WINDOW_END.to_string();
        let cases: Vec<Option<&str>> = vec![
            None,
            Some(""),
            Some("   "),
            Some("not-an-integer"),
            Some(below.as_str()),
            Some(after.as_str()),
        ];
        for case in cases {
            let (at, clock) = resolve_emitted_at(case);
            assert_eq!(clock, CLOCK_UNSET, "clock marker for {case:?}");
            assert!(
                at.is_null(),
                "emitted_at for {case:?} must be null, got {at}"
            );
        }
    }

    /// The floor value spelled out EXPLICITLY is still a real, in-window run
    /// epoch — only its use as a fallback is a sentinel. Recording it honestly
    /// means an explicit `SOURCE_DATE_EPOCH=<floor>` is not suppressed.
    #[test]
    fn explicit_floor_epoch_is_still_a_real_timestamp() {
        let (at, clock) = resolve_emitted_at(Some(&RUN_EPOCH_BASE.to_string()));
        assert_eq!(clock, CLOCK_RUN_EPOCH);
        assert_eq!(
            at.as_str(),
            Some("2026-01-01T00:00:00+00:00"),
            "an explicitly-set floor epoch is a claim the operator made"
        );
    }
}
