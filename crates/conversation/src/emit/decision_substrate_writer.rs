//! v4 P2 / F18 — emit-time writer for `runtime/verifier-decisions.jsonl`.
//!
//! Called from `emit_with_conversation_log_tiered` (next to
//! `write_phase16_sidecars`). Drains the process-wide substrate buffer
//! exposed by `ecaa_workflow_core::decision_substrate` and writes one
//! JSON object per line.
//!
//! Atomicity: write to `<filename>.tmp` then rename so a panic mid-write
//! leaves either no file or the previous file, matching the discipline
//! established by `audit_log::write_jsonl` for the conversation/
//! decision logs.

use ecaa_workflow_core::decision_substrate::{drain, VerifierDecision};
use std::collections::HashSet;
use std::path::Path;

/// Drain the substrate buffer and write one JSON line per **distinct**
/// decision to `<runtime_dir>/verifier-decisions.jsonl`. Returns the
/// number of rows written (post-dedup).
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
///
/// The writer is **synchronous** even though `emit/mod.rs` is async;
/// the substrate file is small (distinct verifier decisions, typically
/// a few thousand rows per emit) and avoiding tokio's File handle keeps
/// the call sync-friendly for tests that exercise the function from
/// `#[cfg(test)]` without an active runtime.
pub(super) fn write_verifier_decisions(runtime_dir: &Path) -> std::io::Result<usize> {
    let decisions = drain();
    let mut buf = String::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut written = 0usize;
    for d in &decisions {
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
        record, IncompatibilityReason as SubstrateIncompatibility, VerifierDecision,
    };
    use std::sync::Mutex;

    /// The decision substrate buffer is process-wide; these unit
    /// tests serialize their (drain, record/write, drain) sequences
    /// so cargo's parallel test runner doesn't cross-contaminate.
    static SUBSTRATE_GUARD: Mutex<()> = Mutex::new(());

    #[test]
    fn writes_and_reads_back_round_trip() {
        let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        // Drain anything left from earlier tests so this test is
        // hermetic on the shared process-wide buffer.
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
}
