//! F15 — atom-count drift gate.
//!
//! `config/stage-atoms/*.yaml` is the composer's atom catalog. CLAUDE.md
//! used to hard-code an integer literal ("39 typed atom files") that
//! rotted between releases (was 39, then 45). This test couples the
//! atom-file count to a single in-repo source of truth — the
//! `EXPECTED_STAGE_ATOMS` constant below — so:
//!
//! - Adding an atom YAML without bumping the constant ⇒ this test fails.
//! - Bumping the constant without adding an atom YAML ⇒ this test fails.
//!
//! The fix is to update both in the same change. This test is intentionally
//! SELF-CONTAINED: it depends only on files inside this repository (the
//! `config/stage-atoms/` directory and this constant), so it runs in every
//! checkout — including the OSS repo — and actually guards atom-catalogue-size
//! drift. It used to depend on `.github/ci/expected-test-counts.json`, which is
//! absent from the OSS repo, so it was `#[ignore]`d and never ran.
//! CLAUDE.md no longer carries the integer literal.
//!
//! GUARDS ATOM-CATALOGUE-SIZE DRIFT: `EXPECTED_STAGE_ATOMS` must be bumped
//! INTENTIONALLY in the same change that adds or removes an atom YAML under
//! `config/stage-atoms/`. Do not "fix" a failure by blindly editing this
//! number — a mismatch means the catalogue changed and that change must be
//! deliberate.

use std::fs;
use std::path::Path;

/// Expected number of atom YAMLs under `config/stage-atoms/` (excluding
/// `_`-prefixed partials). Bump this only when atoms are intentionally
/// added or removed.
const EXPECTED_STAGE_ATOMS: usize = 93;

fn repo_root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn count_stage_atoms() -> usize {
    let dir = repo_root().join("config/stage-atoms");
    fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.ends_with(".yaml") && !name.starts_with('_')
        })
        .count()
}

#[test]
fn atom_count_matches_baseline() {
    let actual = count_stage_atoms();
    assert_eq!(
        actual, EXPECTED_STAGE_ATOMS,
        "config/stage-atoms/*.yaml count {actual} differs from expected \
         {EXPECTED_STAGE_ATOMS}. This gate guards atom-catalogue-size drift: \
         if you intentionally added or removed an atom YAML, bump \
         `EXPECTED_STAGE_ATOMS` in this test in the same change — F15."
    );
}
