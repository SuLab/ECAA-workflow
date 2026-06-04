//! v4 P2 / F18 — emit-time writer for `runtime/verifier-decisions.jsonl`.
//!
//! Called from `emit_with_conversation_log_tiered` (next to
//! `write_phase16_sidecars`). Drains the session-keyed substrate buffer
//! exposed by `ecaa_workflow_core::decision_substrate` and writes one
//! JSON object per line.
//!
//! Session isolation: prefer [`write_verifier_decisions_for_session`],
//! which drains only the emitting session's bucket via
//! `decision_substrate::drain_session` — so a sibling session's
//! still-buffered compose-time decisions never leak into this session's
//! sidecar. [`write_verifier_decisions`] is the legacy entry point that
//! drains the unscoped default bucket (and, when no session scope is
//! active on the calling thread, every bucket merged) — retained for the
//! existing `emit/mod.rs` call site and for tests that record into the
//! default bucket.
//!
//! Atomicity: write to `<filename>.tmp` then rename so a panic mid-write
//! leaves either no file or the previous file, matching the discipline
//! established by `audit_log::write_jsonl` for the conversation/
//! decision logs.

use ecaa_workflow_core::decision_substrate::{drain, drain_session, VerifierDecision};
use std::collections::HashSet;
use std::path::Path;

/// Dedup + atomically write a batch of substrate decisions to
/// `<runtime_dir>/verifier-decisions.jsonl`. Returns the number of rows
/// written (post-dedup). Shared by both the legacy and session-isolated
/// entry points so the dedup, ordering, and atomic-rename discipline
/// stay identical.
///
/// Dedup rationale: the v4 proof-carrying planner runs forward/backward
/// search that re-visits the same producer→consumer port pairs many
/// times, and the compatibility engine records an identical
/// `UnificationAttempted`/`UnificationFailed` row on every revisit. A
/// trivial 28-task package was observed emitting ~98k rows that collapse
/// to ~4.5k distinct ones (≈22× duplication). The substrate is
/// observational — each distinct decision carries information exactly
/// once — so collapsing byte-identical serialized rows is lossless for
/// every downstream consumer (audit-proof invariants, the verifier UI
/// table, the RDF projection) while keeping the sidecar small enough
/// that the ECAA SHACL-projection validator completes inside its
/// subprocess timeout. First-seen insertion order is preserved so the
/// file stays byte-deterministic across re-emissions.
fn write_decisions_to_dir(
    runtime_dir: &Path,
    decisions: &[VerifierDecision],
) -> std::io::Result<usize> {
    let mut buf = String::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut written = 0usize;
    for d in decisions {
        match serde_json::to_string(d) {
            Ok(line) => {
                // Collapse byte-identical rows the planner re-recorded on
                // search revisits; first occurrence wins so order is stable.
                if seen.insert(line.clone()) {
                    buf.push_str(&line);
                    buf.push('\n');
                    written += 1;
                }
            }
            Err(e) => {
                // Substrate is observational; a single un-serializable
                // row should not abort the emit. Log to stderr (no
                // tracing dep at this call site) and skip the row.
                eprintln!(
                    "warn: verifier-decisions: failed to serialize event ({}), skipping",
                    e
                );
            }
        }
    }
    let target = runtime_dir.join("verifier-decisions.jsonl");
    let tmp = target.with_extension("jsonl.tmp");
    std::fs::create_dir_all(runtime_dir)?;
    std::fs::write(&tmp, buf)?;
    std::fs::rename(&tmp, &target)?;
    Ok(written)
}

/// Drain the unscoped/default substrate bucket (or, with no active
/// session scope on the calling thread, every bucket merged) and write
/// one JSON line per **distinct** decision to
/// `<runtime_dir>/verifier-decisions.jsonl`. Returns the number of rows
/// written (post-dedup).
///
/// Prefer [`write_verifier_decisions_for_session`] on the emit path: this
/// legacy entry point cannot isolate two sessions that both finished
/// composing but have not yet emitted, because it has no session id to
/// key on. It is retained for the current `emit/mod.rs` call site and for
/// `#[cfg(test)]` callers that record into the default bucket.
///
/// The writer is **synchronous** even though `emit/mod.rs` is async;
/// the substrate file is small (distinct verifier decisions, typically
/// a few thousand rows per emit) and avoiding tokio's File handle keeps
/// the call sync-friendly for tests that exercise the function from
/// `#[cfg(test)]` without an active runtime.
pub(super) fn write_verifier_decisions(runtime_dir: &Path) -> std::io::Result<usize> {
    let decisions = drain();
    write_decisions_to_dir(runtime_dir, &decisions)
}

