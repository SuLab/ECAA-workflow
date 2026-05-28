//! Path-jail helper (RC-17) for routes that join URL/body string
//! components into filesystem paths under an emitted package root.
//! Centralizes canonicalize + starts_with checks so we can't have one
//! route forget. Every HTTP write sink that splices URL/body strings
//! into `pkg.join(...)` must go through these helpers.

use std::path::{Path, PathBuf};

/// Error returned when a path component fails the jail check.
#[derive(Debug, thiserror::Error)]
pub enum PathJailError {
    /// Component contains `..`.
    #[error("path component contains '..'")]
    ParentTraversal,
    /// Component is the empty string.
    #[error("path component is empty")]
    Empty,
    /// Component is an absolute path.
    #[error("path component is absolute")]
    Absolute,
    /// Component contains `/` or `\`.
    #[error("path component contains path separator")]
    Separator,
    /// Component contains a NUL byte. Rust filesystem APIs reject NUL
    /// bytes downstream (`File::open` returns `InvalidInput`), but
    /// surfacing the rejection here turns "verifier ran against a
    /// phantom task" into a clean 400, and removes the worry that some
    /// future path-handling code in `crates/conversation` or a plugin
    /// might forward the NUL-bearing string into a less defensive
    /// syscall.
    #[error("path component contains a NUL byte")]
    NulByte,
    /// Canonicalized result escapes the jail root.
    #[error("path escapes the jail root")]
    Escape,
    /// The jail root does not exist or is not a directory.
    #[error("root is not a directory: {0}")]
    BadRoot(PathBuf),
}

/// Join `component` onto `root` and return the joined path, but only
/// if `component` is a single safe path segment. Use this for URL-path
/// and request-body strings (task_id, stage_id, proposal_id, etc.)
/// that the caller is about to splice into a filesystem path.
///
/// Rejects: `..`, empty, absolute paths, embedded `/` or `\`.
/// Does NOT itself touch the filesystem.
#[track_caller]
#[tracing::instrument(skip_all, fields(component = %component))]
#[must_use = "path-jail return must be inspected — dropping the Result re-introduces the RC-17 directory-traversal vulnerability"]
pub fn safe_segment_join(root: &Path, component: &str) -> Result<PathBuf, PathJailError> {
    if component.is_empty() {
        return Err(PathJailError::Empty);
    }
    if component == ".." || component == "." {
        return Err(PathJailError::ParentTraversal);
    }
    if component.contains('/') || component.contains('\\') {
        return Err(PathJailError::Separator);
    }
    if component.contains('\0') {
        return Err(PathJailError::NulByte);
    }
    let candidate = Path::new(component);
    if candidate.is_absolute() {
        return Err(PathJailError::Absolute);
    }
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(PathJailError::ParentTraversal);
    }
    Ok(root.join(component))
}

/// Like `safe_segment_join` but accepts a multi-segment relative path
/// (e.g., `runtime/outputs/foo`). Rejects any `..` component and
/// rejects absolute paths. Use for whitelisted prefixes.
#[track_caller]
#[tracing::instrument(skip_all, fields(relative = %rel.display()))]
#[must_use = "path-jail return must be inspected — dropping the Result re-introduces the RC-17 directory-traversal vulnerability"]
pub fn safe_relative_join(root: &Path, rel: &Path) -> Result<PathBuf, PathJailError> {
    if rel.is_absolute() {
        return Err(PathJailError::Absolute);
    }
    if rel
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(PathJailError::ParentTraversal);
    }
    Ok(root.join(rel))
}

/// Resolve `<pkg>/runtime/outputs/<task_id>` with the same defenses as
/// `safe_segment_join` + `assert_under_root` combined. Returns the
/// joined directory path after verifying the canonicalized longest-existing
/// prefix sits under `pkg`. Use this everywhere a handler accepts a
/// URL-path or body-string `task_id` and needs to read or write under
/// `runtime/outputs`.
#[track_caller]
#[tracing::instrument(skip_all, fields(task_id = %task_id))]
#[must_use = "path-jail return must be inspected — dropping the Result re-introduces the RC-17 directory-traversal vulnerability"]
pub fn runtime_outputs_for_task(pkg: &Path, task_id: &str) -> Result<PathBuf, PathJailError> {
    let base = pkg.join("runtime/outputs");
    let joined = safe_segment_join(&base, task_id)?;
    assert_under_root(pkg, &joined)?;
    Ok(joined)
}

