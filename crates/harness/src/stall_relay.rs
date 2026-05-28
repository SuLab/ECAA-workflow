//! Direct stall-signal relay: copies `stall_rx → POST .../stall-signal`
//! on a dedicated background thread, bypassing the main harness loop.
//!
//! When AWS SSM hangs inside `executor.run_iteration()`, the main loop
//! is blocked and can no longer drain the stall-monitor mpsc channel.
//! Signals queue up unread and the operator loses visibility precisely
//! when they need it most. This module spawns a relay thread that
//! blocks on its own dedicated receiver and fires a direct HTTP POST to
//! `/api/chat/session/:id/stall-signal` for every signal it receives,
//! independently of the main-loop drain.
//!
//! Fan-out approach: the caller creates a **second** `mpsc::channel`
//! and a splitter thread that reads from the original stall-monitor
//! receiver and forwards each `StallSignal` to BOTH the main-loop
//! receiver (unchanged semantics) AND the relay receiver. No changes to
//! `start_stall_monitor` or the `Executor` trait are required.
//!
//! `spawn` is a no-op (returns a dummy handle) when `session_id` or
//! `server_url` is empty so the non-chat harness path (no `--session-id`)
//! stays unaffected.

use crate::executor::stall_monitor::StallSignal;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Wire body sent to `POST .../stall-signal`. Mirrors the four signal
/// variants as flat JSON so the server handler can deserialize without
/// depending on the harness crate.
#[derive(Debug, serde::Serialize)]
pub struct StallSignalBody {
    /// Task that triggered the stall signal.
    pub task_id: String,
    /// Discriminator: "cpu_starvation" | "memory_pressure" |
    /// "gpu_idle_during_training" | "runtime_over_expected".
    pub kind: String,
    /// Human-readable summary of the measurements captured by the
    /// signal variant.
    pub measurements: serde_json::Value,
    /// Recommended recovery: "retry" | "resize" | "abort".
    pub suggested_action: String,
}

impl StallSignalBody {
    fn from_signal(signal: &StallSignal) -> Self {
        use scripps_workflow_core::blocker::StallAction;
        let task_id = signal.task_id().to_string();
        let suggested_action = match signal.suggested_action() {
            StallAction::Retry => "retry",
            StallAction::Resize => "resize",
            StallAction::Abort => "abort",
        }
        .to_string();
        let (kind, measurements) = match signal {
            StallSignal::CpuStarvation {
                avg_cpu_pct,
                window_mins,
                ..
            } => (
                "cpu_starvation",
                serde_json::json!({
                    "avg_cpu_pct": avg_cpu_pct,
                    "window_mins": window_mins,
                }),
            ),
            StallSignal::MemoryPressure {
                pct, window_mins, ..
            } => (
                "memory_pressure",
                serde_json::json!({
                    "pct": pct,
                    "window_mins": window_mins,
                }),
            ),
            StallSignal::GpuIdleDuringTraining { window_mins, .. } => (
                "gpu_idle_during_training",
                serde_json::json!({
                    "window_mins": window_mins,
                }),
            ),
            StallSignal::RuntimeOverExpected {
                actual_secs,
                expected_secs,
                ..
            } => (
                "runtime_over_expected",
                serde_json::json!({
                    "actual_secs": actual_secs,
                    "expected_secs": expected_secs,
                }),
            ),
        };
        Self {
            task_id,
            kind: kind.to_string(),
            measurements,
            suggested_action,
        }
    }
}

/// Spawn the relay thread. Returns immediately; the thread blocks on
/// `relay_rx` and POST each signal to the server's
/// `/api/chat/session/<session_id>/stall-signal` endpoint.
///
/// `package_root` is recorded for future structured-log enrichment
/// (e.g. the package-local stall-signals.jsonl sidecar written by
/// `stall_monitor::append_stall_signal_record`). Unused by the relay
/// itself beyond the tracing context.
///
/// Returns a `JoinHandle` the caller can drop — the thread exits when
/// `relay_rx` is closed (i.e. when the splitter thread exits or is
/// dropped). Best-effort: if the caller drops the handle without
/// joining, the relay thread is detached and exits with the process.
pub fn spawn(
    _package_root: PathBuf,
    session_id: String,
    server_url: String,
    relay_rx: Receiver<StallSignal>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("stall-signal-relay".into())
        .spawn(move || {
            relay_loop(session_id, server_url, relay_rx);
        })
        .expect("spawn stall-signal-relay thread")
}

