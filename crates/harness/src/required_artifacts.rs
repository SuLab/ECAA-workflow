//! Required-artifact verification shared by the harness binary and tests.

use anyhow::{anyhow, Context, Result};
use scripps_workflow_core::dag::RequiredArtifact;
use std::path::{Component, Path};

/// Verify that every required artifact exists under
/// `<package>/runtime/outputs/<task_id>/`, is non-empty, and meets its
/// optional minimum size. Returns the declared relative paths that are
/// missing or too small.
///
/// Artifact paths are path-jailed before resolution: absolute paths,
/// parent-directory components, and symlink escapes are rejected.
pub fn verify_required_artifacts(
    package_root: &Path,
    task_id: &str,
    required: &[RequiredArtifact],
) -> Result<Vec<String>> {
    let base = package_root.join("runtime/outputs").join(task_id);
    let mut missing: Vec<String> = Vec::new();
    for entry in required {
        let rel = required_artifact_relative_path(&entry.path)?;
        let full = base.join(rel);
        match std::fs::metadata(&full) {
            Err(_) => missing.push(entry.path.clone()),
            Ok(meta) => {
                let base_canon = base
                    .canonicalize()
                    .with_context(|| format!("canonicalize base {}", base.display()))?;
                let full_canon = full
                    .canonicalize()
                    .with_context(|| format!("canonicalize {}", full.display()))?;
                if !full_canon.starts_with(&base_canon) {
                    return Err(anyhow!(
                        "required artifact escaped task output dir: {} -> {}",
                        entry.path,
                        full_canon.display()
                    ));
                }
                let size = meta.len();
                let min = entry.min_size_bytes.unwrap_or(0);
                if size == 0 || size < min {
                    missing.push(entry.path.clone());
                }
            }
        }
    }
    Ok(missing)
}

fn required_artifact_relative_path(path: &str) -> Result<&Path> {
    let rel = Path::new(path);
    if rel.is_absolute() {
        return Err(anyhow!(
            "required artifact path must be relative, got absolute path: {}",
            path
        ));
    }
    if rel.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(anyhow!(
            "required artifact path must not contain parent directory components: {}",
            path
        ));
    }
    Ok(rel)
}
