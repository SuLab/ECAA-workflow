//! Integration tests for `provenance_tiers::scrub_agent_trace_logs`.
//!
//! Lives outside the production module per the
//! FunctionalCoreBoundary (C22 / R-7). The walk-runtime-and-rewrite-
//! files tests sit in this integration-test crate
//! (`crates/core/tests/*.rs` is permitted to do I/O) so the
//! production module's `mod tests` stays free of fs-touching
//! scaffolding while the `scrub_agent_trace_logs` invariants stay
//! covered.
//!
//! The production module retains `scrub_secrets` (pure-string redaction)
//! tests inline — those don't touch the filesystem.

use ecaa_workflow_core::provenance_tiers::scrub_agent_trace_logs;

#[test]
fn scrub_agent_trace_logs_walks_runtime_outputs() {
    let tmp = tempfile::tempdir().unwrap();
    let outputs = tmp.path().join("runtime").join("outputs");
    let t1 = outputs.join("task_a");
    let t2 = outputs.join("task_b").join("nested");
    std::fs::create_dir_all(&t1).unwrap();
    std::fs::create_dir_all(&t2).unwrap();
    std::fs::write(
        t1.join("agent-trace.log"),
        "export ANTHROPIC_API_KEY=sk-ant-api03-LEAKED123456789012345\n",
    )
    .unwrap();
    // Nested location also gets scrubbed.
    std::fs::write(
        t2.join("agent-trace.log"),
        "HF_TOKEN=hf_LEAKED1234567890ABCDEF\n",
    )
    .unwrap();
    // Non-trace file is left alone.
    std::fs::write(
        t1.join("other.txt"),
        "sk-ant-api03-NOTATRACE123456789012345",
    )
    .unwrap();
    let n = scrub_agent_trace_logs(tmp.path()).unwrap();
    assert_eq!(n, 2);
    let scrubbed_a = std::fs::read_to_string(t1.join("agent-trace.log")).unwrap();
    let scrubbed_b = std::fs::read_to_string(t2.join("agent-trace.log")).unwrap();
    assert!(!scrubbed_a.contains("sk-ant-api03-LEAKED"));
    assert!(scrubbed_a.contains("REDACTED"));
    assert!(!scrubbed_b.contains("hf_LEAKED"));
    // Non-trace file is left alone.
    let other = std::fs::read_to_string(t1.join("other.txt")).unwrap();
    assert!(other.contains("NOTATRACE"));
}

#[test]
fn scrub_agent_trace_logs_missing_outputs_dir_is_ok() {
    let tmp = tempfile::tempdir().unwrap();
    let n = scrub_agent_trace_logs(tmp.path()).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn scrub_agent_trace_logs_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path().join("runtime").join("outputs").join("task_a");
    std::fs::create_dir_all(&t).unwrap();
    std::fs::write(
        t.join("agent-trace.log"),
        "export ANTHROPIC_API_KEY=sk-ant-api03-LEAKED123456789012345\n",
    )
    .unwrap();
    let n1 = scrub_agent_trace_logs(tmp.path()).unwrap();
    assert_eq!(n1, 1);
    let n2 = scrub_agent_trace_logs(tmp.path()).unwrap();
    // Second pass writes no further changes (file already clean).
    assert_eq!(n2, 0);
}
