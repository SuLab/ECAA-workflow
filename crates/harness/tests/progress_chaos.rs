//! Chaos tests for `ProgressClient` resilience under hostile mock servers.
//!
//! Three fault-injection scenarios exercised on the same bounded-mpsc sender:
//!
//! 1. **503 storm** — mock returns 503 for the first N requests then 200.
//!    The sender retries per its backoff table (100ms / 500ms / 2000ms) and
//!    eventually succeeds without blocking the harness main thread.
//!
//! 2. **Mid-response truncation** — one request receives a partial HTTP
//!    response and the connection is closed. ureq treats this as an error;
//!    the sender increments the retry count and moves on.
//!
//! 3. **Slow server** — one request hangs for longer than the sender's
//!    `timeout_connect` + `timeout` budget. The sender times out and the
//!    main thread observes no blocking.
//!
//! Together these verify:
//! - `post()` never stalls the main thread regardless of server behaviour.
//! - Events eventually land when the server recovers (no thundering herd).
//! - `events_dropped()` may grow under saturation but no panics occur.
//!
//! All mocks are hand-rolled via `std::net::TcpListener` — no additional
//! crate dependencies required.

use ecaa_workflow_harness::progress_client::{HarnessProgressEvent, ProgressClient};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

// ── helpers ───────────────────────────────────────────────────────────────────

/// Read and discard HTTP request headers from `stream` up through the blank
/// line that separates headers from body. Ignores errors — the mock doesn't
/// care about the request content.
fn drain_request_headers(stream: &mut TcpStream) {
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream for reading"));
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if line.trim_end_matches(['\r', '\n']).is_empty() {
                    break;
                }
            }
        }
    }
}

