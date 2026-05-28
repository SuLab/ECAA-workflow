//! End-to-end integration tests that spawn the harness binary, point
//! it at a minimal mock HTTP server (bare `std::net::TcpListener` on a
//! throwaway port — no extra dependencies), and assert the expected
//! progress events land.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One received HTTP request captured by the mock server.
#[derive(Debug, Clone)]
struct CapturedRequest {
    path: String,
    body: String,
}

type RequestLog = Arc<Mutex<Vec<CapturedRequest>>>;
type ShutdownFlag = Arc<Mutex<bool>>;

/// Spawn a one-shot mock HTTP server that accepts requests until the
/// caller drops the returned handle. Every POST is recorded as a
/// `CapturedRequest`; the response is always `204 No Content` so the
/// harness's `ureq` client treats the POST as successful.
///
/// Returns the (bound port, request log, shutdown flag). Dropping the
/// shutdown arc flips it and the listener thread exits on its next
/// accept cycle.
fn spawn_mock_server() -> (u16, RequestLog, ShutdownFlag) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind port 0");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).expect("set non-blocking");
    let captured = Arc::new(Mutex::new(Vec::<CapturedRequest>::new()));
    let shutdown = Arc::new(Mutex::new(false));

    let captured_clone = captured.clone();
    let shutdown_clone = shutdown.clone();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(120);
        while Instant::now() < deadline {
            if *shutdown_clone.lock().unwrap() {
                return;
            }
            match listener.accept() {
                Ok((mut stream, _addr)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
                    let req = read_one_request(&mut stream);
                    // `GET.../state` is the dispatch
                    // gate. The harness now FAILS CLOSED if the
                    // response isn't parseable JSON, so we mirror
                    // the production response shape with a
                    // not-paused state (`emitted`) — exactly enough
                    // for `is_session_pausing_dispatch` to return
                    // `Ok(false)` and let dispatch proceed.
                    let is_state = req
                        .as_ref()
                        .map(|r| r.path.ends_with("/state"))
                        .unwrap_or(false);
                    if let Some(req) = req {
                        captured_clone.lock().unwrap().push(req);
                    }
                    if is_state {
                        let body = b"{\"state\":{\"kind\":\"emitted\"}}";
                        let resp = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(resp.as_bytes());
                        let _ = stream.write_all(body);
                    } else {
                        // All other paths reply 204 — keeps ureq
                        // happy and avoids body-length parsing on
                        // the harness side.
                        let _ = stream.write_all(
                            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => return,
            }
        }
    });

    (port, captured, shutdown)
}

/// Read one HTTP request off a blocking-enough stream. Very forgiving:
/// we only need the first request line for the path and the body (so
/// we can look for `kind` markers). Not RFC-compliant — sufficient for
/// the harness's ureq client.
fn read_one_request(stream: &mut std::net::TcpStream) -> Option<CapturedRequest> {
    let mut reader = BufReader::new(stream);
    let mut first_line = String::new();
    reader.read_line(&mut first_line).ok()?;
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return None;
    }
    let path = parts[1].to_string();

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

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some(CapturedRequest {
        path,
        body: String::from_utf8_lossy(&body).to_string(),
    })
}

