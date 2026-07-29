//! Tree-copy helpers that ship repo-side library trees (Python +
//! R plotting; install-proxy shims) into the emitted package's
//! `runtime/` subtree.
//!
//! Each `copy_*` function pairs with a `locate_*_src`
//! sibling that walks up from CWD until it finds the source tree
//! (or honors a `ECAA_*_LIB` env-var override). All three copy
//! flows are idempotent — they remove any stale destination before
//! re-copying so amendment re-emits overwrite cleanly. Each soft
//! no-ops when its source tree isn't present so non-bio test
//! packages emit without dragging the repo layout in.
//!
//! `copy_dir_recursive` is the shared underlying directory walker;
//! it skips `__pycache__/`, `tests/`, and `*.pyc` so the emitted
//! tree stays minimal and byte-stable.

use anyhow::{Context, Result};
use std::path::Path;

/// Ship the literature-retrieval helper inside every package. Agent execution
/// and replay can then import the same pinned bytes from `lib/` without a host
/// cache mount or a machine-specific path.
pub(super) fn copy_literature_fetch_helper(package_dir: &Path) -> Result<()> {
    const HELPER: &str = include_str!("../../../../scripts/agent_literature_fetch.py");
    let lib_dir = package_dir.join("lib");
    std::fs::create_dir_all(&lib_dir).context("creating lib dir")?;
    std::fs::write(lib_dir.join("agent_literature_fetch.py"), HELPER)
        .context("writing packaged literature retrieval helper")
}

/// Copy the shared plotting library from the repo's `lib/plotting/` tree
/// into `runtime/plotting/` inside the emitted package so the agent can
/// `from runtime.plotting.core import generate` without any external
/// dependency on the repo checkout. Silently no-ops when the source
/// tree isn't present — keeps tests using `TempDir` package roots from
/// failing when they don't care about figures.
///
/// The library is located by walking up from the binary's CWD (or the
/// ECAA_PLOTTING_LIB override) until a `lib/plotting/core.py` is
/// found. Stops at filesystem root.
pub(super) fn copy_plotting_library(package_dir: &Path) -> Result<()> {
    let Some(src) = locate_plotting_library_src() else {
        return Ok(());
    };
    let dest_parent = package_dir.join("runtime");
    std::fs::create_dir_all(&dest_parent).context("creating runtime dir")?;
    let dest = dest_parent.join("plotting");
    // Idempotent — remove a stale copy before re-emitting. Concurrent
    // emits against the same session no longer race here (per-session
    // mutex in `service/mod.rs::send_turn` serializes emit calls). An
    // amendment re-emit still needs to overwrite, so we can't
    // short-circuit on `dest.exists()`.
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .with_context(|| format!("removing stale plotting dir at {}", dest.display()))?;
    }
    copy_dir_recursive(&src, &dest)
        .with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;
    Ok(())
}

/// Copy the R-side plotting library at `lib/plotting_r/` into
/// `runtime/plotting_r/` inside the emitted package. Mirrors
/// `copy_plotting_library` for the Python side. Soft-skips when the
/// source tree isn't present so non-bio test packages don't fail.
pub(super) fn copy_r_plotting_library(package_dir: &Path) -> Result<()> {
    let Some(src) = locate_r_plotting_library_src() else {
        return Ok(());
    };
    let dest_parent = package_dir.join("runtime");
    std::fs::create_dir_all(&dest_parent).context("creating runtime dir")?;
    let dest = dest_parent.join("plotting_r");
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .with_context(|| format!("removing stale plotting_r dir at {}", dest.display()))?;
    }
    copy_dir_recursive(&src, &dest)
        .with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;
    Ok(())
}

/// Copy the shared literature-contextualization library at
/// `lib/literature/` into `runtime/literature/` inside the emitted
/// package, so the contextualization atom can invoke
/// `python3 -m runtime.literature` instead of an agent hand-writing
/// per-run evidence constants. Mirrors [`copy_plotting_library`];
/// soft-skips when the source tree isn't present.
pub(super) fn copy_literature_library(package_dir: &Path) -> Result<()> {
    let Some(src) = locate_literature_library_src() else {
        return Ok(());
    };
    let dest_parent = package_dir.join("runtime");
    std::fs::create_dir_all(&dest_parent).context("creating runtime dir")?;
    let dest = dest_parent.join("literature");
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .with_context(|| format!("removing stale literature dir at {}", dest.display()))?;
    }
    copy_dir_recursive(&src, &dest)
        .with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;
    Ok(())
}

/// Locate `lib/literature/` by honoring `ECAA_LITERATURE_LIB` first, then
/// walking up from CWD until a `lib/literature/contextualize.py` is found.
pub(super) fn locate_literature_library_src() -> Option<std::path::PathBuf> {
    if let Ok(override_path) = std::env::var("ECAA_LITERATURE_LIB") {
        let p = std::path::PathBuf::from(override_path);
        if p.join("contextualize.py").exists() {
            return Some(p);
        }
    }
    let cwd = std::env::current_dir().ok()?;
    let mut cursor: Option<&Path> = Some(cwd.as_path());
    while let Some(dir) = cursor {
        let candidate = dir.join("lib").join("literature");
        if candidate.join("contextualize.py").exists() {
            return Some(candidate);
        }
        cursor = dir.parent();
    }
    None
}

pub(super) fn locate_r_plotting_library_src() -> Option<std::path::PathBuf> {
    if let Ok(override_path) = std::env::var("ECAA_PLOTTING_R_LIB") {
        let p = std::path::PathBuf::from(override_path);
        if p.join("core.R").exists() {
            return Some(p);
        }
    }
    let cwd = std::env::current_dir().ok()?;
    let mut cursor: Option<&Path> = Some(cwd.as_path());
    while let Some(dir) = cursor {
        let candidate = dir.join("lib").join("plotting_r");
        if candidate.join("core.R").exists() {
            return Some(candidate);
        }
        cursor = dir.parent();
    }
    None
}

