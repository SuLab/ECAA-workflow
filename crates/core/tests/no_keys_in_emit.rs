//! CI gate.
//!
//! Asserts that the `scrub_agent_trace_logs` pre-manifest sweep called
//! from `emit_package` removes well-known API-key patterns from every
//! `runtime/outputs/**/agent-trace.log` in the package directory.
//!
//! This is the integration-level guard: the unit tests next to the
//! function check the regex set; this test stages a package-shaped
//! directory tree (with traces written by hand to simulate a prior
//! debug-mode agent run) and verifies the scrubber call site at
//! `provenance_tiers::scrub_agent_trace_logs` clears every known
//! pattern.
//!
//! The plan's original sketch was to drive a full `emit_package`
//! invocation here; the emit harness is too heavy to spin up from
//! scratch without a populated `EmitConfig`, so this test exercises
//! the scrubber directly. The scrubber is what emit_package calls;
//! the wire-up is asserted by the test
//! `scrub_agent_trace_logs_walks_runtime_outputs` inside
//! `provenance_tiers.rs` (which runs as part of crate `cargo test`).

use scripps_workflow_core::provenance_tiers::{scrub_agent_trace_logs, scrub_secrets};

/// All five known patterns from a single trace blob disappear after a
/// single scrub pass.
#[test]
fn no_known_secret_pattern_survives_scrub_secrets() {
    let blob = "\
        + [agent.sh:454] export ANTHROPIC_API_KEY=sk-ant-api03-LEAKED1234567890ABCDEF\n\
        + [agent.sh:455] export HF_TOKEN=hf_LEAKED1234567890ABCDEFG\n\
        + [agent.sh:456] export GITHUB_TOKEN=ghp_LEAKED1234567890ABCDEF\n\
        + [agent.sh:457] export GITHUB_PAT=github_pat_AB_CDEFG12345678901234567890\n\
        + [agent.sh:458] export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\n\
    ";
    let out = scrub_secrets(blob);

    // Negative assertions over the known pattern leaders.
    assert!(
        !out.contains("sk-ant-api03-LEAKED"),
        "Anthropic key leaked through scrubber: {out}"
    );
    assert!(
        !out.contains("hf_LEAKED"),
        "HF token leaked through scrubber: {out}"
    );
    assert!(
        !out.contains("ghp_LEAKED"),
        "GitHub PAT leaked through scrubber: {out}"
    );
    assert!(
        !out.contains("github_pat_AB_"),
        "Fine-grained PAT leaked through scrubber: {out}"
    );
    assert!(
        !out.contains("AKIAIOSFODNN7EXAMPLE"),
        "AWS access key id leaked through scrubber: {out}"
    );

    // Positive assertion: REDACTED appears exactly 5 times (one per
    // pattern). This catches regressions where the regex over-matches
    // (e.g. eating into surrounding context) or under-matches (skipping
    // one of the five families).
    assert_eq!(
        out.matches("REDACTED").count(),
        5,
        "expected one REDACTED per pattern family in: {out}"
    );

    // Surrounding context survives (so trace logs remain readable).
    assert!(out.contains("ANTHROPIC_API_KEY="));
    assert!(out.contains("[agent.sh:454]"));
}

/// Emit-pipeline call-site equivalent: stage a package tree with
/// trace logs at the package root + nested under task dirs and verify
/// the scrubber walks both levels.
#[test]
fn package_runtime_outputs_walk_clears_known_patterns() {
    let pkg = tempfile::tempdir().unwrap();
    let outputs = pkg.path().join("runtime").join("outputs");
    let task_root = outputs.join("discover_qc");
    let task_nested = outputs.join("integrate_layers").join("scripts");
    std::fs::create_dir_all(&task_root).unwrap();
    std::fs::create_dir_all(&task_nested).unwrap();

    std::fs::write(
        task_root.join("agent-trace.log"),
        "+ export ANTHROPIC_API_KEY=sk-ant-api03-PRODKEY1234567890ABCDEF\n\
         + cleanup complete\n",
    )
    .unwrap();
    std::fs::write(
        task_nested.join("agent-trace.log"),
        "HF_TOKEN=hf_LEAKED9876543210ABCDEF retry attempt 2\n",
    )
    .unwrap();

    // Files that aren't named agent-trace.log are NOT scrubbed,
    // matching emit_package's promise of "scrub trace artifacts only".
    let sibling = task_root.join("decision.json");
    std::fs::write(
        &sibling,
        r#"{"reason":"sk-ant-api03-INSIDEJSON1234567890ABCDEF","note":"would NOT be scrubbed"}"#,
    )
    .unwrap();

    let n = scrub_agent_trace_logs(pkg.path()).unwrap();
    assert_eq!(n, 2, "expected exactly 2 trace files scrubbed");

    let scrubbed_a = std::fs::read_to_string(task_root.join("agent-trace.log")).unwrap();
    let scrubbed_b = std::fs::read_to_string(task_nested.join("agent-trace.log")).unwrap();
    let sibling_after = std::fs::read_to_string(&sibling).unwrap();

    assert!(
        !scrubbed_a.contains("sk-ant-api03-PRODKEY"),
        "key survived in {scrubbed_a:?}"
    );
    assert!(scrubbed_a.contains("REDACTED"));
    assert!(scrubbed_a.contains("cleanup complete"));

    assert!(
        !scrubbed_b.contains("hf_LEAKED9876543210ABCDEF"),
        "HF token survived in {scrubbed_b:?}"
    );
    assert!(scrubbed_b.contains("REDACTED"));
    assert!(scrubbed_b.contains("retry attempt 2"));

    // The sibling decision.json is NOT touched — only `agent-trace.log`
    // is in-scope for the pre-manifest scrub. This guards against the
    // regression where a regex sweep accidentally rewrites structured
    // policy / decision artifacts that downstream consumers parse.
    assert!(
        sibling_after.contains("sk-ant-api03-INSIDEJSON"),
        "non-trace file MUST NOT be scrubbed (would corrupt JSON / structured data)"
    );
}

/// Calling the scrubber on a freshly-created package with no
/// `runtime/outputs` returns Ok(0). The emit_package wire-up depends
/// on this to avoid spurious errors on first emit.
#[test]
fn scrub_is_noop_when_runtime_outputs_missing() {
    let pkg = tempfile::tempdir().unwrap();
    let n = scrub_agent_trace_logs(pkg.path()).unwrap();
    assert_eq!(n, 0);
}
