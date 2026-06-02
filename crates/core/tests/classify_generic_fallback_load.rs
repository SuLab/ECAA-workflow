//! Load-time generic-fallback invariant (core-04).
//!
//! `Classifier::classify` selects a keyword-less modality as the fallback
//! when no keyword-bearing modality scores. That used to be guarded only
//! by config convention with a runtime `.expect()` deep inside
//! `classify`. The loader now validates the invariant up front: a
//! modality registry that carries no keyword-less (`generic_omics`)
//! manifest must fail `Classifier::load` with a clear error instead of
//! producing a classifier that panics on out-of-vocabulary intake.
//!
//! Auto-discovered as its own integration-test crate (top-level
//! `tests/*.rs` file; no `[[test]]` entry needed).

use ecaa_workflow_core::classify::Classifier;
use std::path::{Path, PathBuf};

fn repo_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

/// Build a temp config layout: `<tmp>/modality-keywords.yaml` plus a
/// `<tmp>/modalities/` dir seeded with the real `_modality.schema.json`
/// and one keyword-bearing modality manifest. `include_generic` adds a
/// keyword-less `generic_omics.yaml` fallback.
fn make_config(include_generic: bool) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Cross-cutting keyword file (modalities block intentionally absent —
    // the registry under `modalities/` is authoritative).
    std::fs::write(
        tmp.path().join("modality-keywords.yaml"),
        "method_keywords: []\norganism_keywords: []\ndata_source_patterns: []\n",
    )
    .expect("write modality-keywords.yaml");

    let modalities_dir = tmp.path().join("modalities");
    std::fs::create_dir_all(&modalities_dir).expect("mkdir modalities");
    let schema_src = repo_config().join("modalities/_modality.schema.json");
    std::fs::copy(&schema_src, modalities_dir.join("_modality.schema.json"))
        .expect("copy modality schema");

    // One keyword-bearing modality (no empty-keyword fallback here).
    std::fs::write(
        modalities_dir.join("bulk_rnaseq.yaml"),
        r#"
schema_version: "0.1"
id: bulk_rnaseq
display_name: Bulk RNA-seq
keywords: ["rna seq", "bulk rnaseq", "differential expression"]
edam_topic: "topic:3170"
edam_operation: "operation:3680"
"#,
    )
    .expect("write bulk_rnaseq manifest");

    if include_generic {
        std::fs::write(
            modalities_dir.join("generic_omics.yaml"),
            r#"
schema_version: "0.1"
id: generic_omics
display_name: Generic Omics
keywords: []
edam_topic: "topic:3391"
edam_operation: "operation:2945"
"#,
        )
        .expect("write generic_omics manifest");
    }

    tmp
}

#[test]
fn load_rejects_registry_without_generic_fallback() {
    let tmp = make_config(/* include_generic */ false);
    let path = tmp.path().join("modality-keywords.yaml");
    let err = Classifier::load(&path).expect_err("load must reject a fallback-less registry");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("generic-fallback") || msg.contains("empty `keywords:`"),
        "error should explain the missing generic fallback, got: {msg}"
    );
}

#[test]
fn load_accepts_registry_with_generic_fallback() {
    let tmp = make_config(/* include_generic */ true);
    let path = tmp.path().join("modality-keywords.yaml");
    let classifier = Classifier::load(&path).expect("load must accept a registry with a fallback");

    // Out-of-vocabulary intake routes to the generic fallback without
    // panicking (the path that previously hit the `.expect()`).
    let result = classifier.classify("xyzzy plugh nothing matches here");
    assert_eq!(
        result.modality, "generic_omics",
        "unmatched intake must route to the generic fallback"
    );
}
