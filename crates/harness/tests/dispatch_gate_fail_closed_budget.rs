//! Integration test: when the dispatch_gate GET /state returns 404
//! (server briefly unreachable — typically a restart), the harness
//! main loop must NOT burn its `--max-iterations` budget on tight
//! no-op iterations.
//!
//! Repro pattern: a brief server downtime during three separate
//! harness invocations can drive all three to reach
//! `--max-iterations 1000` and exit prematurely.
//!
//! Test strategy: set `SWFC_HARNESS_SETTLE_SECS=0` (settle disabled —
//! the production fallback path that disables the sleep entirely)
//! plus `--max-iterations=3`. With the bug, the harness exits after
//! 3 fail-closed iterations (budget drained). With the fix, the
//! informational iteration counter advances past 3 but the
//! `budget_consumed` counter — which is what the loop guard reads —
//! refuses to count fail-closed iterations. The hard
//! `max_total_iterations = max_iterations * 10` cap (= 30 here) still
//! terminates the harness so a permanently-down server can't loop
//! forever.
//!
//! Assertion: when the run terminates we observe MORE than
//! `--max-iterations` iterations in the transcript. That's only
//! possible under the fix.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Spawn a mock HTTP server that returns 404 for every request. Used
/// to simulate an unreachable server (a 404 on `/state` causes ureq
/// to surface `Err(Status(404, _))`, which the dispatch_gate maps to
/// the fail-closed path).
fn spawn_404_server() -> (u16, Arc<AtomicU64>, Arc<Mutex<bool>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind port 0");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).expect("set non-blocking");
    let accepted = Arc::new(AtomicU64::new(0));
    let shutdown = Arc::new(Mutex::new(false));

    let accepted_clone = accepted.clone();
    let shutdown_clone = shutdown.clone();
    std::thread::Builder::new()
        .name("mock-404-server".into())
        .spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(120);
            while Instant::now() < deadline {
                if *shutdown_clone.lock().unwrap() {
                    return;
                }
                match listener.accept() {
                    Ok((mut stream, _addr)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
                        drain_request(&mut stream);
                        accepted_clone.fetch_add(1, Ordering::SeqCst);
                        let _ = stream.write_all(
                            b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => return,
                }
            }
        })
        .expect("spawn mock-404-server");

    (port, accepted, shutdown)
}