fn relay_loop(session_id: String, server_url: String, relay_rx: Receiver<StallSignal>) {
    if session_id.is_empty() || server_url.is_empty() {
        // No session configured — drain silently and exit when closed.
        while relay_rx.recv().is_ok() {}
        return;
    }

    let url = format!(
        "{}/api/chat/session/{}/stall-signal",
        server_url.trim_end_matches('/'),
        session_id
    );
    let auth_token = std::env::var("SWFC_SERVER_AUTH_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build();

    while let Ok(signal) = relay_rx.recv() {
        let body = StallSignalBody::from_signal(&signal);
        let body_value = match serde_json::to_value(&body) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    error = %e,
                    "stall-relay: failed to serialize signal body"
                );
                continue;
            }
        };

        let mut req = agent.post(&url);
        if let Some(tok) = auth_token.as_deref() {
            req = req.set("Authorization", &format!("Bearer {tok}"));
        }
        match req.send_json(&body_value) {
            Ok(_) => {
                tracing::debug!(
                    session_id = %session_id,
                    task_id = %body.task_id,
                    kind = %body.kind,
                    "stall-relay: direct POST succeeded"
                );
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %session_id,
                    task_id = %body.task_id,
                    kind = %body.kind,
                    error = %e,
                    "stall-relay: direct POST failed (continuing)"
                );
            }
        }
    }

    tracing::debug!(session_id = %session_id, "stall-relay: channel closed, exiting");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::stall_monitor::StallSignal;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    /// Spawn a minimal one-shot TCP server that accepts one connection,
    /// reads the HTTP POST, records `(path, body)`, and replies 204.
    /// Returns `(port, captured)` where `captured` is a shared vec of
    /// `(path, body)` pairs appended by each accepted connection.
    fn spawn_mock_server(
        reply_status: u16,
        max_requests: usize,
    ) -> (u16, Arc<Mutex<Vec<(String, String)>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind port 0");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).expect("set non-blocking");
        let captured: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = captured.clone();
        thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut served = 0;
            while Instant::now() < deadline && served < max_requests {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
                        let mut reader = BufReader::new(&mut stream);
                        let mut first_line = String::new();
                        let _ = reader.read_line(&mut first_line);
                        let parts: Vec<&str> = first_line.split_whitespace().collect();
                        let path = parts.get(1).copied().unwrap_or("").to_string();
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
                            if let Some(v) = trimmed
                                .strip_prefix("Content-Length:")
                                .or_else(|| trimmed.strip_prefix("content-length:"))
                            {
                                content_length = v.trim().parse().unwrap_or(0);
                            }
                        }
                        let mut body_bytes = vec![0u8; content_length];
                        if content_length > 0 {
                            let _ = reader.read_exact(&mut body_bytes);
                        }
                        captured_clone
                            .lock()
                            .unwrap()
                            .push((path, String::from_utf8_lossy(&body_bytes).to_string()));
                        let status_line = if reply_status == 204 {
                            "HTTP/1.1 204 No Content\r\n"
                        } else {
                            "HTTP/1.1 500 Internal Server Error\r\n"
                        };
                        let _ = stream.write_all(
                            format!(
                                "{}Content-Length: 0\r\nConnection: close\r\n\r\n",
                                status_line
                            )
                            .as_bytes(),
                        );
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        served += 1;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => return,
                }
            }
        });
        (port, captured)
    }

    #[test]
    fn relay_posts_signal_to_endpoint() {
        let (port, captured) = spawn_mock_server(204, 1);
        let (relay_tx, relay_rx) = std::sync::mpsc::channel::<StallSignal>();

        let session_id = "test-session-relay".to_string();
        let server_url = format!("http://127.0.0.1:{}", port);
        let handle = spawn(
            std::path::PathBuf::from("/tmp/test-pkg"),
            session_id.clone(),
            server_url,
            relay_rx,
        );

        relay_tx
            .send(StallSignal::MemoryPressure {
                task_id: "alignment".into(),
                pct: 93.5,
                window_mins: 5,
            })
            .unwrap();
        // Close the sender so the relay thread exits after draining.
        drop(relay_tx);

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !captured.lock().unwrap().is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = handle.join();

        let log = captured.lock().unwrap();
        assert_eq!(
            log.len(),
            1,
            "exactly one POST captured; got {:?}",
            log.len()
        );
        let (path, body) = &log[0];
        assert!(
            path.contains(&session_id),
            "POST path must include session_id; got {path}"
        );
        assert!(
            path.ends_with("/stall-signal"),
            "POST path must end with /stall-signal; got {path}"
        );
        let parsed: serde_json::Value =
            serde_json::from_str(body).expect("body should be valid JSON");
        assert_eq!(parsed["task_id"], "alignment");
        assert_eq!(parsed["kind"], "memory_pressure");
        assert_eq!(
            parsed["suggested_action"], "resize",
            "memory pressure → resize"
        );
        assert!(
            parsed["measurements"]["pct"].as_f64().is_some(),
            "pct field must be present in measurements"
        );
    }

    #[test]
    fn relay_continues_on_post_failure() {
        // Server returns 500 on the first request, then 204 on the second.
        // The relay must not crash; it must process both signals.
        let (port, captured) = spawn_mock_server(500, 2);
        let (relay_tx, relay_rx) = std::sync::mpsc::channel::<StallSignal>();

        let server_url = format!("http://127.0.0.1:{}", port);
        let handle = spawn(
            std::path::PathBuf::from("/tmp/test-pkg2"),
            "test-session-err".into(),
            server_url,
            relay_rx,
        );

        relay_tx
            .send(StallSignal::CpuStarvation {
                task_id: "t1".into(),
                avg_cpu_pct: 1.2,
                window_mins: 30,
            })
            .unwrap();
        relay_tx
            .send(StallSignal::RuntimeOverExpected {
                task_id: "t2".into(),
                actual_secs: 3600,
                expected_secs: 1200,
            })
            .unwrap();
        drop(relay_tx);

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if captured.lock().unwrap().len() >= 2 {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = handle.join();

        // The relay must have attempted both POSTs without panicking.
        let log = captured.lock().unwrap();
        assert_eq!(
            log.len(),
            2,
            "relay must attempt POST for each signal regardless of prior 500; got {}",
            log.len()
        );
        let kinds: Vec<String> = log
            .iter()
            .map(|(_, body)| {
                let v: serde_json::Value = serde_json::from_str(body).unwrap();
                v["kind"].as_str().unwrap_or("").to_string()
            })
            .collect();
        assert!(
            kinds.iter().any(|k| k == "cpu_starvation"),
            "first signal kind not found; got: {:?}",
            kinds
        );
        assert!(
            kinds.iter().any(|k| k == "runtime_over_expected"),
            "second signal kind not found; got: {:?}",
            kinds
        );
    }
}
