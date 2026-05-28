//! Workspace-wide filesystem helpers. Consolidates the
//! read-with-path-context pattern and the atomic-write footgun behind
//! one set of named helpers so every call site surfaces uniform error
//! chains and durable rename semantics.
//!
//! The module is in active use across the workspace: `crates/core`
//! (atom + intake-port + registry loaders, provenance scrub, emitter
//! reads + every package-surface write), `crates/harness` (executor
//! sizing/per-atom-image/overrides/aws/slurm pre-flight + ProgressClient
//! sidecar), `crates/server` (git keys, remediation/blocker writes),
//! `crates/conversation` (transitions runtime-prereqs write),
//! `crates/eval-adapters` (scorer prompt template), and
//! `crates/cli` (migrate-sessions).
//!
//! The sync `atomic_write_bytes_sync` here is the stdlib-only sibling of
//! `crates/conversation/src/persistence.rs::atomic_write_bytes_to`. The
//! conversation crate's tokio variant stays as-is for async hot paths;
//! this helper is for the harness + cli + emitter (no tokio) write sites.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use std::path::Path;

/// Read `path` to a `String` with a `with_context(...)` annotation that
/// includes the path on failure.
pub fn read_to_string_ctx(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

/// Read `path`, parse as JSON, and deserialize into `T`. Both stages
/// surface the path in their error context.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let s = read_to_string_ctx(path)?;
    serde_json::from_str(&s).with_context(|| format!("parsing JSON from {}", path.display()))
}

/// Read `path`, parse as YAML, and deserialize into `T`. Both stages
/// surface the path in their error context.
pub fn read_yaml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let s = read_to_string_ctx(path)?;
    serde_yml::from_str(&s).with_context(|| format!("parsing YAML from {}", path.display()))
}

/// Read `path` as YAML and re-encode as a `serde_json::Value`. Convenient
/// for downstream JSON Schema validation (jsonschema accepts only
/// `serde_json::Value`).
pub fn read_yaml_as_json(path: &Path) -> Result<serde_json::Value> {
    let v: serde_yml::Value = read_yaml(path)?;
    serde_json::to_value(v)
        .with_context(|| format!("yaml->json conversion failed for {}", path.display()))
}

/// Recursively create the parent directory of `path` if it doesn't exist.
/// No-op when `path` has no parent component (e.g. relative file in CWD).
pub fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        // `Path::parent()` returns `Some("")` for a bare filename. Calling
        // `create_dir_all("")` errors on some platforms; guard against the
        // empty parent so this helper is safe on any path.
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating parent {}", parent.display()))?;
        }
    }
    Ok(())
}

/// Sync sibling of
/// `crates/conversation/src/persistence.rs::atomic_write_bytes_to`.
/// Stdlib-only; no tokio. Does the full crash-safe dance:
/// write to `.<uuid>.tmp` -> fsync file -> rename -> best-effort fsync
/// parent dir.
///
/// The parent-directory fsync is best-effort: some filesystems (notably
/// older NFS, fuse layers, tmpfs in some kernels) reject `sync_all` on a
/// directory file descriptor. The plan accepts the trade-off because the
/// alternatives — hard-fail on rare filesystems, or skip parent fsync
/// entirely — are both worse than "durable on every mainstream Linux
/// filesystem".
pub fn atomic_write_bytes_sync(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::fs::{File, OpenOptions};
    use std::io::Write;

    ensure_parent_dir(path).map_err(std::io::Error::other)?;

    // Use a per-write UUID in the temp suffix so concurrent writers to the
    // same final path don't trample each other's `.tmp`. Mirrors the
    // session-store pattern in `persistence.rs`.
    let tmp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));

    // Write + fsync the file. Drop the handle before rename so Windows-style
    // open-file locking doesn't surprise us if this crate is ever ported.
    {
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }

    // Atomic rename.
    std::fs::rename(&tmp, path)?;

    // fsync the parent dir to durably record the rename. Best-effort; some
    // filesystems don't support directory sync_all.
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq, schemars::JsonSchema)]
    struct Sample {
        name: String,
        count: u32,
    }

    #[test]
    fn read_to_string_ctx_surfaces_path_on_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("does-not-exist.txt");
        let err = read_to_string_ctx(&p).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("does-not-exist.txt"), "got: {msg}");
    }

    #[test]
    fn read_to_string_ctx_returns_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("file.txt");
        std::fs::write(&p, "hello world").unwrap();
        let got = read_to_string_ctx(&p).unwrap();
        assert_eq!(got, "hello world");
    }

    #[test]
    fn read_json_parses_into_struct() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("sample.json");
        std::fs::write(&p, r#"{"name":"alice","count":42}"#).unwrap();
        let got: Sample = read_json(&p).unwrap();
        assert_eq!(
            got,
            Sample {
                name: "alice".to_string(),
                count: 42,
            }
        );
    }

    #[test]
    fn read_json_surfaces_path_on_parse_error() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("bad.json");
        std::fs::write(&p, "this is not json").unwrap();
        let err = read_json::<Sample>(&p).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("bad.json"), "got: {msg}");
        assert!(msg.contains("parsing JSON"), "got: {msg}");
    }

    #[test]
    fn read_yaml_parses_into_struct() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("sample.yaml");
        std::fs::write(&p, "name: bob\ncount: 7\n").unwrap();
        let got: Sample = read_yaml(&p).unwrap();
        assert_eq!(
            got,
            Sample {
                name: "bob".to_string(),
                count: 7,
            }
        );
    }

    #[test]
    fn read_yaml_as_json_converts_correctly() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("doc.yaml");
        std::fs::write(&p, "key: value\nnested:\n  inner: 5\n").unwrap();
        let v = read_yaml_as_json(&p).unwrap();
        assert_eq!(v["key"], "value");
        assert_eq!(v["nested"]["inner"], 5);
    }

    #[test]
    fn ensure_parent_dir_creates_missing_chain() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a/b/c/file.txt");
        assert!(!p.parent().unwrap().exists());
        ensure_parent_dir(&p).unwrap();
        assert!(p.parent().unwrap().is_dir());
    }

    #[test]
    fn ensure_parent_dir_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("file.txt");
        ensure_parent_dir(&p).unwrap();
        ensure_parent_dir(&p).unwrap();
    }

    #[test]
    fn atomic_write_bytes_sync_writes_contents() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("out.bin");
        atomic_write_bytes_sync(&p, b"hello").unwrap();
        let got = std::fs::read(&p).unwrap();
        assert_eq!(got, b"hello");
    }

    #[test]
    fn atomic_write_bytes_sync_creates_parent_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("nested/dirs/out.bin");
        atomic_write_bytes_sync(&p, b"x").unwrap();
        assert!(p.is_file());
    }

    #[test]
    fn atomic_write_bytes_sync_overwrites_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("out.bin");
        std::fs::write(&p, b"old").unwrap();
        atomic_write_bytes_sync(&p, b"new").unwrap();
        let got = std::fs::read(&p).unwrap();
        assert_eq!(got, b"new");
    }

    #[test]
    fn atomic_write_bytes_sync_leaves_no_tmp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("out.bin");
        atomic_write_bytes_sync(&p, b"hello").unwrap();
        // Walk the dir and assert nothing else lingers.
        let mut entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        entries.sort();
        assert_eq!(entries, vec![std::ffi::OsString::from("out.bin")]);
    }
}
