//! Adding a 9th modality is a single YAML drop in
//! `config/modalities/`, not a Rust code edit.
//!
//! This test proves that `ModalityRegistry::load_from_dir` accepts
//! a synthetic modality authored only in YAML and exposes its
//! metadata to consumers without any code change. Mirrors the
//! `project_class_registry_synthetic_class` test pattern from S4.

use ecaa_workflow_core::modality_registry::ModalityRegistry;
use std::path::Path;

#[test]
fn synthetic_9th_modality_loads_from_yaml_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Copy the schema sidecar so the loader passes validation.
    let schema_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/modalities/_modality.schema.json");
    std::fs::copy(&schema_src, tmp.path().join("_modality.schema.json")).expect("schema copy");

    // Drop a 9th modality YAML — no Rust edits required for the
    // registry to accept it.
    std::fs::write(
        tmp.path().join("spatial_transcriptomics.yaml"),
        r#"
schema_version: "0.1"
id: spatial_transcriptomics
display_name: Spatial Transcriptomics
keywords: ["visium", "spatial", "10x spatial", "merfish", "stereo-seq"]
edam_topic: "topic:3308"
edam_operation: "operation:3223"
taxonomy_path: stage-taxonomies/spatial-transcriptomics.yaml
archetype_id: spatial_transcriptomics
fixture_corpus_rows: [spatial_visium_human, spatial_merfish_mouse]
"#,
    )
    .expect("write synthetic modality");

    let reg = ModalityRegistry::load_from_dir(tmp.path()).expect("registry load");
    let m = reg
        .get("spatial_transcriptomics")
        .expect("modality must register");
    assert_eq!(m.id, "spatial_transcriptomics");
    assert_eq!(m.display_name, "Spatial Transcriptomics");
    assert_eq!(m.edam_topic, "topic:3308");
    assert_eq!(m.edam_operation, "operation:3223");
    assert_eq!(m.archetype_id.as_deref(), Some("spatial_transcriptomics"));
    assert_eq!(
        m.fixture_corpus_rows,
        vec![
            "spatial_visium_human".to_string(),
            "spatial_merfish_mouse".to_string()
        ]
    );
    assert!(m.keywords.contains(&"visium".to_string()));
}

#[test]
fn schema_rejects_modality_missing_required_fields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let schema_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/modalities/_modality.schema.json");
    std::fs::copy(&schema_src, tmp.path().join("_modality.schema.json")).unwrap();
    // Missing edam_operation → schema must reject.
    std::fs::write(
        tmp.path().join("incomplete.yaml"),
        r#"
id: incomplete
display_name: Incomplete
keywords: []
edam_topic: "topic:0000"
"#,
    )
    .unwrap();
    assert!(
        ModalityRegistry::load_from_dir(tmp.path()).is_err(),
        "schema must reject modality missing edam_operation"
    );
}

#[test]
fn schema_rejects_invalid_edam_format() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let schema_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/modalities/_modality.schema.json");
    std::fs::copy(&schema_src, tmp.path().join("_modality.schema.json")).unwrap();
    // edam_topic format violation: schema requires `topic:NNNN`.
    std::fs::write(
        tmp.path().join("bad_topic.yaml"),
        r#"
id: bad_topic
display_name: Bad Topic
keywords: []
edam_topic: "data:0000"
edam_operation: "operation:0000"
"#,
    )
    .unwrap();
    assert!(
        ModalityRegistry::load_from_dir(tmp.path()).is_err(),
        "schema must reject edam_topic that doesn't match `topic:NNNN`"
    );
}
