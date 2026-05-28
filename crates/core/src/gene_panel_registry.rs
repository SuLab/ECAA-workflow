//! Gene-panel registry loader.
//!
//! Mirrors [`crate::atom_registry::AtomRegistry`]: walks `config/gene-panels/`,
//! validates each `<id>.yaml` against the embedded `_gene_panel.schema.json`
//! sidecar, deserializes into [`crate::gene_panel::GenePanel`], and yields a
//! sorted-by-id collection.
//!
//! This is the (a)-lite wire-up: the substrate exists so composer
//! (S6.11) and discover_* `required_inputs` (per S5.14) can consume
//! curated marker-gene panels deterministically. Today the registry
//! is data-only — no consumer wired in `crates/core/src/composer.rs`
//! yet; that integration is a planned follow-on.

use crate::gene_panel::GenePanel;
use anyhow::{anyhow, Context, Result};
use jsonschema::JSONSchema;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// Embedded schema. Compile-time include guarantees the schema ships
/// with the binary; matches the AtomRegistry / ArchetypeRegistry
/// pattern for `include_str!` schema-validate-before-deserialize.
const GENE_PANEL_SCHEMA_JSON: &str =
    include_str!("../../../config/gene-panels/_gene_panel.schema.json");

/// In-memory gene-panel catalog. Keyed by `id`; `BTreeMap` so
/// iteration is byte-deterministic.
#[derive(Debug, Clone, Default)]
pub struct GenePanelRegistry {
    panels: BTreeMap<String, GenePanel>,
}

impl GenePanelRegistry {
    /// Walk `dir`, load every `*.yaml` file (skipping `_*.yaml` schema
    /// sidecars + shared fragments). Returns an error on the first
    /// malformed panel.
    ///
    /// `dir` is typically `config/gene-panels/`. Subdirectories are
    /// not recursed (panels are flat curated metadata).
    ///
    /// An empty or missing dir is allowed — composer fallback logic
    /// (when wired) checks `is_empty()` and routes through the prior
    /// behaviour. This matches the v1 atom registry's empty-dir
    /// tolerance.
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let schema = Self::compiled_schema()?;
        let mut panels = BTreeMap::new();
        if !dir.exists() {
            return Ok(Self { panels });
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| format!("reading gene-panel directory {}", dir.display()))?
            .filter_map(|r| r.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension().and_then(|s| s.to_str()) == Some("yaml")
                    && !p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with('_'))
                        .unwrap_or(false)
            })
            .collect();
        entries.sort();
        for path in entries {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading gene-panel file {}", path.display()))?;
            let yaml_val: serde_yml::Value = serde_yml::from_str(&raw)
                .with_context(|| format!("parsing gene-panel YAML {}", path.display()))?;
            let parsed: Value = serde_json::to_value(&yaml_val)
                .with_context(|| format!("yaml→json reshape for {}", path.display()))?;
            if let Err(errors) = schema.validate(&parsed) {
                let msgs: Vec<String> = errors
                    .map(|e| format!("{} at {}", e, e.instance_path))
                    .collect();
                return Err(anyhow!(
                    "gene-panel {} failed schema validation:\n  - {}",
                    path.display(),
                    msgs.join("\n  - ")
                ));
            }
            let panel: GenePanel = serde_json::from_value(parsed)
                .with_context(|| format!("deserializing gene-panel {}", path.display()))?;
            // Filename stem must match the panel id — same byte-
            // determinism contract as the atom registry.
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("invalid filename: {}", path.display()))?;
            if stem != panel.id {
                return Err(anyhow!(
                    "gene-panel filename stem {} does not match panel id {}",
                    stem,
                    panel.id
                ));
            }
            if let Some(prior) = panels.insert(panel.id.clone(), panel) {
                return Err(anyhow!("duplicate gene-panel id: {}", prior.id));
            }
        }
        Ok(Self { panels })
    }

    fn compiled_schema() -> Result<&'static JSONSchema> {
        crate::schema_helpers::compile_schema_cached("gene-panel", GENE_PANEL_SCHEMA_JSON)
    }

    /// Lookup by id.
    pub fn get(&self, id: &str) -> Option<&GenePanel> {
        self.panels.get(id)
    }

    /// Iterate panels in deterministic id order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &GenePanel)> {
        self.panels.iter()
    }

    /// `true` when the registry contains no panels (e.g. when
    /// `config/gene-panels/` is missing or empty).
    pub fn is_empty(&self) -> bool {
        self.panels.is_empty()
    }

    /// Total number of panels loaded.
    pub fn len(&self) -> usize {
        self.panels.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_root() -> std::path::PathBuf {
        // crates/core/Cargo.toml lives two levels above the workspace
        // root; CARGO_MANIFEST_DIR points at crates/core itself.
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("workspace root")
            .to_path_buf()
    }

    #[test]
    fn loads_real_config_directory_byte_identically() {
        let root = repo_root();
        let registry = GenePanelRegistry::load_from_dir(&root.join("config").join("gene-panels"))
            .expect("load real config/gene-panels/");
        assert!(!registry.is_empty(), "expected ≥ 1 panel in real config");
        let pain = registry
            .get("pain_pathway_canonical")
            .expect("pain_pathway_canonical present");
        assert!(pain.is_flat());
        assert!(pain.all_symbols().contains(&"SCN9A"));
    }

    #[test]
    fn missing_directory_returns_empty_registry() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let registry = GenePanelRegistry::load_from_dir(&tmp.path().join("does_not_exist"))
            .expect("load missing dir");
        assert!(registry.is_empty());
    }

    #[test]
    fn rejects_panel_with_both_flat_and_contrasts() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("bad.yaml"),
            "id: bad\ngenes: [A, B]\ncontrasts:\n  c:\n    - {symbol: X}\n",
        )
        .unwrap();
        let err = GenePanelRegistry::load_from_dir(tmp.path()).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("schema validation"),
            "expected schema-validation failure, got: {}",
            msg
        );
    }

    #[test]
    fn rejects_filename_id_mismatch() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("foo.yaml"), "id: bar\ngenes: [A]\n").unwrap();
        let err = GenePanelRegistry::load_from_dir(tmp.path()).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("does not match"),
            "expected filename-id mismatch error, got: {}",
            msg
        );
    }
}
