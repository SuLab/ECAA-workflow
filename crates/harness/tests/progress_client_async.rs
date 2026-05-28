//! `ProgressClient` posts must not block the
//! harness main thread on a network outage.
//!
//! Pre-19.3, every `pc.task_started(...)` / `task_completed(...)` /
//! `task_blocked(...)` / `set_task_state(...)` call did a sync
//! `ureq` POST with connect_timeout=2s + 4 attempts + exponential
//! backoff (100ms / 500ms / 2000ms), summing to ~30s worst-case
//! per call. WAL recovery emitting N `task_blocked` events
//! serialized N x 30s of dead time on the main thread.
//!
//! After 19.3 the public methods enqueue into a bounded mpsc and
//! return immediately; an internal sender thread does the HTTP
//! work. The 50-enqueue smoke below should finish in well under
//! 50 ms — orders of magnitude below the legacy worst case — even
//! against a guaranteed-unreachable port.

use std::time::{Duration, Instant};

use scripps_workflow_harness::progress_client::ProgressClient;

/// Port 1 is reserved and refuses connections immediately on Linux.
/// We use it (rather than a random unbound port) so the OS rejects
/// the SYN deterministically without the OS having to time out.
const UNREACHABLE_BASE_URL: &str = "http://127.0.0.1:1";

#[test]
fn progress_client_task_started_does_not_block_main_thread_on_server_outage() {
    let pc = ProgressClient::new("session-async-smoke", UNREACHABLE_BASE_URL);
    let start = Instant::now();
    for i in 0..50 {
        pc.task_started(&format!("task_{}", i), "smoke");
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "50 task_started enqueues took {:?} — the main thread is blocking on the network. \
         Pre-19.3 this would take ~50 × 30s = 25 minutes. Post-19.3 the calls should \
         enqueue into the bounded channel in microseconds.",
        elapsed
    );
}

#[test]
fn progress_client_task_blocked_does_not_block_main_thread_on_server_outage() {
    // Mirrors the WAL-recovery hot path: N task_blocked events emitted
    // back-to-back. This is the path that motivated 19.3.
    let pc = ProgressClient::new("session-wal-recovery", UNREACHABLE_BASE_URL);
    let start = Instant::now();
    for i in 0..50 {
        pc.task_blocked(&format!("task_{}", i), "orphaned by crash");
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "50 task_blocked enqueues took {:?} — main thread is blocking on network",
        elapsed
    );
}

#[test]
fn progress_client_set_task_state_does_not_block_main_thread_on_server_outage() {
    use scripps_workflow_core::dag::TaskState;
    let pc = ProgressClient::new("session-state-mirror", UNREACHABLE_BASE_URL);
    let state = TaskState::Ready;
    let start = Instant::now();
    for i in 0..50 {
        pc.set_task_state(&format!("task_{}", i), &state);
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(500),
        "50 set_task_state enqueues took {:?} — main thread is blocking on network",
        elapsed
    );
}

/// `is_session_pausing_dispatch` must FAIL CLOSED
/// on network errors. Pre-19.4 it returned `false` (fail-open) which
/// let the harness launch agents against a paused session whenever the
/// server was unreachable.
///
/// Post-19.4 the method returns `Result<bool>`; the caller in
/// `main.rs` treats `Err` as "paused" and sleeps
/// `SWFC_HARNESS_SETTLE_SECS` before retrying. Here we exercise the
/// underlying primitive: an unreachable server must produce an `Err`,
/// not a silent `Ok(false)`.
#[test]
fn is_session_pausing_dispatch_returns_err_on_network_error() {
    let pc = ProgressClient::new("session-network-err", UNREACHABLE_BASE_URL);
    let result = pc.is_session_pausing_dispatch();
    assert!(
        result.is_err(),
        "is_session_pausing_dispatch must return Err when the server is unreachable; \
         got Ok({:?}). The previous fail-open behavior let the harness happily \
         dispatch agents during a server hiccup while the SME was mid-amend.",
        result.ok()
    );
}

/// Counterpart to the network-err test: when the server replies with
/// a parseable state-payload, `is_session_pausing_dispatch` returns
/// `Ok(bool)` keyed off the `state.kind` field. Verifies the happy
/// path stayed correct through the §19.4 Result refactor.
#[test]
fn is_session_pausing_dispatch_returns_ok_true_for_blocked_state() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    // One-shot mock that always replies with `{"state":{"kind":"blocked"}}`.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind port 0");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).expect("set non-blocking");
    let shutdown = Arc::new(Mutex::new(false));
    let shutdown_clone = shutdown.clone();
    let handle = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if *shutdown_clone.lock().unwrap() {
                return;
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
                    // Consume the request headers + body; we don't
                    // need to parse them.
                    let mut reader = BufReader::new(&mut stream);
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_err() {
                            break;
                        }
                        if line.trim_end_matches(['\r', '\n']).is_empty() {
                            break;
                        }
                    }
                    let body = br#"{"state":{"kind":"blocked"}}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.write_all(body);
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => return,
            }
        }
    });

    let pc = ProgressClient::new("session-blocked", format!("http://127.0.0.1:{}", port));
    let result = pc.is_session_pausing_dispatch();

    *shutdown.lock().unwrap() = true;
    let _ = handle.join();

    assert!(
        matches!(result, Ok(true)),
        "is_session_pausing_dispatch must return Ok(true) for state.kind='blocked'; \
         got {:?}",
        result
    );
}
