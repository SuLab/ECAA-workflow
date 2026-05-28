//! D8 audit-proof sidecar emission tests — verifies that
//! `emit_with_conversation_log` writes `runtime/audit-proof-report.json`
//! with the expected schema_version + 6 verdicts, and that
//! `SWFC_ABLATE_AUDIT_PROOF` suppresses the sidecar entirely.

use scripps_workflow_conversation::emit::emit_with_conversation_log;
use scripps_workflow_conversation::session::Session;
use scripps_workflow_conversation::tools::{dispatch_one, BatchableTool, Tool, ToolContext};
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
async fn emit_writes_audit_proof_sidecar() {
    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .unwrap();

    let sidecar = dir.path().join("runtime/audit-proof-report.json");
    assert!(
        sidecar.exists(),
        "audit-proof-report.json should be emitted"
    );
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert_eq!(
        body.get("schema_version").and_then(|v| v.as_str()),
        Some("0.1")
    );
    let verdicts = body.get("verdicts").and_then(|v| v.as_array()).unwrap();
    assert_eq!(verdicts.len(), 6);
}

#[tokio::test]
#[serial]
async fn audit_proof_suppressed_when_ablate_audit_proof_set() {
    std::env::set_var("SWFC_ABLATE_AUDIT_PROOF", "1");
    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .unwrap();
    let sidecar = dir.path().join("runtime/audit-proof-report.json");
    let exists = sidecar.exists();
    std::env::remove_var("SWFC_ABLATE_AUDIT_PROOF");
    assert!(
        !exists,
        "audit-proof-report.json should NOT be emitted under ablation"
    );
}
