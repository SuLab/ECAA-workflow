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
/// Checks four subdirectories:
/// - `conda-envs/`: has a real install iff at least one environment subdir
///   contains a `bin/` directory.
/// - `R-libs/`: has a real install iff at least one package subdir exists
///   (any child directory of `R-libs/` counts as a package).
/// - `pip/`: has a real install iff at least one child entry exists (files or
///   directories inside `pip/` indicate pip-installed content).
/// - `python/`: has a real install iff `python/lib/<any>/site-packages/`
///   exists and contains at least one entry. This covers packages installed
///   into `PYTHONUSERBASE` via `pip install --user` (e.g. pydeseq2).
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
        // any entry means content is present
        if entries.flatten().next().is_some() {
            return true;
        }
    }

    // python/ (PYTHONUSERBASE): a real install has packages under
    // python/lib/<python-version>/site-packages/<package>/. Walk two levels:
    // python/lib/ → version dirs → site-packages/ → check for any entry.
    let python_lib = cache_dir.join("python").join("lib");
    if let Ok(ver_entries) = std::fs::read_dir(&python_lib) {
        for ver_entry in ver_entries.flatten() {
            let site_packages = ver_entry.path().join("site-packages");
            if let Ok(pkg_entries) = std::fs::read_dir(&site_packages) {
                // any entry means a package is installed
                if pkg_entries.flatten().next().is_some() {
                    return true;
                }
            }
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
    #[test]
    fn python_userbase_site_packages_present_means_installs() {
        // Simulate a run that installed ONLY Python packages via pip --user
        // (empty conda-envs and R-libs). The python/lib/<ver>/site-packages/
        // tree must be sufficient to trigger cache_has_installs.
        let t = tempfile::tempdir().unwrap();
        // Explicitly create empty conda-envs and R-libs to confirm they don't
        // trigger the result on their own.
        std::fs::create_dir_all(t.path().join("conda-envs")).unwrap();
        std::fs::create_dir_all(t.path().join("R-libs")).unwrap();
        // Install a package into PYTHONUSERBASE layout.
        std::fs::create_dir_all(
            t.path().join("python/lib/python3.11/site-packages/pydeseq2"),
        )
        .unwrap();
        assert!(cache_has_installs(t.path()));
    }
}
