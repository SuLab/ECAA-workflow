//! Audit F4 — modality classifier-coverage gate.
//!
//! This gate walks `config/modalities/*.yaml` (excluding schema files),
//! collects every modality `id:` value, then scans:
//! - `crates/core/tests/` (all `.rs` files, one level only)
//! - `tests/conversation-fixtures/` (one level down: top-level files +
//! files inside direct subdirectories)
//!
//! A modality is considered covered if its `id` string appears in at
//! least one test file as a bare word (exact substring match). When the
//! gate fails it names every uncovered modality and tells the engineer
//! where to add coverage.
//!
//! After the F4 backfill (`modality_classifier_coverage.rs`) all 19
//! keyword-routable modalities must pass.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Collect all modality IDs defined in `config/modalities/*.yaml`,
/// excluding files whose name starts with `_` (schema sidecars).
fn collect_modality_ids() -> BTreeSet<String> {
    let modalities_dir = repo_root().join("config").join("modalities");
    let mut ids = BTreeSet::new();

    let entries = std::fs::read_dir(&modalities_dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", modalities_dir.display(), e));

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip schema files and non-YAML
        if name_str.starts_with('_') || !name_str.ends_with(".yaml") {
            continue;
        }

        let content = std::fs::read_to_string(entry.path())
            .unwrap_or_else(|e| panic!("cannot read {}: {}", entry.path().display(), e));

        // Parse `id: <value>` — first occurrence wins.
        for line in content.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("id:") {
                let id = rest.trim().to_string();
                if !id.is_empty() {
                    ids.insert(id);
                }
                break;
            }
        }
    }

    assert!(
        !ids.is_empty(),
        "no modality IDs found under {}",
        modalities_dir.display()
    );
    ids
}

/// Collect all text that appears in test files we care about.
/// Returns a single concatenated string of every file's content.
fn collect_test_corpus() -> String {
    let root = repo_root();
    let mut corpus = String::new();

    // 1. crates/core/tests/**/*.rs — top level and one subdirectory level
    //    (covers both the old flat layout and the new themed-group layout
    //    where each group lives in tests/<group>/*.rs).
    let core_tests = root.join("crates").join("core").join("tests");
    for entry in std::fs::read_dir(&core_tests)
        .unwrap_or_else(|e| panic!("cannot read {}: {}", core_tests.display(), e))
        .flatten()
    {
        let path = entry.path();
        if path.is_file() {
            if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    corpus.push_str(&text);
                    corpus.push('\n');
                }
            }
        } else if path.is_dir() {
            // Descend one level into group subdirectories
            if let Ok(sub_entries) = std::fs::read_dir(&path) {
                for sub in sub_entries.flatten() {
                    let sub_path = sub.path();
                    if sub_path.is_file()
                        && sub_path.extension().and_then(|e| e.to_str()) == Some("rs")
                    {
                        if let Ok(text) = std::fs::read_to_string(&sub_path) {
                            corpus.push_str(&text);
                            corpus.push('\n');
                        }
                    }
                }
            }
        }
    }

    // 2. tests/conversation-fixtures/ — top level + one subdirectory level
    let fixtures_root = root.join("tests").join("conversation-fixtures");
    if let Ok(top_entries) = std::fs::read_dir(&fixtures_root) {
        for top in top_entries.flatten() {
            if top.path().is_file() {
                if let Ok(text) = std::fs::read_to_string(top.path()) {
                    corpus.push_str(&text);
                    corpus.push('\n');
                }
            } else if top.path().is_dir() {
                // Descend exactly one level
                if let Ok(sub_entries) = std::fs::read_dir(top.path()) {
                    for sub in sub_entries.flatten() {
                        if sub.path().is_file() {
                            if let Ok(text) = std::fs::read_to_string(sub.path()) {
                                corpus.push_str(&text);
                                corpus.push('\n');
                            }
                        }
                    }
                }
            }
        }
    }

    corpus
}

#[test]
fn every_modality_has_at_least_one_test_reference() {
    let ids = collect_modality_ids();
    let corpus = collect_test_corpus();

    let uncovered: BTreeSet<&str> = ids
        .iter()
        .filter(|id| !corpus.contains(id.as_str()))
        .map(String::as_str)
        .collect();

    assert!(
        uncovered.is_empty(),
        "Audit F4: the following modalities have ZERO classifier-test coverage:\n\
         \n\
         {}\n\
         \n\
         Add a #[test] for each in:\n\
         \
         crates/core/tests/modality_classifier_coverage.rs\n\
         \n\
         Each test must call clf.classify(<discriminating prose>) and\n\
         assert_eq!(result.modality, \"<modality_id>\").\n\
         Each prompt should include ≥2 distinct keywords from the\n\
         modality's config/modalities/<id>.yaml keyword list.",
        uncovered
            .iter()
            .map(|s| format!("  - {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
