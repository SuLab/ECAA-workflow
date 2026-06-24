//! Cache-directory resolution and install-presence detection.
//!
//! `resolve_cache_dir` mirrors the agent's runtime cache layout:
//!   `${ECAA_AGENT_CACHE_DIR:-$HOME/.ecaa-workflow/agent-cache}/<ECAA_CHAT_SESSION_ID|global>`
//!
//! `cache_has_installs` returns `true` iff at least one real package/env has
//! been installed into the session cache (i.e., the snapshot would capture
//! something meaningful).

use std::path::{Path, PathBuf};

/// Resolve the per-session agent cache directory.
///
/// Returns `None` only when neither `ECAA_AGENT_CACHE_DIR` nor `HOME` is set
/// (extremely unusual outside of sandboxed tests).
pub fn resolve_cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("ECAA_AGENT_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ecaa-workflow/agent-cache"))
        })?;

    let session = std::env::var("ECAA_CHAT_SESSION_ID")
        .unwrap_or_else(|_| "global".to_owned());

    Some(base.join(session))
}

/// Return `true` iff the cache directory contains real installed content.
///
/// Checks three subdirectories:
/// - `conda-envs/`: has a real install iff at least one environment subdir
///   contains a `bin/` directory.
/// - `R-libs/`: has a real install iff at least one package subdir exists
///   (any child directory of `R-libs/` counts as a package).
/// - `pip/`: has a real install iff at least one child entry exists (files or
///   directories inside `pip/` indicate pip-installed content).
///
/// Structural-only directories (i.e., the subdirs exist but are empty) return
/// `false`.
pub fn cache_has_installs(cache_dir: &Path) -> bool {
    // conda-envs: a real env has a bin/ directory inside it.
    if let Ok(entries) = std::fs::read_dir(cache_dir.join("conda-envs")) {
        for entry in entries.flatten() {
            if entry.path().join("bin").is_dir() {
                return true;
            }
        }
    }

    // R-libs: any package subdir inside R-libs counts as installed.
    if let Ok(entries) = std::fs::read_dir(cache_dir.join("R-libs")) {
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                return true;
            }
        }
    }

    // pip: any child entry (file or directory) inside pip/ indicates content.
    // Unlike conda (which has a canonical `bin/`) or R-libs (which has named
    // package subdirs), pip caches and install trees have no single fixed
    // sub-structure — layout varies by pip version and invocation flags.
    // Therefore the presence of *any* content under `pip/` is sufficient
    // evidence that pip-managed packages have been installed.
    if let Ok(entries) = std::fs::read_dir(cache_dir.join("pip")) {
        for entry in entries.flatten() {
            let _ = entry; // any entry means content is present
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_cache_has_no_installs() {
        let t = tempfile::tempdir().unwrap();
        for d in ["conda-envs", "R-libs", "pip", "apt"] {
            std::fs::create_dir_all(t.path().join(d)).unwrap();
        }
        assert!(!cache_has_installs(t.path()));
    }
    #[test]
    fn conda_env_present_means_installs() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("conda-envs/deseq2_env/bin")).unwrap();
        std::fs::write(t.path().join("conda-envs/deseq2_env/bin/python"), "x").unwrap();
        assert!(cache_has_installs(t.path()));
    }
    #[test]
    fn r_libs_package_present_means_installs() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("R-libs/DESeq2/R")).unwrap();
        assert!(cache_has_installs(t.path()));
    }
    #[test]
    fn pip_content_present_means_installs() {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("pip/site-packages/numpy")).unwrap();
        assert!(cache_has_installs(t.path()));
    }
}
