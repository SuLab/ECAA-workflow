//! Grant v19 §Authentication of Key Resources — integration tests for
//! the four `runtime/*.json` sidecars (D1-D4): claim-verification,
//! determinism-shim, security-policy, model-policy.
//!
//! Each test boots a `Session` via the appropriate `test_fixture_*`
//! helper, drives the classifier to populate a DAG (required by core's
//! `emit_package`), runs the public emit entrypoint, and asserts the
//! sidecar exists with a minimal-but-valid schema.

use ecaa_workflow_conversation::emit::emit_with_conversation_log;
use ecaa_workflow_conversation::session::Session;
use ecaa_workflow_conversation::tools::{dispatch_one, BatchableTool, Tool, ToolContext};
use std::path::PathBuf;
use tempfile::tempdir;

fn config_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

/// Build a session and run the classifier-driven DAG construction.
/// Mirrors the in-crate `emit_writes_conversation_log_and_patches_metadata`
/// pattern: `Session::test_fixture_*` returns a barebones session;
/// `AppendIntakeProse` populates the DAG via the classifier path so
/// `emit_package` has something to lower.
async fn boot_session_with_dag() -> Session {
    let mut session = Session::test_fixture_with_dag();
    let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq from human IVD samples comparing degenerated and healthy"
                .into(),
        }),
        &mut session,
        &ctx,
    )
    .await;
    session
}

#[tokio::test]
async fn claim_verification_sidecar_emitted_when_claims_present() {
    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .unwrap();

    let sidecar = dir.path().join("runtime/claim-verification.json");
    assert!(
        sidecar.exists(),
        "runtime/claim-verification.json must exist after emit"
    );

    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert!(
        body.get("verdicts").is_some() || body.get("claims").is_some(),
        "claim-verification.json must have a verdicts or claims field; got {body}"
    );
}

#[tokio::test]
async fn determinism_shim_sidecar_always_emitted() {
    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .unwrap();

    let sidecar = dir.path().join("runtime/determinism-shim.json");
    assert!(sidecar.exists(), "runtime/determinism-shim.json missing");

    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert_eq!(body["schema_version"], "1");
    assert!(body.get("env_capture").is_some(), "missing env_capture");
    assert!(body.get("seed_policy").is_some(), "missing seed_policy");
    assert!(
        body.get("temp_path_policy").is_some(),
        "missing temp_path_policy"
    );
    // ablation_engaged is the ECAA_ABLATE_REEXECUTION_CLASS mirror; on
    // an un-ablated test run it must be false. We do not unset the
    // env var here because the test would race with parallel tests
    // that may set it (the field is informational, not gated).
    assert!(
        body.get("ablation_engaged").is_some(),
        "missing ablation_engaged"
    );
}

#[tokio::test]
async fn security_policy_sidecar_emitted() {
    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .unwrap();

    let sidecar = dir.path().join("runtime/security-policy.json");
    assert!(sidecar.exists(), "runtime/security-policy.json missing");

    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert_eq!(body["schema_version"], "1");
    assert!(body.get("atom_policies").is_some(), "missing atom_policies");
    assert!(
        body.get("package_max_safety_level").is_some(),
        "missing package_max_safety_level"
    );
    assert!(
        body.get("package_max_network_policy").is_some(),
        "missing package_max_network_policy"
    );
    assert!(
        body.get("container_image_digests").is_some(),
        "missing container_image_digests"
    );
}

#[tokio::test]
async fn model_policy_sidecar_emitted_with_prompt_hash() {
    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .unwrap();

    let sidecar = dir.path().join("runtime/model-policy.json");
    assert!(sidecar.exists(), "runtime/model-policy.json missing");

    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert_eq!(body["schema_version"], "1");
    // active_model_id is the Debug-formatted ModelId variant; one of
    // Sonnet46 / Opus48 / Opus46 / Haiku45.
    let model_id = body["active_model_id"]
        .as_str()
        .expect("active_model_id must be a string");
    assert!(
        model_id.starts_with("Sonnet")
            || model_id.starts_with("Opus")
            || model_id.starts_with("Haiku"),
        "unexpected model id: {model_id}"
    );
    let hash = body["system_prompt_sha256"]
        .as_str()
        .expect("system_prompt_sha256 must be a string");
    assert_eq!(hash.len(), 64, "SHA-256 hex must be 64 chars");
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "SHA-256 hex must be all hex chars"
    );
    assert!(body["tool_count"].as_u64().unwrap() >= 19, "tool_count");
    assert_eq!(body["provider_id"], "anthropic");
}

/// Uniform 8-sidecar presence (B6): `runtime/proofs.jsonl` and
/// `runtime/assumptions.jsonl` must be present on disk after every
/// emit, even when the v4 planner produced no compatibility proofs and
/// no assumptions. A 0-byte file is acceptable; absence is not.
/// Downstream audit-proof loaders treat a present-empty file as
/// "no rows" and an absent file as a substrate gap — so the file must
/// always exist.
///
/// Exercises the planner-output-empty case directly by clearing the
/// cached v4 DAG's `edges` (the conversation-crate proofs writer
/// projects one row per edge) and `assumptions` (one row per ledger
/// entry) before emit, so `write_phase16_sidecars` receives an empty
/// string for both. Without the present-but-empty fix the conversation
/// writer skips the write under those conditions.
#[tokio::test]
async fn proofs_and_assumptions_sidecars_present_even_when_empty() {
    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;

    // Force the planner-output-empty case in the conversation writer.
    {
        let dag = session
            .workflow_dag
            .as_mut()
            .expect("classifier path must populate workflow_dag");
        dag.edges.clear();
        dag.assumptions.entries.clear();
    }

    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .unwrap();

    let proofs = dir.path().join("runtime/proofs.jsonl");
    let assumptions = dir.path().join("runtime/assumptions.jsonl");
    assert!(
        proofs.exists(),
        "runtime/proofs.jsonl must exist after emit (present-but-empty allowed)"
    );
    assert!(
        assumptions.exists(),
        "runtime/assumptions.jsonl must exist after emit (present-but-empty allowed)"
    );
    // assumptions.jsonl has no other producer in this scenario, so an
    // empty ledger yields a 0-byte file.
    assert_eq!(
        std::fs::read(&assumptions).unwrap().len(),
        0,
        "assumptions.jsonl must be 0 bytes when no assumptions produced"
    );
}

/// Byte-determinism for the present-but-empty sidecars: re-emitting the
/// SAME session into two directories yields byte-identical
/// `proofs.jsonl`, `assumptions.jsonl`, and `ro-crate-metadata.json`.
/// (Two independently-booted sessions would differ only by their random
/// session UUID, which the byte-characterization baseline excludes; this
/// test instead exercises idempotent re-emission of one session.)
#[tokio::test]
async fn proofs_and_assumptions_sidecars_byte_deterministic() {
    let dir_a = tempdir().unwrap();
    let dir_b = tempdir().unwrap();

    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir_a.path(), &config_dir())
        .await
        .unwrap();
    emit_with_conversation_log(&mut session, dir_b.path(), &config_dir())
        .await
        .unwrap();

    for rel in [
        "runtime/proofs.jsonl",
        "runtime/assumptions.jsonl",
        "ro-crate-metadata.json",
    ] {
        let a = std::fs::read(dir_a.path().join(rel)).unwrap();
        let b = std::fs::read(dir_b.path().join(rel)).unwrap();
        assert_eq!(a, b, "{rel} must be byte-identical across two emits");
    }
}