/// After joining and writing/reading, canonicalize-verify that the
/// resulting path is still under `root`. Use as a belt-and-suspenders
/// check before any FS operation on the joined path. `root` must
/// exist on disk.
///
/// The target may not yet exist (e.g., we're about to create it), so
/// this canonicalizes the longest existing prefix of `full`. If the
/// canonicalized prefix sits outside `root_canon`, we reject.
#[track_caller]
#[tracing::instrument(skip_all, fields(child = %full.display()))]
#[must_use = "path-jail return must be inspected — dropping the Result re-introduces the RC-17 directory-traversal vulnerability"]
pub fn assert_under_root(root: &Path, full: &Path) -> Result<(), PathJailError> {
    let root_canon = root
        .canonicalize()
        .map_err(|_| PathJailError::BadRoot(root.to_path_buf()))?;
    // The target may not yet exist (e.g., we're about to create it),
    // so canonicalize the longest existing prefix.
    let mut probe = full.to_path_buf();
    loop {
        if probe.exists() {
            let canon = probe.canonicalize().map_err(|_| PathJailError::Escape)?;
            if !canon.starts_with(&root_canon) {
                return Err(PathJailError::Escape);
            }
            return Ok(());
        }
        if !probe.pop() {
            return Err(PathJailError::Escape);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn safe_segment_rejects_parent_traversal() {
        let root = Path::new("/tmp/pkg");
        assert!(matches!(
            safe_segment_join(root, ".."),
            Err(PathJailError::ParentTraversal)
        ));
        assert!(matches!(
            safe_segment_join(root, "../etc"),
            Err(PathJailError::Separator)
        ));
        assert!(matches!(
            safe_segment_join(root, "foo/../bar"),
            Err(PathJailError::Separator)
        ));
    }

    #[test]
    fn safe_segment_rejects_absolute_paths() {
        let root = Path::new("/tmp/pkg");
        assert!(matches!(
            safe_segment_join(root, "/etc/passwd"),
            Err(PathJailError::Separator)
        ));
    }

    #[test]
    fn safe_segment_rejects_separators() {
        let root = Path::new("/tmp/pkg");
        assert!(matches!(
            safe_segment_join(root, "a/b"),
            Err(PathJailError::Separator)
        ));
        assert!(matches!(
            safe_segment_join(root, "a\\b"),
            Err(PathJailError::Separator)
        ));
    }

    #[test]
    fn safe_segment_rejects_nul_byte() {
        let root = Path::new("/tmp/pkg");
        assert!(matches!(
            safe_segment_join(root, "biological\0interpretation"),
            Err(PathJailError::NulByte)
        ));
        assert!(matches!(
            safe_segment_join(root, "\0"),
            Err(PathJailError::NulByte)
        ));
    }

    #[test]
    fn safe_segment_rejects_empty() {
        let root = Path::new("/tmp/pkg");
        assert!(matches!(
            safe_segment_join(root, ""),
            Err(PathJailError::Empty)
        ));
    }

    #[test]
    fn safe_segment_accepts_normal_task_id() {
        let root = Path::new("/tmp/pkg");
        assert_eq!(
            safe_segment_join(root, "deseq2_de_analysis").unwrap(),
            Path::new("/tmp/pkg/deseq2_de_analysis"),
        );
    }

    #[test]
    fn assert_under_root_rejects_symlink_escape() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir(&pkg).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        // Plant a symlink inside pkg pointing outside.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, pkg.join("link")).unwrap();
        let target = pkg.join("link").join("escape.txt");
        std::fs::write(&target, b"x").unwrap();
        assert!(matches!(
            assert_under_root(&pkg, &target),
            Err(PathJailError::Escape)
        ));
    }

    #[test]
    fn assert_under_root_accepts_legitimate_subpath() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir_all(pkg.join("runtime/outputs/task1")).unwrap();
        let target = pkg.join("runtime/outputs/task1/sme-selection.json");
        std::fs::write(&target, b"{}").unwrap();
        assert_under_root(&pkg, &target).unwrap();
    }

    #[test]
    fn runtime_outputs_for_task_rejects_traversal() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit")).unwrap();
        // `..` alone trips ParentTraversal; `../..` etc. trip Separator because of `/`.
        let err = runtime_outputs_for_task(pkg, "..").unwrap_err();
        assert!(matches!(err, PathJailError::ParentTraversal));
        let err2 = runtime_outputs_for_task(pkg, "../../etc").unwrap_err();
        assert!(matches!(err2, PathJailError::Separator));
    }

    #[test]
    fn runtime_outputs_for_task_resolves_legit() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime/outputs/legit")).unwrap();
        let p = runtime_outputs_for_task(pkg, "legit").unwrap();
        assert_eq!(p, pkg.join("runtime/outputs/legit"));
    }

    #[test]
    fn runtime_outputs_for_task_rejects_symlink_escape() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path().join("pkg");
        std::fs::create_dir_all(pkg.join("runtime/outputs")).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, pkg.join("runtime/outputs/evil")).unwrap();
        let err = runtime_outputs_for_task(&pkg, "evil").unwrap_err();
        assert!(matches!(err, PathJailError::Escape));
    }
}
