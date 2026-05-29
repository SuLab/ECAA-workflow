//! Integration tests for Arm B″ (`ECAA_ECAA_MODE=conventional`) emit.
//!
//! Boots a session through the same `Session::test_fixture_with_dag` +
//! `AppendIntakeProse` path that `sidecar_emission.rs` uses, then asserts
//! the conventional envelope is present and ECAA-specific sidecars are
//! absent (and vice-versa for the default `Full` mode).
//!
//! These tests mutate process-global env via `set_var` /
//! `remove_var`, so they're serialized via `serial_test::serial` to
//! avoid colliding with sibling tests in this crate.

use ecaa_workflow_conversation::emit::emit_with_conversation_log;
use ecaa_workflow_conversation::session::Session;
use ecaa_workflow_conversation::tools::{dispatch_one, BatchableTool, Tool, ToolContext};
use serial_test::serial;
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
#[serial]
async fn conventional_mode_emits_readme_and_ipynb_no_sidecars() {
    // Process-global env mutation: serial gate above prevents races with
    // the `Full`-mode sibling test below.
    std::env::set_var("ECAA_ECAA_MODE", "conventional");

    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    let emit_result = emit_with_conversation_log(&mut session, dir.path(), &config_dir()).await;
    std::env::remove_var("ECAA_ECAA_MODE");
    emit_result.expect("conventional emit must succeed");

    let pkg = dir.path();
    assert!(pkg.join("README.md").exists(), "README.md missing");
    assert!(
        pkg.join("analysis.ipynb").exists(),
        "analysis.ipynb missing"
    );
    assert!(
        pkg.join("ro-crate-metadata.json").exists(),
        "ro-crate-metadata.json missing"
    );
    // ECAA-specific sidecars MUST be absent.
    assert!(
        !pkg.join("runtime").join("audit-proof-report.json").exists(),
        "runtime/audit-proof-report.json must be absent in Conventional mode"
    );
    assert!(
        !pkg.join("runtime").join("decisions.jsonl").exists(),
        "runtime/decisions.jsonl must be absent in Conventional mode"
    );
    assert!(
        !pkg.join("runtime").join("claim-verification.json").exists(),
        "runtime/claim-verification.json must be absent in Conventional mode"
    );
}

#[tokio::test]
#[serial]
async fn full_mode_emits_decisions_log_as_before() {
    // Sanity check: with ECAA_ECAA_MODE unset, the existing emit
    // pipeline runs and at least the decisions log lands.
    std::env::remove_var("ECAA_ECAA_MODE");

    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .expect("full emit must succeed");

    assert!(
        dir.path().join("runtime").join("decisions.jsonl").exists(),
        "runtime/decisions.jsonl must exist in Full mode"
    );
}
