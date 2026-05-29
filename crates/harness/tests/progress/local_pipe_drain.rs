//! Regression: a child can write more than a pipe buffer to stderr while
//! keeping stdout silent. The production local executor must drain both
//! pipes concurrently so the child is not blocked on stderr while the
//! parent waits for stdout EOF.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn drain_concurrently(
    mut child: std::process::Child,
) -> (String, String, std::process::ExitStatus) {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut h) = stdout {
            let _ = h.read_to_string(&mut buf);
        }
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(mut h) = stderr {
            let _ = h.read_to_string(&mut buf);
        }
        buf
    });

    let status = child.wait().expect("wait");
    let out = stdout_thread.join().unwrap_or_default();
    let err = stderr_thread.join().unwrap_or_default();
    (out, err, status)
}

#[test]
fn concurrent_drain_handles_stderr_flood() {
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg("head -c 262144 /dev/zero >&2; exit 0")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let child = cmd.spawn().expect("spawn");
    let start = Instant::now();
    let (out, err, status) = drain_concurrently(child);
    let elapsed = start.elapsed();

    assert!(status.success(), "child must exit cleanly");
    assert!(out.is_empty(), "stdout was redirected to /dev/null");
    assert!(err.len() >= 200_000, "stderr should be near 256 KiB");
    assert!(
        elapsed < Duration::from_secs(5),
        "concurrent drain must complete quickly; took {elapsed:?}"
    );
}
