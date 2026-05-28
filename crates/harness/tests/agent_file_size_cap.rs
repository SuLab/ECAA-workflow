//! Integration tests for the agent-file size cap
//! plumbed through `ecaa_workflow_harness::ecaa_io::read_capped`.
//!
//! Validates that:
//! - A small file passes the cap and returns its contents verbatim.
//! - A file larger than the cap is rejected before any read happens.
//! - The convenience env-resolved variant honours
//!   `ECAA_AGENT_FILE_MAX_MB`.
//! - The 200 MiB result.json scenario described in the security
//!   remediation plan is refused at the default cap (100 MiB) — the
//!   harness would historically have read this into memory and OOM'd
//!   on the JSON parse.

use ecaa_workflow_harness::ecaa_io::{
    read_bytes_capped, read_capped, read_capped_default, ENV_AGENT_FILE_MAX_MB,
};
use std::io::Write;

#[test]
fn small_file_is_returned_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.json");
    std::fs::write(&path, b"{\"metric\":42}").unwrap();
    let got = read_capped(&path, 1024).unwrap();
    assert_eq!(got, "{\"metric\":42}");
}

#[test]
fn oversized_file_is_refused_before_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.json");
    // Write 4 KiB; cap at 1 KiB.
    std::fs::write(&path, vec![b'x'; 4096]).unwrap();
    let err = read_capped(&path, 1024).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

/// Builds a 200 MiB result.json on disk and verifies the harness's
/// capped reader refuses it. The default cap is 100 MiB so this is
/// exactly the scenario the security remediation called out.
///
/// Heads up: the file is 200 MiB on disk. The tempdir gets removed at
/// drop so the test leaves nothing behind, but the runner must have
/// at least 256 MiB of free space in the system temp dir.
#[test]
fn two_hundred_megabyte_result_json_refused_at_default_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("result.json");
    // Stream the write in 1 MiB chunks so the test itself doesn't
    // allocate a 200 MiB buffer up front.
    let mut f = std::fs::File::create(&path).unwrap();
    let chunk = vec![b'x'; 1024 * 1024];
    for _ in 0..200 {
        f.write_all(&chunk).unwrap();
    }
    f.sync_all().unwrap();
    drop(f);

    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::remove_var(ENV_AGENT_FILE_MAX_MB);
    let err = read_capped_default(&path).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    let msg = err.to_string();
    assert!(
        msg.contains("byte cap"),
        "expected cap-related error, got {msg}"
    );
}

#[test]
fn env_override_lifts_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("medium.json");
    // Build a 2 MiB file.
    std::fs::write(&path, vec![b'x'; 2 * 1024 * 1024]).unwrap();

    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    std::env::set_var(ENV_AGENT_FILE_MAX_MB, "4");
    let got = read_capped_default(&path);
    std::env::remove_var(ENV_AGENT_FILE_MAX_MB);

    let bytes = got.expect("4 MiB cap should accept 2 MiB file");
    assert_eq!(bytes.len(), 2 * 1024 * 1024);
}

#[test]
fn read_bytes_capped_respects_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("contract.json");
    std::fs::write(&path, vec![b'x'; 4096]).unwrap();
    let err = read_bytes_capped(&path, 1024).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
}

/// Process-wide env lock so the cap-override tests don't race other
/// tests in this binary that read `ECAA_AGENT_FILE_MAX_MB`.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