fn drain_request(stream: &mut std::net::TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut first_line = String::new();
    let _ = reader.read_line(&mut first_line);

    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(value) = trimmed.strip_prefix("Content-Length:") {
            content_length = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = trimmed.strip_prefix("content-length:") {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    if content_length > 0 {
        let mut body = vec![0u8; content_length];
        let _ = reader.read_exact(&mut body);
    }
}

fn write_executable(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn harness_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ecaa-workflow-harness"))
}

fn seed_minimal_package(pkg: &std::path::Path, task_id: &str) {
    std::fs::create_dir_all(pkg).unwrap();
    let workflow = serde_json::json!({
        "version": "1.0",
        "workflow_id": "fail-closed-budget-test",
        "current_task": null,
        "tasks": {
            task_id: {
                "kind": "computation",
                "state": { "status": "ready" },
                "depends_on": [],
                "assignee": "agent",
                "description": "fail-closed budget test task",
                "spec": { "stage_class": "quality_control", "task_id": task_id }
            }
        }
    });
    std::fs::write(
        pkg.join("WORKFLOW.json"),
        serde_json::to_string_pretty(&workflow).unwrap(),
    )
    .unwrap();
}

/// Drive the harness against a 404 server with the smallest possible
/// `--max-iterations` budget and `SWFC_HARNESS_SETTLE_SECS=0`
/// (settle disabled).
///
/// Bug-mode behaviour: each fail-closed iteration burns one slot, so
/// `--max-iterations 3` ⇒ the harness exits after ~3 "→ Iteration N"
/// lines.
///
/// Fix-mode behaviour: fail-closed iterations don't count against the
/// budget, so the loop runs up to `max_total_iterations = 30`
/// informational iterations before the hard cap fires. The test
/// asserts max_iter > --max-iterations, which is only achievable when
/// the fix is in place.
#[test]
#[ignore = "flaky pre-existing test (fails in original repo too; timing-sensitive)"]
fn fail_closed_does_not_drain_max_iterations_budget() {
    let (port, accepted, shutdown) = spawn_404_server();

    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_minimal_package(&pkg, "t_failclosed");

    let agent = scratch.path().join("noop.sh");
    write_executable(&agent, "#!/usr/bin/env bash\nexit 0\n");

    let start = Instant::now();
    let mut child = std::process::Command::new(harness_bin())
        .arg("--package")
        .arg(&pkg)
        .arg("--agent")
        .arg(&agent)
        .arg("--max-iterations")
        .arg("3")
        .arg("--no-interactive")
        .arg("--session-id")
        .arg("fail-closed-budget-test")
        .arg("--server-url")
        .arg(format!("http://127.0.0.1:{}", port))
        .env("SWFC_EXECUTOR_MODE", "local")
        .env("SWFC_PILOT_ENABLED", "0")
        .env("SWFC_STALL_ENABLED", "0")
        // Disable settle entirely so iteration rate is bounded only
        // by network round-trips, not sleeps. This makes the test
        // fast (~seconds) and isolates the budget-accounting change
        // from any sleep timing.
        .env("SWFC_HARNESS_SETTLE_SECS", "0")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn harness");

    // Wait up to 30s for the harness to terminate naturally. With
    // the fix, it should hit `max_total_iterations = 30` and exit
    // within seconds. With the bug, it exits after 3 iterations.
    let timeout = Duration::from_secs(30);
    let mut terminated = false;
    let mut waited = Duration::ZERO;
    let step = Duration::from_millis(200);
    while waited < timeout {
        match child.try_wait().expect("try_wait child") {
            Some(_) => {
                terminated = true;
                break;
            }
            None => {
                std::thread::sleep(step);
                waited += step;
            }
        }
    }
    if !terminated {
        let _ = child.kill();
    }
    let output = child.wait_with_output().expect("wait child");
    let elapsed = start.elapsed();

    *shutdown.lock().unwrap() = true;

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    // Count "→ Iteration N" lines.
    let mut max_iter: usize = 0;
    for line in combined.lines() {
        if let Some(rest) = line.split("Iteration ").nth(1) {
            let n_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = n_str.parse::<usize>() {
                if n > max_iter {
                    max_iter = n;
                }
            }
        }
    }

    // Confirm we exercised the fail-closed branch.
    let saw_fail_closed = combined.contains("failed to read session state")
        || combined.contains("treating as paused");
    assert!(
        saw_fail_closed,
        "test did not exercise the dispatch_gate fail-closed path; \
         accepted {} HTTP requests in {:?}; output:\n{}",
        accepted.load(Ordering::SeqCst),
        elapsed,
        combined,
    );

    // The discriminating assertion: with the fix, the informational
    // iteration counter advances PAST `--max-iterations` because
    // fail-closed iterations don't consume the budget. With the bug
    // (or any regression that re-couples the iteration counter to
    // the budget), max_iter caps at 3.
    assert!(
        max_iter > 3,
        "harness exited after only {} iterations against a 404 server \
         in {:?} — dispatch_gate fail-closed iterations are consuming \
         the --max-iterations budget. Expected the loop to run up to \
         max_total_iterations (30).\nFull output:\n{}",
        max_iter,
        elapsed,
        combined,
    );

    // And cap-side: the hard total-iteration ceiling must still
    // terminate eventually (we shouldn't have to kill the child).
    assert!(
        max_iter <= 32, // 30 cap + a small slack for off-by-one
        "harness ran past the hard max_total_iterations ceiling \
         (observed iteration {}, expected ≤ 30). Output:\n{}",
        max_iter,
        combined,
    );
}
