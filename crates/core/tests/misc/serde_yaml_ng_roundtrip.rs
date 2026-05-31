//! Guards the YAML-loader determinism invariant after migrating the
//! workspace YAML backend from the archived `serde_yml`
//! (RUSTSEC-2025-0068) to `serde_yaml_ng`. Every config registry parses
//! through `serde_yaml_ng::from_str`, and the emit path depends on the
//! re-serialized form being stable; this exercises a representative
//! committed registry YAML to confirm the new backend round-trips
//! deterministically and value-preservingly.

use std::path::PathBuf;

/// `CARGO_MANIFEST_DIR` is `crates/core`; the committed config lives at
/// the repo root one level up from the `crates/` directory.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read_registry_yaml(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn representative_registry_yaml_parses_and_reserializes_deterministically() {
    // A real committed stage-atom registry file — the same shape the
    // compiler's `atom_registry` loader parses on every build.
    let raw = read_registry_yaml("config/stage-atoms/alignment.yaml");

    let value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&raw).expect("registry YAML parses under serde_yaml_ng");

    // Re-serialization must be stable across repeated calls so emitted
    // packages stay byte-reproducible.
    let first = serde_yaml_ng::to_string(&value).expect("re-serialize");
    let second = serde_yaml_ng::to_string(&value).expect("re-serialize again");
    assert_eq!(
        first, second,
        "serde_yaml_ng re-serialization must be deterministic across calls"
    );

    // Parse the re-serialized form again; the value must be preserved
    // (no information loss across a full round trip).
    let reparsed: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&first).expect("re-serialized YAML re-parses");
    assert_eq!(
        value, reparsed,
        "a serde_yaml_ng parse -> serialize -> parse round trip must preserve the value"
    );
}

#[test]
fn modality_manifest_yaml_round_trips_value_identically() {
    // A second registry family (per-modality manifest) to confirm the
    // round-trip invariant isn't specific to one document shape.
    let raw = read_registry_yaml("config/modalities/bulk_rnaseq.yaml");

    let value: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&raw).expect("modality manifest parses under serde_yaml_ng");
    let serialized = serde_yaml_ng::to_string(&value).expect("re-serialize manifest");
    let reparsed: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&serialized).expect("re-serialized manifest re-parses");

    assert_eq!(
        value, reparsed,
        "modality-manifest round trip must preserve the value under serde_yaml_ng"
    );
}