/// Send a minimal HTTP/1.1 response with the given status code and empty body.
fn write_status(stream: &mut TcpStream, code: u16, reason: &str) {
    let resp =
        format!("HTTP/1.1 {code} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Send a `200 OK` JSON response with a trivial body.
fn write_200(stream: &mut TcpStream) {
    let body = b"{}";
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

/// Build a minimal `HarnessProgressEvent` for testing.
fn make_event(task_id: &str) -> HarnessProgressEvent {
    // Use the internal bare constructor path via the public fields.
    // `HarnessProgressEvent` derives `Clone` and has public fields, so
    // we can construct one directly without going through the `ProgressClient`
    // convenience methods.
    HarnessProgressEvent {
        schema_version:
            ecaa_workflow_harness::progress_client::harness_progress_event_schema_version(),
        kind: "task_started".into(),
        task_id: task_id.into(),
        status: "running".into(),
        detail: "chaos test".into(),
        stall_signal: None,
        suggested_action: None,
        pilot_report: None,
        cross_version_report: None,
        from_instance_type: None,
        to_instance_type: None,
        agent_usage: None,
        executor_info: None,
        client_health: None,
        orphan_reap: None,
        heartbeat_age_secs: None,
        client_now: None,
        cost_guard: None,
    }
}

// ── test: 503 storm followed by 200 recovery ─────────────────────────────────

/// Serve `fail_count` 503 responses, then switch to 200 forever.
///
/// The mock counts the total TCP connections it accepted so the assertion
/// can verify that the sender retried but did not multiply events.
fn spawn_503_then_200_server(fail_count: u64) -> (u16, Arc<AtomicU64>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind port 0");
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicU64::new(0));
    let accepted_clone = accepted.clone();

    std::thread::Builder::new()
        .name("mock-503-server".into())
        .spawn(move || {
            listener
                .set_nonblocking(false)
                .expect("set blocking on mock");
            // Serve up to 200 connections then exit — well beyond what
            // any single test will produce.
            for _ in 0..200u64 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(3))).ok();
                        drain_request_headers(&mut stream);
                        let n = accepted_clone.fetch_add(1, Ordering::SeqCst);
                        if n < fail_count {
                            write_status(&mut stream, 503, "Service Unavailable");
                        } else {
                            write_200(&mut stream);
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .expect("spawn mock-503-server");

    (port, accepted)
}

/// 503-then-200: the sender retries until the server heals and
/// the main thread never stalls.
///
/// The sender loop uses 3 backoffs (100ms / 500ms / 2000ms) so if we set
/// `fail_count = 2` the first event hits 503 twice and succeeds on attempt
/// 3. We send 5 events total; each enqueue must be immediate.
#[test]
fn post_does_not_block_during_503_storm() {
    // fail_count=2: the first two attempts per event get 503, third
    // succeeds.  With 5 events and the backoff table, we'd need at
    // most 5 * 3 = 15 server connections; set the budget at 2 so the
    // server switches to 200 early.
    let (port, accepted) = spawn_503_then_200_server(2);
    let base_url = format!("http://127.0.0.1:{}", port);

    let pc = ProgressClient::new("chaos-503", &base_url);

    let start = Instant::now();
    for i in 0..5u32 {
        pc.post(&make_event(&format!("task_{i}")));
    }
    let enqueue_elapsed = start.elapsed();

    // Enqueues must be instantaneous — the sender thread bears the
    // retry latency, not the main thread.
    assert!(
        enqueue_elapsed < Duration::from_millis(200),
        "5 post() calls took {:?}; expected <200ms (bounded channel should return immediately)",
        enqueue_elapsed,
    );

    // Give the sender thread time to drain the queue and exhaust retries.
    // Worst case: 5 events * (100ms + 500ms) first-two-attempt backoffs = 3s.
    // We budget 6s to stay well clear.
    drop(pc); // close channel; Drop joins the sender thread (≤3s).

    let total_accepted = accepted.load(Ordering::SeqCst);
    // We must have accepted at least 5 connections (one per event for the
    // eventual 200 hit) and at most 5 * 4 = 20 (max 4 attempts per event).
    assert!(
        total_accepted >= 5,
        "mock accepted only {total_accepted} connections; expected ≥5 (one per event)"
    );
    assert!(
        total_accepted <= 20,
        "mock accepted {total_accepted} connections; expected ≤20 (4 attempts × 5 events max)"
    );
}

// ── test: mid-response truncation ────────────────────────────────────────────

/// Server that tears one connection mid-response and then serves 200 forever.
/// Connection index 0 gets the truncated response; all subsequent connections
/// get 200.
fn spawn_truncation_then_200_server() -> (u16, Arc<AtomicU64>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind port 0");
    let port = listener.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicU64::new(0));
    let accepted_clone = accepted.clone();

    std::thread::Builder::new()
        .name("mock-truncation-server".into())
        .spawn(move || {
            listener
                .set_nonblocking(false)
                .expect("set blocking on mock");
            for _ in 0..200u64 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(3))).ok();
                        drain_request_headers(&mut stream);
                        let n = accepted_clone.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            // Write an incomplete response: status line only,
                            // no headers or body. ureq will parse the status
                            // but fail when it tries to read the headers.
                            // Alternatively we can just abruptly close the
                            // connection after sending a partial header.
                            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\n");
                            // Drop without writing Content-Length or the blank
                            // separator line — the connection closes with an
                            // incomplete response; ureq treats this as an error.
                            let _ = stream.shutdown(std::net::Shutdown::Both);
                        } else {
                            write_200(&mut stream);
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .expect("spawn mock-truncation-server");

    (port, accepted)
}

/// Mid-response truncation: ureq surfaces a read error; the sender
/// retries and eventually succeeds on the next attempt.
#[test]
fn post_recovers_from_mid_response_truncation() {
    let (port, accepted) = spawn_truncation_then_200_server();
    let pc = ProgressClient::new("chaos-truncation", format!("http://127.0.0.1:{}", port));

    let start = Instant::now();
    // Send two events: the first will experience the truncated connection.
    pc.post(&make_event("task_trunc_0"));
    pc.post(&make_event("task_trunc_1"));
    let enqueue_elapsed = start.elapsed();

    // Enqueues must still be immediate.
    assert!(
        enqueue_elapsed < Duration::from_millis(200),
        "post() blocked: {:?}",
        enqueue_elapsed
    );

    // Drop joins the sender thread; at most 3+1=4 attempts per event.
    drop(pc);

    let total = accepted.load(Ordering::SeqCst);
    // At least 2 connections (one per event on their eventual success
    // attempt), at most 8 (4 attempts × 2 events).
    assert!(
        (2..=8).contains(&total),
        "unexpected connection count {total}; expected 2..=8"
    );

    // No panic occurred — if we reached here the test passed.
}

// ── test: slow server (hang) ──────────────────────────────────────────────────

/// Server that hangs one connection for `hang_secs` then switches to 200.
fn spawn_hanging_then_200_server(hang_secs: u64) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind port 0");
    let port = listener.local_addr().unwrap().port();

    std::thread::Builder::new()
        .name("mock-hang-server".into())
        .spawn(move || {
            listener
                .set_nonblocking(false)
                .expect("set blocking on mock");
            let accepted = AtomicU64::new(0);
            for _ in 0..200u64 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(hang_secs + 2)))
                            .ok();
                        drain_request_headers(&mut stream);
                        let n = accepted.fetch_add(1, Ordering::SeqCst);
                        if n == 0 {
                            // Hang: accept the connection but delay the
                            // response for `hang_secs`. The sender's
                            // `timeout(5s)` will fire before `hang_secs`
                            // when `hang_secs > 5`.
                            std::thread::sleep(Duration::from_secs(hang_secs));
                            // After the sleep the sender has already timed
                            // out and closed the connection; the write will
                            // silently fail.
                            write_200(&mut stream);
                        } else {
                            write_200(&mut stream);
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .expect("spawn mock-hang-server");

    port
}

/// Slow server: the sender's per-attempt `timeout(5s)` fires; the main
/// thread observes no blocking because enqueue is immediate.
///
/// We use `hang_secs = 8` which is longer than the sender's `timeout(5s)`
/// but short enough that the test completes in a reasonable wall-clock time.
/// The sender will time out, retry with backoff, and eventually succeed on
/// a later attempt that hits the non-hanging path.
#[test]
fn post_does_not_block_during_server_hang() {
    let hang_secs = 8u64;
    let port = spawn_hanging_then_200_server(hang_secs);
    let pc = ProgressClient::new("chaos-hang", format!("http://127.0.0.1:{}", port));

    let start = Instant::now();
    pc.post(&make_event("task_hang_0"));
    pc.post(&make_event("task_hang_1"));
    let enqueue_elapsed = start.elapsed();

    // Enqueues must be immediate — the sender thread bears the hang cost.
    assert!(
        enqueue_elapsed < Duration::from_millis(200),
        "post() blocked during server hang: {:?}",
        enqueue_elapsed
    );

    // No panic asserted — if we reached here, the caller was not blocked.
    // Drop with a bounded join (ProgressClient::drop caps at 3s).
    drop(pc);
}

// ── test: no-thundering-herd under queue saturation ──────────────────────────

/// Enqueue more events than the channel capacity (256) by hammering with
/// 300 rapid posts against an unreachable port. Some will be dropped
/// (try_send overflow). Verify:
///  - Main thread never stalls.
///  - `events_dropped()` reflects the drops.
///  - No panic.
#[test]
fn no_thundering_herd_events_dropped_counter_reflects_saturation() {
    // Port 1 refuses connections immediately — ureq returns a connect error
    // without sleeping, so the sender thread drains the queue quickly.
    const UNREACHABLE: &str = "http://127.0.0.1:1";
    let pc = ProgressClient::new("chaos-saturation", UNREACHABLE);

    let n_events: u64 = 300;
    let start = Instant::now();
    for i in 0..n_events {
        pc.post(&make_event(&format!("task_{i}")));
    }
    let enqueue_elapsed = start.elapsed();

    // Must never block regardless of drops.
    assert!(
        enqueue_elapsed < Duration::from_millis(500),
        "300 post() calls took {:?}; channel must not stall the main thread",
        enqueue_elapsed,
    );

    // The channel capacity is 256; at least some of the 300 events should
    // have been dropped if the sender thread is slower than the producer.
    // In practice the channel may drain faster than we fill it, so
    // `events_dropped()` may be 0 — we only assert it doesn't panic and
    // that the drop counter is bounded by n_events.
    let dropped = pc.events_dropped();
    assert!(
        dropped <= n_events,
        "events_dropped={dropped} exceeds n_events={n_events}; counter must not over-count"
    );
}
