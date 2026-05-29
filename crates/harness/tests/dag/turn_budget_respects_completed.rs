//! Regression test for the turn-budget enforcement contract.
//!
//! The `enforce_turn_budget_limit` bash helper in
//! `scripts/agent-claude-common.sh` MUST NOT overwrite
//! `state.patch.json` + `result.json` with a blocked-by-turn-budget
//! patch when the agent has finished the task and self-reported
//! `completed`. Unconditional overwriting would silently lose real
//! results when a task produced valid artifacts in the final turns
//! of its budget.
//!
//! The helper checks for an existing `state.patch.json` with
//! `to.status == "completed"` before overwriting. This test
//! exercises both branches via the actual shell script — no
//! Rust-side reimplementation of the budget logic.
//!
//! Skipped when `bash` or `jq` is missing; both are required by the
//! shell helper itself, so any host that can run the harness can run
//! the test.

use std::fs;
use std::process::Command;
use tempfile::TempDir;

fn bash_and_jq_available() -> bool {
    Command::new("bash")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        && Command::new("jq")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
}

fn workspace_root() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR points at crates/harness/. The workspace root
    // is two levels up.
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root must be two levels above CARGO_MANIFEST_DIR")
        .to_path_buf()
}

/// Source the shell helper and invoke `enforce_turn_budget_limit` with
/// the given args. Returns (stdout, stderr, exit_status).
fn run_enforce(pkg: &std::path::Path, task_id: &str, cap: u32) -> (String, String, i32) {
    let script = workspace_root().join("scripts/agent-claude-common.sh");
    let cmd = format!(
        "set -e; source '{script}'; enforce_turn_budget_limit '{pkg}' '{task_id}' '{cap}'",
        script = script.display(),
        pkg = pkg.display(),
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&cmd)
        .output()
        .expect("bash -c must run");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn task_dir(pkg: &std::path::Path, task_id: &str) -> std::path::PathBuf {
    let d = pkg.join("runtime/outputs").join(task_id);
    fs::create_dir_all(&d).expect("create task dir");
    d
}

fn write_usage(task_dir: &std::path::Path, num_turns: u32) {
    fs::write(
        task_dir.join("agent-usage.json"),
        format!(r#"{{"num_turns": {num_turns}}}"#),
    )
    .expect("write usage");
}

#[test]
fn agent_self_reported_completed_is_preserved_above_cap() {
    if !bash_and_jq_available() {
        eprintln!("skipping: bash or jq not on PATH");
        return;
    }

    let tmp = TempDir::new().expect("tempdir");
    let dir = task_dir(tmp.path(), "normalisation");

    // Agent completed the work in 50 turns; the cap is 40.
    write_usage(&dir, 50);
    let agent_patch = r#"{"from":"running","to":{"status":"completed","result":{"k":"v"}}}"#;
    fs::write(dir.join("state.patch.json"), agent_patch).unwrap();

    let (_stdout, stderr, code) = run_enforce(tmp.path(), "normalisation", 40);
    assert_eq!(code, 0, "helper should exit 0 when respecting agent state");

    let on_disk = fs::read_to_string(dir.join("state.patch.json")).unwrap();
    assert_eq!(
        on_disk.trim(),
        agent_patch,
        "state.patch.json must be preserved when agent self-reports completed",
    );
    assert!(
        stderr.contains("self-reported completed"),
        "stderr should explain why the budget was respected; got: {stderr}",
    );
}

#[test]
fn agent_silent_above_cap_is_blocked() {
    if !bash_and_jq_available() {
        eprintln!("skipping: bash or jq not on PATH");
        return;
    }

    let tmp = TempDir::new().expect("tempdir");
    let dir = task_dir(tmp.path(), "differential_expression");

    // Agent didn't write a state.patch.json — exceeded the cap silently.
    write_usage(&dir, 60);

    let (_stdout, stderr, code) = run_enforce(tmp.path(), "differential_expression", 40);
    assert_eq!(code, 0, "helper should exit 0 even when it writes a block");

    // The helper should have written both result.json and state.patch.json
    // marking the task blocked.
    let result_raw = fs::read_to_string(dir.join("result.json")).expect("result.json written");
    let result: serde_json::Value = serde_json::from_str(&result_raw).expect("valid json");
    assert_eq!(
        result.get("status").and_then(|v| v.as_str()),
        Some("blocked")
    );
    assert_eq!(
        result.get("blocker_kind").and_then(|v| v.as_str()),
        Some("TurnBudgetExceeded"),
    );

    let patch_raw = fs::read_to_string(dir.join("state.patch.json")).expect("patch written");
    let patch: serde_json::Value = serde_json::from_str(&patch_raw).expect("valid json");
    assert_eq!(
        patch.pointer("/to/status").and_then(|v| v.as_str()),
        Some("blocked"),
    );
    assert!(
        stderr.contains("exceeded cap"),
        "stderr should explain that the cap was hit; got: {stderr}",
    );
}

#[test]
fn within_cap_is_noop() {
    if !bash_and_jq_available() {
        eprintln!("skipping: bash or jq not on PATH");
        return;
    }

    let tmp = TempDir::new().expect("tempdir");
    let dir = task_dir(tmp.path(), "qc_preprocessing");

    write_usage(&dir, 25);
    let agent_patch = r#"{"from":"running","to":{"status":"completed","result":{}}}"#;
    fs::write(dir.join("state.patch.json"), agent_patch).unwrap();

    let (_stdout, stderr, code) = run_enforce(tmp.path(), "qc_preprocessing", 40);
    assert_eq!(code, 0);
    assert_eq!(
        fs::read_to_string(dir.join("state.patch.json"))
            .unwrap()
            .trim(),
        agent_patch,
        "state.patch.json must be untouched below the cap",
    );
    assert!(
        !stderr.contains("exceeded cap") && !stderr.contains("self-reported completed"),
        "stderr should be empty under the cap; got: {stderr}",
    );
}

#[test]
fn missing_usage_file_is_noop() {
    if !bash_and_jq_available() {
        eprintln!("skipping: bash or jq not on PATH");
        return;
    }

    let tmp = TempDir::new().expect("tempdir");
    let dir = task_dir(tmp.path(), "data_acquisition");
    // No agent-usage.json — early return.
    let (_stdout, _stderr, code) = run_enforce(tmp.path(), "data_acquisition", 40);
    assert_eq!(code, 0);
    assert!(
        !dir.join("result.json").exists(),
        "no result.json should be written when usage file is missing",
    );
    assert!(
        !dir.join("state.patch.json").exists(),
        "no state.patch.json should be written when usage file is missing",
    );
}
