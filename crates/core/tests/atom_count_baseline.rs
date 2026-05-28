//! F15 — atom-count drift gate.
//!
//! `config/stage-atoms/*.yaml` is the composer's atom catalog. CLAUDE.md
//! used to hard-code an integer literal ("39 typed atom files") that
//! rotted between releases (was 39, then 45). This test couples the
//! atom-file count to a single source of truth in
//! `.github/ci/expected-test-counts.json` (a new key
//! `config_yaml_baselines.stage_atoms`), so:
//!
//! - Adding an atom YAML without bumping the baseline ⇒ this test fails.
//! - Bumping the baseline without adding an atom YAML ⇒ this test fails.
//!
//! The fix is to update both in the same PR. CLAUDE.md no longer carries
//! the integer literal.

use serde_json::Value;
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
#[ignore = ".github/ci/expected-test-counts.json baseline not in OSS repo"]
fn atom_count_matches_baseline() {
    let baseline_path = repo_root().join(".github/ci/expected-test-counts.json");
    let body = fs::read_to_string(&baseline_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", baseline_path.display()));
    let parsed: Value = serde_json::from_str(&body).expect("expected-test-counts.json is JSON");

    let baseline = parsed
        .get("config_yaml_baselines")
        .and_then(|v| v.get("stage_atoms"))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| {
            panic!(
                "expected `config_yaml_baselines.stage_atoms` to be a number in \
                 .github/ci/expected-test-counts.json (F15)"
            )
        });

    let actual = count_stage_atoms() as u64;
    assert_eq!(
        actual, baseline,
        "config/stage-atoms/*.yaml count {actual} differs from baseline {baseline}. \
         Update `config_yaml_baselines.stage_atoms` in .github/ci/expected-test-counts.json \
         (or vice versa). CLAUDE.md no longer carries the integer literal — F15."
    );
}