/// Session-isolated counterpart to [`write_verifier_decisions`]. Drains
/// **only** the `session_id` bucket — so even when a sibling session's
/// compose-time rows are still buffered in the process, this session's
/// `verifier-decisions.jsonl` carries exactly its own decisions. This is
/// the cross-session-contamination fix: the composer enters a matching
/// `decision_substrate::enter_session(session_id)` scope around its
/// `plan()` call, so every row this drains was recorded under the same
/// id.
///
/// Synchronous for the same reason as [`write_verifier_decisions`].
#[allow(dead_code)]
pub(super) fn write_verifier_decisions_for_session(
    runtime_dir: &Path,
    session_id: &str,
) -> std::io::Result<usize> {
    let decisions = drain_session(session_id);
    write_decisions_to_dir(runtime_dir, &decisions)
}

/// Read the substrate file back into a `Vec<VerifierDecision>`. Used
/// by the server's `GET /api/chat/session/:id/verifier-decisions`
/// route and by integration tests asserting round-trip equality.
///
/// Returns an empty Vec when the file is absent (a v1/v2/v3 emit, or a
/// v4 emit that ran no `prove()` calls). Malformed lines are skipped
/// with a stderr warning so a partial file remains queryable.
pub fn read_verifier_decisions(runtime_dir: &Path) -> std::io::Result<Vec<VerifierDecision>> {
    let target = runtime_dir.join("verifier-decisions.jsonl");
    if !target.exists() {
        return Ok(Vec::new());
    }
    let bytes = std::fs::read(&target)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut out: Vec<VerifierDecision> = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<VerifierDecision>(trimmed) {
            Ok(d) => out.push(d),
            Err(e) => {
                eprintln!(
                    "warn: verifier-decisions: skipping malformed line {} ({})",
                    i + 1,
                    e
                );
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::decision_substrate::{
        enter_session, record, IncompatibilityReason as SubstrateIncompatibility, VerifierDecision,
    };
    use std::sync::Mutex;

    /// The decision substrate buffer is session-keyed; tests that
    /// record/drain the *unscoped default* bucket serialize their
    /// (drain, record/write, drain) sequences through this guard so
    /// cargo's parallel test runner doesn't cross-contaminate. Tests
    /// that scope into a unique session id are isolated by the key and
    /// additionally hold this guard so a concurrent unscoped merge-all
    /// drain can't steal their scoped rows.
    static SUBSTRATE_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn writes_and_reads_back_round_trip() {
        let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        // Drain anything left from earlier tests so this test is
        // hermetic on the shared default substrate bucket (unscoped
        // `drain()` merges all buckets).
        let _ = ecaa_workflow_core::decision_substrate::drain();
        record(VerifierDecision::UnificationAttempted {
            id: "u1".into(),
            timestamp: "0".into(),
            producer_port: "p".into(),
            consumer_port: "c".into(),
            ctx_hash: "h".into(),
        });
        record(VerifierDecision::UnificationFailed {
            id: "u1-fail".into(),
            timestamp: "0".into(),
            producer_port: "p".into(),
            consumer_port: "c".into(),
            reason: SubstrateIncompatibility::Other {
                statement: "test".into(),
            },
        });
        let dir = tempfile::tempdir().unwrap();
        let n = write_verifier_decisions(dir.path()).unwrap();
        assert_eq!(n, 2);
        let read_back = read_verifier_decisions(dir.path()).unwrap();
        assert_eq!(read_back.len(), 2);
        match &read_back[0] {
            VerifierDecision::UnificationAttempted { id, .. } => assert_eq!(id, "u1"),
            other => panic!("expected UnificationAttempted, got {:?}", other),
        }
    }

    #[test]
    fn collapses_duplicate_rows_the_planner_re_recorded() {
        let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let _ = ecaa_workflow_core::decision_substrate::drain();
        // The v4 planner re-records identical unification attempts on every
        // search revisit; emit exactly one of each distinct row.
        let dup = || VerifierDecision::UnificationAttempted {
            id: "unif:data:2531:data:2044".into(),
            timestamp: "0".into(),
            producer_port: "data:2531".into(),
            consumer_port: "data:2044".into(),
            ctx_hash: "::Draft".into(),
        };
        for _ in 0..50 {
            record(dup());
        }
        record(VerifierDecision::UnificationFailed {
            id: "unif:data:9:data:9".into(),
            timestamp: "0".into(),
            producer_port: "data:9".into(),
            consumer_port: "data:9".into(),
            reason: SubstrateIncompatibility::Other {
                statement: "distinct".into(),
            },
        });
        let dir = tempfile::tempdir().unwrap();
        let n = write_verifier_decisions(dir.path()).unwrap();
        assert_eq!(n, 2, "50 identical attempts + 1 distinct failure → 2 rows");
        let read_back = read_verifier_decisions(dir.path()).unwrap();
        assert_eq!(read_back.len(), 2);
    }

    #[test]
    fn absent_file_returns_empty_vec() {
        let dir = tempfile::tempdir().unwrap();
        let v = read_verifier_decisions(dir.path()).unwrap();
        assert!(v.is_empty());
    }

    #[test]
    fn empty_buffer_writes_zero_byte_file() {
        let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let _ = ecaa_workflow_core::decision_substrate::drain();
        let dir = tempfile::tempdir().unwrap();
        let n = write_verifier_decisions(dir.path()).unwrap();
        assert_eq!(n, 0);
        let p = dir.path().join("verifier-decisions.jsonl");
        assert!(p.exists());
        let bytes = std::fs::read(&p).unwrap();
        assert!(bytes.is_empty());
    }

    /// The session-isolated writer drains only the emitting session's
    /// bucket: a sibling session that finished composing (its rows still
    /// buffered) never contaminates this session's sidecar. This mirrors
    /// the production flow where the composer enters
    /// `enter_session(session_id)` around `plan()` and the emit step
    /// drains that same session.
    #[test]
    fn session_writer_isolates_concurrent_sessions() {
        let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let tag = std::ptr::addr_of!(SUBSTRATE_GUARD) as usize;
        let sess_emitting = format!("emit-{tag}");
        let sess_other = format!("other-{tag}");

        // Session "other" composed first and its rows are still buffered.
        {
            let _scope = enter_session(sess_other.clone());
            for i in 0..3 {
                record(VerifierDecision::UnificationAttempted {
                    id: format!("other-{i}"),
                    timestamp: "0".into(),
                    producer_port: "po".into(),
                    consumer_port: "co".into(),
                    ctx_hash: "ho".into(),
                });
            }
        }
        // Session being emitted recorded its own rows.
        {
            let _scope = enter_session(sess_emitting.clone());
            record(VerifierDecision::UnificationAttempted {
                id: "mine-0".into(),
                timestamp: "0".into(),
                producer_port: "pm".into(),
                consumer_port: "cm".into(),
                ctx_hash: "hm".into(),
            });
        }

        let dir = tempfile::tempdir().unwrap();
        let n = write_verifier_decisions_for_session(dir.path(), &sess_emitting).unwrap();
        assert_eq!(n, 1, "writer drained only the emitting session's row");
        let read_back = read_verifier_decisions(dir.path()).unwrap();
        assert_eq!(read_back.len(), 1);
        assert!(matches!(
            &read_back[0],
            VerifierDecision::UnificationAttempted { id, .. } if id == "mine-0"
        ));

        // "other" survives untouched and can be drained separately.
        let other_dir = tempfile::tempdir().unwrap();
        let other_n = write_verifier_decisions_for_session(other_dir.path(), &sess_other).unwrap();
        assert_eq!(other_n, 3, "sibling session's buffered rows are intact");
    }
}
