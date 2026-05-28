//! Atomic read/write of `runtime/inputs/<task_id>/overrides.json`.
//!
//! The server-side apply-remediation endpoint writes; the executor
//! reads at dispatch time. Atomic-rename semantics so a partial
//! write never lands in front of the executor.

use anyhow::{Context, Result};
use ecaa_workflow_core::remediation::ExecutorOverrides;
use std::path::{Path, PathBuf};

/// Resolve the on-disk path for a task's overrides file. The directory
/// is created lazily by the writer; readers must tolerate absence.
pub fn overrides_path(package: &Path, task_id: &str) -> PathBuf {
    package
        .join("runtime")
        .join("inputs")
        .join(task_id)
        .join("overrides.json")
}

/// Read the overrides for a task. Returns `Ok(None)` when the file is
/// absent (no remediation has been applied yet) — the common case.
/// Returns `Err` only when the file exists but is unreadable / malformed,
/// so the executor can surface a clear diagnostic instead of silently
/// dispatching with the wrong shape.
pub fn read(package: &Path, task_id: &str) -> Result<Option<ExecutorOverrides>> {
    let path = overrides_path(package, task_id);
    if !path.exists() {
        return Ok(None);
    }
    let raw = crate::swfc_io::read_capped(&path, crate::swfc_io::resolve_max_bytes())
        .with_context(|| format!("reading overrides {}", path.display()))?;
    let parsed: ExecutorOverrides = serde_json::from_str(&raw)
        .with_context(|| format!("parsing overrides {}", path.display()))?;
    Ok(Some(parsed))
}

/// Write overrides atomically via `.tmp` + rename. Creates the parent
/// directory if it doesn't yet exist.
pub fn write(package: &Path, task_id: &str, overrides: &ExecutorOverrides) -> Result<()> {
    let path = overrides_path(package, task_id);
    let raw = serde_json::to_string_pretty(overrides).context("serialising overrides")?;
    ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(&path, raw.as_bytes())
        .with_context(|| format!("atomic write overrides at {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::remediation::ResourceTarget;

    #[test]
    fn read_missing_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let out = read(tmp.path(), "missing").unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn write_then_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let ov = ExecutorOverrides {
            resources: Some(ResourceTarget {
                memory_gb: Some(64),
                ..Default::default()
            }),
            ..Default::default()
        };
        write(tmp.path(), "alignment", &ov).unwrap();
        let loaded = read(tmp.path(), "alignment").unwrap().unwrap();
        assert_eq!(loaded.resources.unwrap().memory_gb, Some(64));
    }

    #[test]
    fn read_malformed_errors_loudly() {
        let tmp = tempfile::tempdir().unwrap();
        let p = overrides_path(tmp.path(), "bad");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "not json").unwrap();
        let err = read(tmp.path(), "bad").unwrap_err();
        let s = format!("{:#}", err);
        assert!(s.contains("parsing overrides"), "got: {}", s);
    }

    #[test]
    fn write_is_atomic_no_tmp_left_behind() {
        let tmp = tempfile::tempdir().unwrap();
        let ov = ExecutorOverrides::default();
        write(tmp.path(), "t", &ov).unwrap();
        let dir = tmp.path().join("runtime").join("inputs").join("t");
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["overrides.json"]);
    }
}
