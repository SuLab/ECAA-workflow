//! Regression test for retired Arm B double-prime reduced emission.
//!
//! `ECAA_ECAA_MODE=conventional` used to bypass the full ECAA pipeline and
//! emit a reduced package with no ECAA sidecars. That mode is retired: the
//! env var must not suppress the eight ECAA sidecars.

use ecaa_workflow_conversation::emit::emit_with_conversation_log;
use ecaa_workflow_conversation::session::Session;
use ecaa_workflow_conversation::tools::{dispatch_one, BatchableTool, Tool, ToolContext};
use ecaa_workflow_types::consts::SIDECAR_PATHS;
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
async fn deprecated_ecaa_mode_conventional_does_not_suppress_sidecars() {
    std::env::set_var("ECAA_ECAA_MODE", "conventional");

    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    let emit_result = emit_with_conversation_log(&mut session, dir.path(), &config_dir()).await;

    std::env::remove_var("ECAA_ECAA_MODE");
    emit_result.expect("emit must succeed even when deprecated ECAA_ECAA_MODE is set");

    for (_letter, relpath) in SIDECAR_PATHS {
        assert!(
            dir.path().join(relpath).exists(),
            "{relpath} must exist; ECAA_ECAA_MODE must not suppress sidecars"
        );
    }
}
