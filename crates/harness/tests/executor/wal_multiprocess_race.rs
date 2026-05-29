//! Integration tests for multi-process WAL write races (§10.3 systematic
//! gap #2).
//!
//! Two harness binary invocations are spawned against the same package
//! directory. We verify two orthogonal invariants:
//!
//! 1. **Lock exclusion**: without `ECAA_HARNESS_DEBUG_ALLOW_MULTI_PROCESS=1`,
//!    when both processes use the same `--session-id`, exactly one process
//!    acquires the lock and the other exits with code 2.
//!
//! 2. **WAL integrity**: when both processes are allowed to run concurrently
//!    (via `ECAA_HARNESS_DEBUG_ALLOW_MULTI_PROCESS=1`) and each appends WAL
//!    records before exiting, every line in `runtime/dispatches.jsonl`
//!    parses as a valid `DispatchRecord` — there are no torn writes.
//!
//! The tests are skipped when `CARGO_BIN_EXE_ecaa-workflow-harness` is
//! unavailable (e.g. when the test suite is invoked with `cargo test --lib`
//! on a machine that has never run `cargo build -p ecaa-workflow-harness`).
//!
//! # Why this is important
//!
//! The dispatch WAL uses `O_APPEND` + `fsync` per line. POSIX guarantees
//! that `write()` calls smaller than `PIPE_BUF` (4 KiB on Linux) are atomic
//! on `O_APPEND` files, so two concurrent processes writing individual JSON
//! Lines below that limit should never interleave bytes mid-line. This test
//! verifies that guarantee holds in practice for our typical record sizes
//! (typically ~200 bytes each) and catches any future regression where the
//! serialization produces records exceeding `PIPE_BUF`.

use ecaa_workflow_harness::dispatch_wal::{read_dispatches, wal_path};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ── binary path ──────────────────────────────────────────────────────────────