/// Copy the install-proxy shim tree from
/// `runtime/install-proxy/` (in the repo) into
/// `<package>/runtime/install-proxy/` so the derived-image build
/// context has the shims when the Dockerfile `COPY`s them. Mirrors
/// `copy_plotting_library`: located by walking up from CWD (or the
/// `ECAA_INSTALL_PROXY_LIB` override) until `_common.py` is found.
/// Soft no-ops when the source tree isn't present (lets test packages
/// emit without the shim tree on disk).
///
/// Skips `__pycache__/` and `tests/` via `copy_dir_recursive` so the
/// emitted tree stays minimal and byte-stable. The harness pre-flight
/// (`scripts/build-derived-image.sh`) treats the resulting files as
/// part of the Dockerfile build context and bakes them into
/// `/opt/ecaa-workflow/install-proxy/` inside the image.
pub(super) fn copy_install_proxy(package_dir: &Path) -> Result<()> {
    let Some(src) = locate_install_proxy_src() else {
        return Ok(());
    };
    let dest_parent = package_dir.join("runtime");
    std::fs::create_dir_all(&dest_parent).context("creating runtime dir")?;
    let dest = dest_parent.join("install-proxy");
    // Idempotent — remove a stale copy before re-emitting (mirrors
    // copy_plotting_library). Amendment re-emit overwrites cleanly.
    if dest.exists() {
        std::fs::remove_dir_all(&dest)
            .with_context(|| format!("removing stale install-proxy dir at {}", dest.display()))?;
    }
    copy_dir_recursive(&src, &dest)
        .with_context(|| format!("copying {} to {}", src.display(), dest.display()))?;
    Ok(())
}

pub(super) fn locate_install_proxy_src() -> Option<std::path::PathBuf> {
    if let Ok(override_path) = std::env::var("ECAA_INSTALL_PROXY_LIB") {
        let p = std::path::PathBuf::from(override_path);
        if p.join("_common.py").exists() {
            return Some(p);
        }
    }
    let cwd = std::env::current_dir().ok()?;
    let mut cursor: Option<&Path> = Some(cwd.as_path());
    while let Some(dir) = cursor {
        let candidate = dir.join("runtime").join("install-proxy");
        if candidate.join("_common.py").exists() {
            return Some(candidate);
        }
        cursor = dir.parent();
    }
    None
}

fn locate_plotting_library_src() -> Option<std::path::PathBuf> {
    if let Ok(override_path) = std::env::var("ECAA_PLOTTING_LIB") {
        let p = std::path::PathBuf::from(override_path);
        if p.join("core.py").exists() {
            return Some(p);
        }
    }
    let cwd = std::env::current_dir().ok()?;
    let mut cursor: Option<&Path> = Some(cwd.as_path());
    while let Some(dir) = cursor {
        let candidate = dir.join("lib").join("plotting");
        if candidate.join("core.py").exists() {
            return Some(candidate);
        }
        cursor = dir.parent();
    }
    None
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest).with_context(|| format!("creating {}", dest.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry.context("reading dir entry")?;
        let src_path = entry.path();
        let name = match src_path.file_name() {
            Some(n) => n.to_owned(),
            None => continue,
        };
        // Skip test artifacts + __pycache__ / .pytest_cache — keep the emitted
        // tree minimal so the package stays reproducible (stray pytest tooling
        // state must never leak into an emitted package).
        let name_str = name.to_string_lossy();
        if name_str == "__pycache__"
            || name_str == ".pytest_cache"
            || name_str == "tests"
            || name_str.ends_with(".pyc")
        {
            continue;
        }
        let dest_path = dest.join(&name);
        let file_type = entry.file_type().context("reading file type")?;
        if file_type.is_dir() {
            copy_dir_recursive(&src_path, &dest_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dest_path).with_context(|| {
                format!("copying {} -> {}", src_path.display(), dest_path.display())
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::copy_dir_recursive;
    use tempfile::TempDir;

    #[test]
    fn copy_dir_recursive_skips_test_and_cache_artifacts() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();
        let s = src.path();
        // Keepers.
        std::fs::write(s.join("core.py"), "x").unwrap();
        std::fs::create_dir_all(s.join("stages")).unwrap();
        std::fs::write(s.join("stages/de.py"), "y").unwrap();
        // Skips: pytest tooling state must never leak into an emitted package.
        std::fs::create_dir_all(s.join("__pycache__")).unwrap();
        std::fs::write(s.join("__pycache__/core.cpython-311.pyc"), "b").unwrap();
        std::fs::create_dir_all(s.join(".pytest_cache/v/cache")).unwrap();
        std::fs::write(s.join(".pytest_cache/v/cache/lastfailed"), "{}").unwrap();
        std::fs::create_dir_all(s.join("tests")).unwrap();
        std::fs::write(s.join("tests/test_core.py"), "t").unwrap();
        std::fs::write(s.join("stale.pyc"), "p").unwrap();

        let d = dst.path().join("out");
        copy_dir_recursive(s, &d).unwrap();

        assert!(d.join("core.py").exists(), "real source copied");
        assert!(d.join("stages/de.py").exists(), "nested source copied");
        assert!(!d.join("__pycache__").exists(), "__pycache__ skipped");
        assert!(!d.join(".pytest_cache").exists(), ".pytest_cache skipped");
        assert!(!d.join("tests").exists(), "tests/ skipped");
        assert!(!d.join("stale.pyc").exists(), "*.pyc skipped");
    }
}
