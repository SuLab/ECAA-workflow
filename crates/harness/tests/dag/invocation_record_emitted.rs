//! M2 integration test: one dispatched task yields exactly one
//! `runtime/invocations.jsonl` record.
//!
//! Drives the real harness binary against a seeded single-task package
//! with a no-op agent (`ECAA_EXECUTOR_MODE=local`, no `--session-id` so
//! no HTTP). The invocation record is written at the dispatch site
//! (after the WAL append, before the agent spawns), so a no-op agent is
//! sufficient to exercise the write.

use ecaa_workflow_harness::invocation_log::{invocation_log_path, InvocationRecord};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn harness_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ecaa-workflow-harness"))
}

fn write_executable(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

/// Seed a minimal package with exactly one Ready task that carries a
/// `source_atom_id` and a non-default sandbox requirement.
fn seed_single_task_package(pkg: &std::path::Path, task_id: &str) {
    std::fs::create_dir_all(pkg).unwrap();
    let workflow = serde_json::json!({
        "version": "1.0",
        "workflow_id": "invocation-record-test",
        "current_task": null,
        "tasks": {
            task_id: {
                "kind": "computation",
                "state": { "status": "ready" },
                "depends_on": [],
                "assignee": "agent",
                "description": "invocation record test task",
                "spec": { "stage_class": "quality_control", "task_id": task_id },
                "source_atom_id": "quality_control",
                "safety": { "sandbox": "process_isolation" }
            }
        }
    });
    std::fs::write(
        pkg.join("WORKFLOW.json"),
        serde_json::to_string_pretty(&workflow).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "drives the full harness binary, which blocks in init (waitpid on a \
            container/credential probe subprocess) in a bare sandbox without a \
            container runtime; consistent with the repo's other full-binary harness \
            tests that are #[ignore]d for environment sensitivity. The InvocationRecord \
            writer itself is covered by invocation_log::tests. Run in an environment \
            with a working local executor toolchain."]
fn one_dispatched_task_yields_exactly_one_invocation_record() {
    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    let task_id = "qc_preprocessing";
    seed_single_task_package(&pkg, task_id);

    // No-op agent: exits 0 immediately. The dispatch-site invocation
    // write fires before the agent runs, so the agent's behaviour is
    // irrelevant to this assertion.
    let agent = scratch.path().join("noop.sh");
    write_executable(&agent, "#!/usr/bin/env bash\nexit 0\n");

    let mut child = Command::new(harness_bin())
        .arg("--package")
        .arg(&pkg)
        .arg("--agent")
        .arg(&agent)
        .arg("--max-iterations")
        .arg("1")
        .arg("--no-interactive")
        .env("ECAA_EXECUTOR_MODE", "local")
        .env("ECAA_PILOT_ENABLED", "0")
        .env("ECAA_STALL_ENABLED", "0")
        .env("ECAA_HARNESS_SETTLE_SECS", "0")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn harness");

    let timeout = Duration::from_secs(60);
    let mut waited = Duration::ZERO;
    let step = Duration::from_millis(200);
    let mut terminated = false;
    while waited < timeout {
        if child.try_wait().expect("try_wait").is_some() {
            terminated = true;
            break;
        }
        std::thread::sleep(step);
        waited += step;
    }
    if !terminated {
        let _ = child.kill();
    }
    let output = child.wait_with_output().expect("wait child");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let path = invocation_log_path(&pkg);
    let body = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "invocations.jsonl must exist after a dispatch ({e}); harness output:\n{combined}"
        )
    });
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "exactly one invocation record per dispatched task; got {}:\n{body}\nharness output:\n{combined}",
        lines.len()
    );
    let rec: InvocationRecord = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(rec.task_id, task_id);
    assert_eq!(rec.atom_id.as_deref(), Some("quality_control"));
    assert!(
        rec.sandbox_required,
        "process_isolation sandbox must set sandbox_required"
    );
    assert!(
        rec.port_typed_inputs_satisfied,
        "no-prereq task is trivially satisfied"
    );
    assert!(
        rec.prerequisites.is_empty(),
        "no-prereq task has empty prerequisites"
    );
}