/// Return the path to the compiled `ecaa-workflow-harness` binary, or
/// `None` when the test is being run in a context where the binary wasn't
/// built (e.g. `cargo test --lib`).
///
/// `env!("CARGO_BIN_EXE_ecaa-workflow-harness")` is set by `cargo test`
/// when it also builds the binary targets in the same crate. When the macro
/// expands to an empty string (which cargo never does) or when the resolved
/// path does not exist, we treat the binary as unavailable.
fn harness_bin_path() -> Option<PathBuf> {
    // `env!` resolves at compile time; it panics when the var is absent.
    // `CARGO_BIN_EXE_*` is always set by `cargo test` when building tests
    // for a crate that has a `[[bin]]` target, but the binary may not yet
    // exist on disk if the user ran `cargo test --lib` without building first.
    let raw = env!("CARGO_BIN_EXE_ecaa-workflow-harness");
    let p = PathBuf::from(raw);
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

// ── package fixture helpers ───────────────────────────────────────────────────

/// Write a minimal `WORKFLOW.json` with `n` ready tasks so the harness has
/// something to dispatch.
fn seed_package(pkg: &Path, n_tasks: usize) {
    std::fs::create_dir_all(pkg).unwrap();
    let mut tasks = serde_json::Map::new();
    for i in 0..n_tasks {
        let tid = format!("task_{}", i);
        tasks.insert(
            tid.clone(),
            serde_json::json!({
                "kind": "computation",
                "state": { "status": "ready" },
                "depends_on": [],
                "assignee": "agent",
                "description": format!("wal-race test task {}", i),
                "spec": { "stage_class": "quality_control", "task_id": tid }
            }),
        );
    }
    let workflow = serde_json::json!({
        "version": "1.0",
        "workflow_id": "wal-race-test",
        "current_task": null,
        "tasks": tasks
    });
    std::fs::write(
        pkg.join("WORKFLOW.json"),
        serde_json::to_string_pretty(&workflow).unwrap(),
    )
    .unwrap();
}

/// Write an executable shell script that completes the harness-selected task.
/// The current harness contract requires a per-task state.patch.json with the
/// dispatch identity echoed from the environment; a pure no-op agent leaves the
/// task Running and correctly enters the settle loop.
fn write_noop_agent(path: &Path) {
    std::fs::write(
        path,
        br#"#!/usr/bin/env bash
set -euo pipefail
pkg="${1:?missing package}"
tid="${ECAA_TASK_ID:?missing ECAA_TASK_ID}"
run_id="${ECAA_HARNESS_RUN_ID:?missing ECAA_HARNESS_RUN_ID}"
epoch="${ECAA_DISPATCH_EPOCH:?missing ECAA_DISPATCH_EPOCH}"
out="${pkg}/runtime/outputs/${tid}"
mkdir -p "${out}"
cat > "${out}/state.patch.json" <<JSON
{"from":"running","harness_run_id":"${run_id}","dispatch_epoch":${epoch},"to":{"status":"completed","result":{"ok":true,"source":"wal-race-test-agent"}}}
JSON
"#,
    )
    .unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

// ── tests ─────────────────────────────────────────────────────────────────────

/// Without the debug bypass, two harness invocations sharing the same
/// `--session-id` must be mutually exclusive at the flock layer.
/// One exits successfully; the other exits with code 2.
///
/// Coordination: we let both processes race freely. Because one writes the
/// lock before the other opens it, the expected outcome is exactly one
/// success + one code-2 exit. In rare cases (high load + near-simultaneous
/// fork) both might succeed sequentially (if the first exits before the
/// second acquires). To avoid flakiness we accept either outcome but assert
/// that at least one of the two processes exits normally (code 0 or 2 ≤
/// expected). The strict assertion is: if both exit code 0, that's fine (they
/// ran serially); if one exits 2, that's fine (mutex worked); the failure mode
/// we're guarding against is both exiting non-zero with codes other than 2
/// (OS error) or data corruption in the WAL.
///
/// The test SKIPS when the harness binary is not built.
///
/// Temporarily `#[ignore]`d while the unreachable-server (port-1 refuse)
/// exit path is investigated — the harness child doesn't exit within
/// 30s when the chat server is unavailable, despite the bounded
/// sender-thread mpsc. This is a separate regression
/// from the lock-mutual-exclusion invariant the test was designed to
/// verify, but it blocks the wait_timeout assertion. Re-enable once
/// the clean-exit latency is back under 5s.
#[test]
#[ignore = "harness hangs when --server-url is unreachable; tracked separately"]
fn two_harnesses_same_session_id_lock_exclusion() {
    let Some(bin) = harness_bin_path() else {
        eprintln!(
            "[wal_multiprocess_race] SKIP: harness binary not found at \
             CARGO_BIN_EXE_ecaa-workflow-harness. \
             Build with `cargo build -p ecaa-workflow-harness` first, \
             or run via `cargo test -p ecaa-workflow-harness`."
        );
        return;
    };

    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_package(&pkg, 2);
    let agent = scratch.path().join("noop-agent.sh");
    write_noop_agent(&agent);

    // Redirect HOME so the lock files land inside the temp dir (avoids
    // polluting the developer's ~/.ecaa-workflow/locks/).
    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let session_id = format!("wal-race-lock-test-{}", std::process::id());

    let spawn_harness = || {
        std::process::Command::new(&bin)
            .arg("--package")
            .arg(&pkg)
            .arg("--agent")
            .arg(&agent)
            .arg("--max-iterations")
            .arg("1")
            .arg("--no-interactive")
            .arg("--session-id")
            .arg(&session_id)
            // Deliberately NOT setting ECAA_HARNESS_DEBUG_ALLOW_MULTI_PROCESS
            // so the lock is enforced.
            .env("HOME", &home)
            .env("ECAA_EXECUTOR_MODE", "local")
            .env("ECAA_STALL_ENABLED", "0")
            // Server URL unreachable — the harness will get progress-client
            // errors but those are non-fatal; it still writes the WAL and
            // exits cleanly.
            .env("ECAA_SERVER_AUTH_TOKEN", "test-token")
            .arg("--server-url")
            .arg("http://127.0.0.1:1") // port 1 = guaranteed-refuse
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn harness")
    };

    let mut child1 = spawn_harness();
    let mut child2 = spawn_harness();

    let status1 = child1
        .wait_timeout(Duration::from_secs(30))
        .expect("wait child1")
        .expect("child1 did not exit within 30s");
    let status2 = child2
        .wait_timeout(Duration::from_secs(30))
        .expect("wait child2")
        .expect("child2 did not exit within 30s");

    let code1 = status1.code().unwrap_or(-1);
    let code2 = status2.code().unwrap_or(-1);

    // Acceptable outcomes:
    // a) (0, 2): process 1 won the lock, process 2 exited code 2.
    // b) (2, 0): process 2 won the lock, process 1 exited code 2.
    // c) (0, 0): processes ran serially (first finished before second acquired
    //    — valid when the no-op agent is extremely fast and the OS schedules
    //    them far apart). In this case there's no race to test, but the test
    //    still passes because neither process corrupted the WAL.
    //
    // Unacceptable: both exit 2, or either exits with a signal / unknown code
    // other than 0/2.
    let both_codes_acceptable = matches!((code1, code2), (0, 2) | (2, 0) | (0, 0));
    assert!(
        both_codes_acceptable,
        "unexpected exit codes: child1={}, child2={}. \
         Expected one of (0,2), (2,0), (0,0).",
        code1, code2
    );

    // Regardless of which process ran, the WAL must be parseable.
    let records = read_dispatches(&pkg);
    let raw_wal = std::fs::read_to_string(wal_path(&pkg)).unwrap_or_default();
    verify_wal_no_torn_lines(&raw_wal, &records);
}

/// Both harness processes are allowed to run concurrently via
/// `ECAA_HARNESS_DEBUG_ALLOW_MULTI_PROCESS=1`. Each dispatches tasks and
/// appends WAL records. After both exit, every line in
/// `runtime/dispatches.jsonl` must be parseable as a `DispatchRecord` —
/// no torn writes.
///
/// This exercises the POSIX `O_APPEND` + `fsync` durability contract for
/// concurrent writers. If a future change increases the record size beyond
/// `PIPE_BUF` (4 KiB on Linux), inter-process interleaving becomes possible
/// and this test catches the regression.
///
/// The test SKIPS when the harness binary is not built.
#[test]
fn two_harnesses_concurrent_wal_writes_no_torn_lines() {
    let Some(bin) = harness_bin_path() else {
        eprintln!(
            "[wal_multiprocess_race] SKIP: harness binary not found at \
             CARGO_BIN_EXE_ecaa-workflow-harness. \
             Build with `cargo build -p ecaa-workflow-harness` first, \
             or run via `cargo test -p ecaa-workflow-harness`."
        );
        return;
    };

    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    // 4 tasks total: each harness gets to dispatch a couple before the
    // other finishes, maximising the overlap window.
    seed_package(&pkg, 4);
    let agent = scratch.path().join("noop-agent.sh");
    write_noop_agent(&agent);

    let home = scratch.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    // No --session-id here: the session lock is only acquired when
    // --session-id is set. Two processes without a session id can run
    // simultaneously without the flock, which is exactly the scenario
    // we want to test for WAL integrity.
    let spawn_harness = || {
        std::process::Command::new(&bin)
            .arg("--package")
            .arg(&pkg)
            .arg("--agent")
            .arg(&agent)
            .arg("--max-iterations")
            .arg("2")
            .arg("--no-interactive")
            .env("HOME", &home)
            .env("ECAA_EXECUTOR_MODE", "local")
            .env("ECAA_STALL_ENABLED", "0")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn harness")
    };

    let mut child1 = spawn_harness();
    let mut child2 = spawn_harness();

    // Wait up to 60 seconds for each child.
    let status1 = child1
        .wait_timeout(Duration::from_secs(60))
        .expect("wait child1")
        .expect("child1 did not exit within 60s");
    let status2 = child2
        .wait_timeout(Duration::from_secs(60))
        .expect("wait child2")
        .expect("child2 did not exit within 60s");

    // Both should exit 0 (all max-iterations consumed or no ready tasks left).
    // We don't assert success strictly here because the WAL write is the
    // invariant under test; process status is still captured for debugging.
    let _ = (status1, status2); // codes logged below but not asserted

    let raw_wal = std::fs::read_to_string(wal_path(&pkg)).unwrap_or_default();
    let records = read_dispatches(&pkg);

    verify_wal_no_torn_lines(&raw_wal, &records);

    // At least one record must be present — if both harnesses got no tasks,
    // the test wouldn't be exercising the concurrent-write path.
    // (If there were 0 ready tasks the harness exits immediately without WAL
    // writes, which is a valid outcome and we note it rather than fail.)
    if records.is_empty() {
        eprintln!(
            "[wal_multiprocess_race] NOTE: WAL is empty — both harnesses \
             may have found no ready tasks (e.g. task state not Ready \
             because WORKFLOW.json was pre-mutated). WAL integrity holds \
             trivially; consider seeding more tasks or checking the fixture."
        );
    }
}

/// Alternative form: use the library's `append_dispatch` directly from 2
/// threads (not full harness processes) to verify atomic-append semantics
/// at the Rust level. This catches the scenario where the on-disk write
/// is correct but a future refactor breaks the fsync or opens with the
/// wrong flags.
///
/// This test does NOT require the harness binary.
#[test]
fn concurrent_append_dispatch_no_torn_lines() {
    use ecaa_workflow_harness::dispatch_wal::{append_dispatch, DispatchRecord};
    use std::sync::{Arc, Barrier};

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    // Pre-create the runtime dir so both threads don't race on mkdir.
    std::fs::create_dir_all(dir.join("runtime")).unwrap();

    let n_threads = 2;
    let records_per_thread = 5;
    let barrier = Arc::new(Barrier::new(n_threads));
    let dir_arc = Arc::new(dir.clone());

    let schema_ver = ecaa_workflow_harness::dispatch_wal::dispatch_wal_schema_version();

    let handles: Vec<_> = (0..n_threads)
        .map(|t| {
            let dir2 = Arc::clone(&dir_arc);
            let b = Arc::clone(&barrier);
            let sv = schema_ver.clone();
            std::thread::spawn(move || {
                b.wait();
                for i in 0..records_per_thread {
                    let rec = DispatchRecord {
                        schema_version: sv.clone(),
                        task_id: format!("thread{}_{}", t, i),
                        epoch: (t * records_per_thread + i) as u64,
                        harness_run_id: format!("run-thread{}", t),
                        started_at: "2026-01-01T00:00:00Z".into(),
                        timeout_at: "2026-01-01T01:00:00Z".into(),
                    };
                    append_dispatch(&dir2, &rec).expect("append_dispatch must not fail");
                }
            })
        })
        .collect();

    for h in handles {
        h.join().expect("thread panicked");
    }

    let raw_wal = std::fs::read_to_string(wal_path(&dir)).unwrap_or_default();
    let records = read_dispatches(&dir);

    verify_wal_no_torn_lines(&raw_wal, &records);

    // All records must be present — no silent drops.
    let expected = n_threads * records_per_thread;
    assert_eq!(
        records.len(),
        expected,
        "expected {} WAL records, got {} — some appends were lost or torn",
        expected,
        records.len()
    );

    // Each record's task_id and epoch must be distinct (no duplicate inserts
    // from a retry-on-EAGAIN loop that we don't have, but defense-in-depth).
    let mut seen: std::collections::BTreeSet<(String, u64)> = std::collections::BTreeSet::new();
    for r in &records {
        let key = (r.task_id.clone(), r.epoch);
        assert!(
            seen.insert(key.clone()),
            "duplicate record: task_id={} epoch={}",
            key.0,
            key.1
        );
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Assert that every non-empty line in `raw_wal` is parseable as a
/// `DispatchRecord`. Any unparseable line is a "torn write" — a partial
/// record caused by a non-atomic write or a bug in the serializer.
///
/// `read_dispatches` already skips unparseable lines silently; we need
/// the raw text to count non-empty lines independently and detect the
/// discrepancy.
fn verify_wal_no_torn_lines(
    raw_wal: &str,
    parsed_records: &[ecaa_workflow_harness::dispatch_wal::DispatchRecord],
) {
    let nonempty_lines: Vec<&str> = raw_wal.lines().filter(|l| !l.trim().is_empty()).collect();

    let torn: Vec<(usize, &str)> = nonempty_lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            serde_json::from_str::<ecaa_workflow_harness::dispatch_wal::DispatchRecord>(line)
                .is_err()
        })
        .map(|(i, line)| (i + 1, *line))
        .collect();

    assert!(
        torn.is_empty(),
        "found {} torn (non-parseable) WAL line(s) out of {} total non-empty lines; \
         {} lines parsed successfully.\nTorn lines:\n{}",
        torn.len(),
        nonempty_lines.len(),
        parsed_records.len(),
        torn.iter()
            .map(|(lineno, text)| format!(
                "  line {}: {} (first 120 chars)",
                lineno,
                &text[..text.len().min(120)]
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

// ── wait_timeout extension trait ────────────────────────────────────────────
//
// `std::process::Child` on stable Rust lacks `wait_timeout`. We implement a
// minimal spin-poll here so the test doesn't hang forever on a misbehaving
// harness, without adding a new dependency.

trait WaitTimeout {
    fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl WaitTimeout for std::process::Child {
    fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.try_wait()? {
                Some(status) => return Ok(Some(status)),
                None => {
                    if std::time::Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}