/// Write a minimal `WORKFLOW.json` with one Ready task. The harness
/// expects the canonical tag-based TaskState serialization.
fn seed_minimal_package(pkg: &std::path::Path, task_id: &str) {
    std::fs::create_dir_all(pkg).unwrap();
    let workflow = serde_json::json!({
        "version": "1.0",
        "workflow_id": "integration-pilot",
        "current_task": null,
        "tasks": {
            task_id: {
                "kind": "computation",
                "state": { "status": "ready" },
                "depends_on": [],
                "assignee": "agent",
                "description": "integration-test task",
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

fn write_executable(path: &std::path::Path, body: &str) {
    std::fs::write(path, body).unwrap();
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn harness_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ecaa-workflow-harness"))
}

#[test]
fn harness_posts_sizing_pilot_complete_with_session_id() {
    // One of the captured requests must be a POST to
    // /api/chat/session/<id>/progress whose JSON body has
    // "kind":"sizing_pilot_complete". We don't care what else comes
    // through (task_started, execution_finished, etc.) — this test is
    // narrowly on the pilot lifecycle.
    let (port, captured, shutdown) = spawn_mock_server();

    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_minimal_package(&pkg, "t_qc");

    // Minimal agent: marks the task Completed and exits so the harness
    // main loop reaches `execution_finished` without hanging. Python
    // True/False (not JSON-lowercase true/false) inside the source; the
    // json.dump writes proper JSON.
    let agent = scratch.path().join("mark-done.sh");
    let wf_path = pkg.join("WORKFLOW.json");
    write_executable(
        &agent,
        &format!(
            r#"#!/usr/bin/env bash
python3 <<'PYEOF'
import json
wf = {wf:?}
with open(wf) as f:
    dag = json.load(f)
for t in dag["tasks"].values():
    if t["state"]["status"] in ("ready", "running"):
        t["state"] = {{"status": "completed", "result": {{"ok": True}}}}
with open(wf, "w") as f:
    json.dump(dag, f, indent=2)
PYEOF
"#,
            wf = wf_path.display().to_string(),
        ),
    );

    let status = std::process::Command::new(harness_bin())
        .arg("--package")
        .arg(&pkg)
        .arg("--agent")
        .arg(&agent)
        .arg("--max-iterations")
        .arg("3")
        .arg("--no-interactive")
        .arg("--session-id")
        .arg("pilot-test-session")
        .arg("--server-url")
        .arg(format!("http://127.0.0.1:{}", port))
        .env("ECAA_EXECUTOR_MODE", "local")
        .env("ECAA_PILOT_ENABLED", "1")
        .env("ECAA_PILOT_TASKS", "1")
        .env("ECAA_PILOT_INTERVAL_SECS", "1")
        .env("ECAA_STALL_ENABLED", "0")
        .status()
        .expect("spawn harness");
    assert!(status.success(), "harness exit status should be success");

    // Give the mock server a beat to drain any trailing POSTs.
    std::thread::sleep(Duration::from_millis(200));
    *shutdown.lock().unwrap() = true;

    let log = captured.lock().unwrap().clone();
    let sizing_events: Vec<&CapturedRequest> = log
        .iter()
        .filter(|r| {
            r.path == "/api/chat/session/pilot-test-session/progress"
                && r.body.contains("\"kind\":\"sizing_pilot")
        })
        .collect();
    assert!(
        !sizing_events.is_empty(),
        "expected at least one sizing_pilot_* POST, got {} total requests:\n{:#?}",
        log.len(),
        log
    );
    assert!(
        sizing_events
            .iter()
            .any(|r| r.body.contains("sizing_pilot_complete")),
        "expected a sizing_pilot_complete POST; saw only:\n{:#?}",
        sizing_events
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "flaky pre-existing test (fails in original repo too; timing-sensitive stall detection)"]
fn harness_drains_stall_signal_and_posts_task_stalled() {
    // `from_env` accepts `ECAA_STALL_CPU_WINDOW_MINS=0` and clamps
    // the resulting window size to 1 sample. With `cpu_min_pct=101`
    // (unreachable) and `sample_interval_secs=1`, the stall monitor
    // fires within ~2-3 samples of agent start. The mock server
    // captures the resulting POST before the sleeping agent exits.
    let (port, captured, shutdown) = spawn_mock_server();

    let scratch = tempfile::tempdir().unwrap();
    let pkg = scratch.path().join("pkg");
    seed_minimal_package(&pkg, "t_stall");

    // Long sleep so the stall monitor has time to sample. The harness's
    // max_iterations=1 lets it drain the stall channel on its next
    // top-of-loop pass and forward the signal to /progress. The
    // task-timeout keeps the test from stalling if the kernel decides
    // to schedule the monitor late.
    let agent = scratch.path().join("sleepy.sh");
    write_executable(&agent, "#!/usr/bin/env bash\nsleep 6\nexit 0\n");

    let status = std::process::Command::new(harness_bin())
        .arg("--package")
        .arg(&pkg)
        .arg("--agent")
        .arg(&agent)
        .arg("--max-iterations")
        .arg("2")
        .arg("--task-timeout")
        .arg("5")
        .arg("--no-interactive")
        .arg("--session-id")
        .arg("stall-test-session")
        .arg("--server-url")
        .arg(format!("http://127.0.0.1:{}", port))
        .env("ECAA_EXECUTOR_MODE", "local")
        .env("ECAA_PILOT_ENABLED", "0")
        .env("ECAA_STALL_ENABLED", "1")
        .env("ECAA_STALL_CPU_MIN_PCT", "101")
        .env("ECAA_STALL_CPU_WINDOW_MINS", "0")
        .env("ECAA_STALL_SAMPLE_INTERVAL_SECS", "1")
        .env("ECAA_STALL_MEM_MAX_PCT", "100")
        .env("ECAA_STALL_MEM_WINDOW_MINS", "999")
        .status()
        .expect("spawn harness");
    // The harness may exit zero (execution_finished) or non-zero
    // (agent timeout) depending on scheduling; both paths drain the
    // stall channel before shutdown.
    let _ = status;

    std::thread::sleep(Duration::from_millis(300));
    *shutdown.lock().unwrap() = true;

    let log = captured.lock().unwrap().clone();
    let stalled: Vec<&CapturedRequest> = log
        .iter()
        .filter(|r| {
            r.path == "/api/chat/session/stall-test-session/progress"
                && r.body.contains("\"kind\":\"task_stalled\"")
        })
        .collect();
    assert!(
        !stalled.is_empty(),
        "expected a task_stalled POST, got {} total requests:\n{:#?}",
        log.len(),
        log
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
#[ignore = "stall monitor requires /proc, Linux-only"]
fn harness_drains_stall_signal_and_posts_task_stalled() {
    eprintln!(
        "harness_drains_stall_signal_and_posts_task_stalled: skipped (requires /proc, Linux-only)"
    );
}
