//! Agent JSON file-size cap.
//!
//! The harness reads several agent-produced files on every dispatch
//! cycle: `result.json`, `state.patch.json`, `error.json`,
//! `WORKFLOW.json`. A compromised or runaway agent that writes a
//! multi-gigabyte JSON blob to one of these paths would OOM the harness
//! process; the underlying `std::fs::read_to_string` reads the whole
//! file into memory up-front. This module provides a single helper that
//! checks the on-disk size BEFORE allocating, so an oversized file
//! returns an error instead of blowing past the resident-set limit.
//!
//! ## Configurable cap
//!
//! The default cap is 100 MiB; override via `SWFC_AGENT_FILE_MAX_MB`
//! (positive integer, megabytes). A value of `0` is rejected as
//! invalid and falls back to the default — the agent shouldn't be
//! producing zero-byte JSON either way.
//!
//! ## Failure mode
//!
//! When the file exceeds the cap, the helper returns an
//! `std::io::Error` with `ErrorKind::InvalidData` so callers using
//! `?` / `with_context` get a single concise error. Conversion to the
//! harness's `anyhow::Error` happens at call sites that already use
//! `.with_context(...)`. The cap check itself is a single
//! `metadata().len()` lookup — cheap and never reads any bytes when
//! oversized.
//!
//! TODO(rust-1.93+): switch to `ErrorKind::FileTooLarge` once MSRV
//! permits — the variant carries the same semantics with a more
//! precise classification.

use std::path::Path;

/// Default cap on agent-produced JSON inputs. 100 MiB is comfortably
/// above any legitimate `WORKFLOW.json` (largest production package
/// Seen at is ~2.5 MiB) but well under what would let a
/// malicious agent OOM the harness on a typical 8 GiB workstation.
pub const DEFAULT_AGENT_FILE_MAX_BYTES: u64 = 100 * 1024 * 1024;

/// Env-var name that overrides the default cap. Documented in
/// `CLAUDE.md` (env-vars table) and `docs/env-vars-reference.md`.
pub const ENV_AGENT_FILE_MAX_MB: &str = "SWFC_AGENT_FILE_MAX_MB";

/// Resolve the active cap from the environment. Out-of-range values
/// (`0`, non-numeric, negative) fall back to the default with a
/// tracing warning — same pattern the harness uses for the other env-
/// var-driven knobs.
pub fn resolve_max_bytes() -> u64 {
    match std::env::var(ENV_AGENT_FILE_MAX_MB) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(mb) if mb > 0 => mb.saturating_mul(1024 * 1024),
            _ => {
                tracing::warn!(
                    raw = %raw,
                    "invalid {}={raw:?}; falling back to default 100 MiB",
                    ENV_AGENT_FILE_MAX_MB,
                );
                DEFAULT_AGENT_FILE_MAX_BYTES
            }
        },
        Err(_) => DEFAULT_AGENT_FILE_MAX_BYTES,
    }
}

/// Read the entirety of `path` into a `String`, but only after
/// confirming the on-disk size is at most `max_bytes`. Returns
/// `std::io::Error` with `ErrorKind::InvalidData` when the on-disk
/// size exceeds `max_bytes`; the caller's existing
/// `with_context(|| format!(...))` chain adds the path-specific
/// suffix. TODO(rust-1.93+): switch to `ErrorKind::FileTooLarge` once
/// MSRV permits — the variant carries the same semantics with a more
/// precise classification.
///
/// The cap check uses `metadata().len()` — one syscall, no reads.
/// Files that pass the cap go through the normal
/// `std::fs::read_to_string` path so behaviour is unchanged for
/// well-behaved agents.
///
/// Note on race: a malicious agent could in principle truncate a file
/// AFTER `metadata()` returns "small" and re-grow it before the read,
/// but the next call would still hit the cap. Atomicity here is best-
/// effort; the goal is to prevent the harness from OOM-ing on a
/// single oversized blob, not to defeat a Time-of-Check-vs-Time-of-Use
/// adversary.
pub fn read_capped(path: &Path, max_bytes: u64) -> std::io::Result<String> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "agent file {} exceeds {} byte cap (size = {} bytes); refusing to read. \
                 Set SWFC_AGENT_FILE_MAX_MB to raise the cap if intentional.",
                path.display(),
                max_bytes,
                metadata.len(),
            ),
        ));
    }
    std::fs::read_to_string(path)
}

/// Convenience: read `path` using the env-resolved cap. The harness
/// production code uses this everywhere instead of plumbing the cap
/// through every call site.
pub fn read_capped_default(path: &Path) -> std::io::Result<String> {
    read_capped(path, resolve_max_bytes())
}

/// Same shape as [`read_capped`] but for binary reads (`fs::read`).
/// Used by call sites that already deserialize bytes (e.g. via
/// `serde_json::from_slice`).
pub fn read_bytes_capped(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > max_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "agent file {} exceeds {} byte cap (size = {} bytes); refusing to read. \
                 Set SWFC_AGENT_FILE_MAX_MB to raise the cap if intentional.",
                path.display(),
                max_bytes,
                metadata.len(),
            ),
        ));
    }
    std::fs::read(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_capped_passes_small_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"{}").unwrap();
        let s = read_capped(&path, 1024).unwrap();
        assert_eq!(s, "{}");
    }

    #[test]
    fn read_capped_rejects_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.json");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(&vec![b'a'; 4096]).unwrap();
        let err = read_capped(&path, 1024).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(
            msg.contains("exceeds 1024 byte cap"),
            "error message missing cap: {}",
            msg
        );
    }

    #[test]
    fn resolve_max_bytes_default_when_unset() {
        // SAFETY: race-free in unit-test context because the lock above
        // is shared with the other env-mutating test below. We unset
        // the var before reading to model the "operator didn't set it"
        // codepath.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::remove_var(ENV_AGENT_FILE_MAX_MB);
        assert_eq!(resolve_max_bytes(), DEFAULT_AGENT_FILE_MAX_BYTES);
    }

    #[test]
    fn resolve_max_bytes_honours_env_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var(ENV_AGENT_FILE_MAX_MB, "50");
        assert_eq!(resolve_max_bytes(), 50 * 1024 * 1024);
        std::env::remove_var(ENV_AGENT_FILE_MAX_MB);
    }

    #[test]
    fn resolve_max_bytes_rejects_zero() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var(ENV_AGENT_FILE_MAX_MB, "0");
        assert_eq!(resolve_max_bytes(), DEFAULT_AGENT_FILE_MAX_BYTES);
        std::env::remove_var(ENV_AGENT_FILE_MAX_MB);
    }

    #[test]
    fn resolve_max_bytes_rejects_garbage() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        std::env::set_var(ENV_AGENT_FILE_MAX_MB, "garbage");
        assert_eq!(resolve_max_bytes(), DEFAULT_AGENT_FILE_MAX_BYTES);
        std::env::remove_var(ENV_AGENT_FILE_MAX_MB);
    }

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
