//! F20 — gate the cross-language plotting-library version sync.
//!
//! Both `lib/plotting/core.py` and `lib/plotting_r/core.R` read their
//! version constant from a shared `lib/plotting/VERSION` file. This
//! test asserts:
//!
//! 1. The shared file exists and is non-empty.
//! 2. The Python loader resolves its `__version__` from VERSION.
//! 3. The R loader resolves `ECAA_PLOTTING_R_VERSION` from VERSION
//! (path search lives in `.ecaa_read_shared_version()` inline).
//!
//! Drift detection: if a developer hard-codes a different version
//! string in either file, the corresponding regex below fails.

use std::fs;
use std::path::Path;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

#[test]
fn shared_version_file_exists_and_is_nonempty() {
    let path = repo_root().join("lib/plotting/VERSION");
    let body =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let trimmed = body.trim();
    assert!(
        !trimmed.is_empty(),
        "lib/plotting/VERSION must contain a single semver-shaped line; got empty"
    );
    // Lightweight semver shape — `M.m.p` with optional pre-release tag.
    let re = regex::Regex::new(r"^\d+\.\d+\.\d+([+-][A-Za-z0-9.-]+)?$").unwrap();
    assert!(
        re.is_match(trimmed),
        "lib/plotting/VERSION should be semver-shaped, got {trimmed:?}"
    );
}

#[test]
fn python_core_reads_from_shared_version() {
    let path = repo_root().join("lib/plotting/core.py");
    let body =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    assert!(
        body.contains("_read_shared_version"),
        "lib/plotting/core.py must define _read_shared_version() (drift gate F20)"
    );
    assert!(
        body.contains("__version__ = _read_shared_version()"),
        "lib/plotting/core.py must assign __version__ from the shared VERSION (drift gate F20)"
    );
    assert!(
        !body.contains("__version__ = \"1."),
        "lib/plotting/core.py must not hard-code __version__ — read from VERSION instead"
    );
}

#[test]
fn r_core_reads_from_shared_version() {
    let path = repo_root().join("lib/plotting_r/core.R");
    let body =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    assert!(
        body.contains(".ecaa_read_shared_version"),
        "lib/plotting_r/core.R must define .ecaa_read_shared_version() (drift gate F20)"
    );
    assert!(
        body.contains("ECAA_PLOTTING_R_VERSION <- .ecaa_read_shared_version()"),
        "lib/plotting_r/core.R must assign ECAA_PLOTTING_R_VERSION from the shared VERSION"
    );
}
