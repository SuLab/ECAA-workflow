//! Tests for `crates/core/src/agent_code.rs`.
//!
//! Covers:
//!  1. JSON round-trip: serialize then deserialize produces identical values.
//!  2. ts-rs binding presence: `ui/src/types/AgentCodeRecord.ts` must exist
//!     after `make types` (or `TS_RS_EXPORT_DIR=… cargo test export_bindings`).

use scripps_workflow_core::agent_code::AgentCodeRecord;

// ── 1. JSON round-trip ───────────────────────────────────────────────────────

#[test]
fn agent_code_record_json_round_trip() {
    // Use AgentCodeRecord::new + field mutation to avoid #[non_exhaustive]
    // struct-literal restriction from outside the crate.
    let mut rec = AgentCodeRecord::new(
        "Align reads with STAR".to_string(),
        "2026-05-22T10:00:00Z".to_string(),
        "2026-05-22T10:05:30Z".to_string(),
    );
    rec.executed_code = "#!/usr/bin/env bash\nSTAR --runMode alignReads".to_string();
    rec.language = "Bash".to_string();

    let json = serde_json::to_string(&rec).expect("serialize must succeed");
    let deserialized: AgentCodeRecord =
        serde_json::from_str(&json).expect("deserialize must succeed");

    assert_eq!(deserialized.prompt, rec.prompt);
    assert_eq!(deserialized.response_text, rec.response_text);
    assert_eq!(deserialized.executed_code, rec.executed_code);
    assert_eq!(deserialized.language, rec.language);
    assert_eq!(deserialized.started_at, rec.started_at);
    assert_eq!(deserialized.completed_at, rec.completed_at);
}

#[test]
fn agent_code_record_minimal_constructor() {
    let rec = AgentCodeRecord::new(
        "hello".to_string(),
        "2026-05-22T09:00:00Z".to_string(),
        "2026-05-22T09:03:00Z".to_string(),
    );
    assert_eq!(rec.prompt, "hello");
    assert_eq!(rec.response_text, "");
    assert_eq!(rec.executed_code, "");
    assert_eq!(rec.language, "unknown");
    assert_eq!(rec.started_at, "2026-05-22T09:00:00Z");
    assert_eq!(rec.completed_at, "2026-05-22T09:03:00Z");
}

#[test]
fn agent_code_record_json_fields_present() {
    let rec = AgentCodeRecord::new(
        "test prompt".to_string(),
        "2026-05-22T08:00:00Z".to_string(),
        "2026-05-22T08:01:00Z".to_string(),
    );
    let val: serde_json::Value = serde_json::to_value(&rec).unwrap();
    assert!(val.get("prompt").is_some(), "JSON must have 'prompt'");
    assert!(
        val.get("response_text").is_some(),
        "JSON must have 'response_text'"
    );
    assert!(
        val.get("executed_code").is_some(),
        "JSON must have 'executed_code'"
    );
    assert!(val.get("language").is_some(), "JSON must have 'language'");
    assert!(
        val.get("started_at").is_some(),
        "JSON must have 'started_at'"
    );
    assert!(
        val.get("completed_at").is_some(),
        "JSON must have 'completed_at'"
    );
}

// ── 2. ts-rs binding presence ────────────────────────────────────────────────

/// After `make types` (i.e. `TS_RS_EXPORT_DIR=… cargo test export_bindings`),
/// the file `ui/src/types/AgentCodeRecord.ts` must exist.
/// This test is advisory when TS_RS_EXPORT_DIR is unset (e.g. plain `cargo test`)
/// but is load-bearing under `make types`.
#[test]
fn agent_code_record_ts_binding_present() {
    let export_dir = match std::env::var("TS_RS_EXPORT_DIR") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => {
            // Not running under `make types` — skip gracefully.
            return;
        }
    };
    let binding_path = export_dir.join("AgentCodeRecord.ts");
    assert!(
        binding_path.exists(),
        "ts-rs binding missing: {binding_path:?} — run `make types` to regenerate"
    );
}
