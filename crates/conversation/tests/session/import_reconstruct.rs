//! Reconstruct a read-only, imported `Session` from an extracted package
//! directory and assert the load-path invariants: `imported = true`, state
//! forced to `Emitted`, DAG + task states rebuilt from `WORKFLOW.json`, the
//! transcript recovered from `intake-conversation.jsonl` (dropping non-`Turn`
//! lines), and a fresh (non-zero) `audit_writer_secret`.

use ecaa_workflow_conversation::Session;
use ecaa_workflow_conversation::SessionState;
use std::fs;

fn write_pkg(root: &std::path::Path) {
    fs::create_dir_all(root.join("runtime")).unwrap();
    let dag = serde_json::json!({
        "version": "1", "workflow_id": "wf_test", "current_task": null,
        "tasks": {
            "data_acq": {"kind": {"discovery":"source"}, "state": {"status":"completed","result":{}}, "depends_on": [], "assignee":"agent", "description":"acquire"}
        },
        "execution_order": ["data_acq"]
    });
    fs::write(
        root.join("WORKFLOW.json"),
        serde_json::to_vec_pretty(&dag).unwrap(),
    )
    .unwrap();
    fs::write(root.join("ro-crate-metadata.json"), b"{}").unwrap();
    fs::write(root.join("runtime/proofs.jsonl"), b"").unwrap();
    fs::write(root.join("runtime/assumptions.jsonl"), b"").unwrap();
    // The transcript file is N Turn lines followed by M ToolCallRecord lines.
    // Only the Turn line should survive reconstruction.
    let turn = serde_json::json!({
        "turn_id": "00000000-0000-0000-0000-000000000001",
        "role": "user", "content": "hello", "timestamp": "2026-07-08T00:00:00Z"
    });
    // A ToolCallRecord-shaped line (no role/content) that must be dropped.
    let tool_call = serde_json::json!({
        "turn_id": "00000000-0000-0000-0000-000000000001",
        "tool_name": "classify_intake", "args": {}, "result": {},
        "is_error": false, "model": "mock", "timestamp": "2026-07-08T00:00:00Z"
    });
    fs::write(
        root.join("runtime/intake-conversation.jsonl"),
        format!("{turn}\n{tool_call}\n"),
    )
    .unwrap();
    // A decisions.jsonl line with a shape the reader tolerates dropping if it
    // doesn't match the real DecisionRecord wire format.
    let decision = serde_json::json!({
        "timestamp": "2026-07-08T00:00:00Z", "session_id": "s",
        "decision": {"kind": "package_emitted"}, "actor": "system"
    });
    fs::write(
        root.join("runtime/decisions.jsonl"),
        format!("{decision}\n"),
    )
    .unwrap();
}

#[test]
fn reconstructs_read_only_emitted_session() {
    let dir = tempfile::tempdir().unwrap();
    write_pkg(dir.path());
    let s = Session::from_imported_package(dir.path()).unwrap();

    assert!(s.imported, "imported session flagged read-only");
    assert_eq!(s.state, SessionState::Emitted, "load forces Emitted state");
    assert_eq!(s.emitted_package_path.as_deref(), Some(dir.path()));
    assert!(s.workflow_dag.is_some(), "DAG reconstructed");
    assert!(
        s.task_states.contains_key("data_acq"),
        "task state map rebuilt from WORKFLOW.json"
    );
    assert_eq!(
        s.conversation.len(),
        1,
        "one Turn line reconstructed; the ToolCallRecord line is dropped"
    );
    // Fresh secret (not the origin's, which never leaves its process) — non-zero.
    assert_ne!(s.audit_writer_secret, [0u8; 32]);
}

#[test]
fn missing_workflow_json_errors() {
    let dir = tempfile::tempdir().unwrap();
    // No WORKFLOW.json written — reconstruction must surface an error rather
    // than fabricate an empty session.
    let err = Session::from_imported_package(dir.path()).unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("reconstruct")
            || format!("{err:#}").to_lowercase().contains("workflow.json"),
        "error should mention the failed reconstruction: {err:#}"
    );
}
