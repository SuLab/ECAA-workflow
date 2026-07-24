//! Curated biomedical subfield catalog: 18 interpretive lenses loaded from
//! `config/ensemble-subfields/<id>.yaml`, schema-guarded by the sidecar
//! `_subfield.schema.json`. These are the adaptive lenses a later
//! deterministic selector picks from per-analysis (matching a goal
//! statement's vocabulary against each entry's `select_keywords`) —
//! distinct from the global 5-lens epistemic core in
//! `config/ensemble-lenses/lenses.yaml` (shared by every modality, see
//! `ensemble_roster::EnsembleRosterProvider::load_epistemic_core`) and the
//! per-modality `interpretive_lenses` block in
//! `config/ensemble-rosters/<modality>.yaml`.
//!
//! Mirrors `ModalityRegistry::load_from_dir`'s schema-guard shape exactly:
//! YAML → reshaped `serde_json::Value` → typed `schema_version` pre-check
//! (a clearer error than a generic JSON-Schema `const` violation) →
//! JSON-Schema validate → typed deserialize, with `_`-prefixed files
//! skipped, `stem == id` enforced, and results collected into a
//! deterministic `BTreeMap`.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const SUBFIELD_SCHEMA_JSON: &str =
    include_str!("../../../config/ensemble-subfields/_subfield.schema.json");

/// Schema-layout version this loader accepts. Mirrors
/// `modality_registry::CURRENT_MODALITY_SCHEMA_VERSION`'s role: a manifest
/// whose `schema_version` disagrees is rejected before generic JSON-Schema
/// validation runs, so the error names the mismatch explicitly.
pub const CURRENT_SUBFIELD_SCHEMA_VERSION: &str = "0.1";

/// One biomedical subfield lens loaded from
/// `config/ensemble-subfields/<id>.yaml`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubfieldLens {
    /// Schema-layout version. Validated against
    /// [`CURRENT_SUBFIELD_SCHEMA_VERSION`] at catalog-load time.
    pub schema_version: String,
    /// Stable subfield id; must equal the file stem.
    pub id: String,
    /// Filename of the persona markdown file under
    /// `config/ensemble-subfields/personas/`.
    pub persona_ref: String,
    /// Model-tier assignment for this lens's contextualization pass
    /// (`opus` or `sonnet`).
    pub model_tier: String,
    /// Literature-retrieval bias for this lens (`recent` or
    /// `foundational`).
    pub retrieval: String,
    /// Domain-vocabulary keywords a goal statement in this subfield would
    /// contain; the deterministic selector scores subfields against a
    /// goal by keyword match.
    pub select_keywords: Vec<String>,
}

/// In-memory catalog of biomedical subfield lenses, keyed by id.
#[derive(Debug, Clone, Default)]
pub struct SubfieldCatalog {
    /// All loaded lenses, sorted by id.
    pub by_id: BTreeMap<String, SubfieldLens>,
    /// The directory `load_from_dir` was loaded from (personas live at
    /// `<root>/personas/`).
    pub root: PathBuf,
}

impl SubfieldCatalog {
    /// Walk `dir`, load every `<id>.yaml` (excluding `_`-prefixed schema
    /// sidecars and the nested `personas/` directory, which has no
    /// `.yaml` extension match). Returns an empty catalog when the dir is
    /// missing — mirrors `ModalityRegistry::load_from_dir` shape.
    pub fn load_from_dir(dir: &Path) -> Result<Self, String> {
        let schema =
            crate::schema_helpers::compile_schema_cached("ensemble_subfield", SUBFIELD_SCHEMA_JSON)
                .map_err(|e| format!("compiling ensemble_subfield schema: {e}"))?;

        let mut by_id = BTreeMap::new();
        if !dir.exists() {
            return Ok(Self {
                by_id,
                root: dir.to_path_buf(),
            });
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| format!("reading subfields dir {}: {e}", dir.display()))?
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
                .map_err(|e| format!("reading subfield file {}: {e}", path.display()))?;
            let yaml_val: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw)
                .map_err(|e| format!("parsing subfield YAML {}: {e}", path.display()))?;
            let parsed: serde_json::Value = serde_json::to_value(&yaml_val)
                .map_err(|e| format!("yaml→json reshape for {}: {e}", path.display()))?;

            // Surface a typed schema_version_mismatch error BEFORE the
            // JSON Schema validator's generic `const` failure, mirroring
            // `ModalityRegistry::load_from_dir` (C23).
            if let Some(found) = parsed.get("schema_version").and_then(|v| v.as_str()) {
                if found != CURRENT_SUBFIELD_SCHEMA_VERSION {
                    return Err(format!(
                        "subfield {} schema_version_mismatch: expected {}, found {}",
                        path.display(),
                        CURRENT_SUBFIELD_SCHEMA_VERSION,
                        found,
                    ));
                }
            }

            if let Err(errors) = schema.validate(&parsed) {
                let msgs: Vec<String> = errors
                    .map(|e| format!("{} at {}", e, e.instance_path))
                    .collect();
                return Err(format!(
                    "subfield {} failed schema validation:\n  - {}",
                    path.display(),
                    msgs.join("\n  - ")
                ));
            }

            let lens: SubfieldLens = serde_json::from_value(parsed)
                .map_err(|e| format!("deserializing subfield {}: {e}", path.display()))?;

            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| format!("subfield path {} has no stem", path.display()))?;
            if stem != lens.id {
                return Err(format!(
                    "subfield file {} has stem {} but declares id {}",
                    path.display(),
                    stem,
                    lens.id
                ));
            }
            if by_id.insert(lens.id.clone(), lens.clone()).is_some() {
                return Err(format!(
                    "duplicate subfield id {} (second file: {})",
                    lens.id,
                    path.display()
                ));
            }
        }
        Ok(Self {
            by_id,
            root: dir.to_path_buf(),
        })
    }

    /// Absolute path to the persona markdown file for `id`. Panics if `id`
    /// is not in the catalog — callers are expected to have already
    /// resolved `id` against [`Self::by_id`] (e.g. via the Task-4
    /// deterministic selector).
    pub fn persona_path(&self, id: &str) -> PathBuf {
        self.root.join("personas").join(&self.by_id[id].persona_ref)
    }

    /// Number of loaded subfield lenses.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Whether the catalog is empty.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Get a lens by id.
    pub fn get(&self, id: &str) -> Option<&SubfieldLens> {
        self.by_id.get(id)
    }

    /// Iterate all lenses, sorted by id.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &SubfieldLens)> {
        self.by_id.iter()
    }
}

/// Resolve the workspace-root `config/ensemble-subfields` directory.
/// Mirrors `modality_registry::workspace_config_dir`'s CARGO_MANIFEST_DIR
/// resolution so tests + downstream callers share one source of truth.
pub fn workspace_subfields_dir() -> PathBuf {
    crate::modality_registry::workspace_config_dir().join("ensemble-subfields")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_loads_all_18() {
        let catalog = SubfieldCatalog::load_from_dir(&workspace_subfields_dir())
            .expect("SubfieldCatalog::load_from_dir must succeed for the live catalog");
        assert_eq!(
            catalog.len(),
            18,
            "expected 18 subfield lenses, got {}",
            catalog.len()
        );
        for (id, lens) in catalog.iter() {
            assert_eq!(id, &lens.id, "map key must equal declared id");
            assert!(
                !lens.select_keywords.is_empty(),
                "subfield {id} must carry at least one select_keyword"
            );
            assert!(
                !lens.persona_ref.is_empty(),
                "subfield {id} must carry persona_ref"
            );
        }
    }

    #[test]
    fn nonexistent_dir_yields_empty_catalog() {
        let catalog = SubfieldCatalog::load_from_dir(Path::new("/nonexistent/xyz")).unwrap();
        assert!(catalog.is_empty());
    }

    #[test]
    fn catalog_rejects_unknown_field() {
        let dir = tempfile::tempdir().unwrap();
        let bad = "schema_version: \"0.1\"\n\
                   id: synthetic\n\
                   persona_ref: synthetic.md\n\
                   model_tier: opus\n\
                   retrieval: recent\n\
                   select_keywords: [foo]\n\
                   rogue_field: true\n";
        std::fs::write(dir.path().join("synthetic.yaml"), bad).unwrap();
        let err = SubfieldCatalog::load_from_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("failed schema validation"),
            "expected schema validation failure, got: {err}"
        );
    }

    #[test]
    fn catalog_rejects_schema_version_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let bad = "schema_version: \"9.9\"\n\
                   id: synthetic\n\
                   persona_ref: synthetic.md\n\
                   model_tier: opus\n\
                   retrieval: recent\n\
                   select_keywords: [foo]\n";
        std::fs::write(dir.path().join("synthetic.yaml"), bad).unwrap();
        let err = SubfieldCatalog::load_from_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("schema_version_mismatch"),
            "expected typed schema_version_mismatch error, got: {err}"
        );
    }

    #[test]
    fn catalog_rejects_stem_id_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let bad = "schema_version: \"0.1\"\n\
                   id: not_the_stem\n\
                   persona_ref: not_the_stem.md\n\
                   model_tier: opus\n\
                   retrieval: recent\n\
                   select_keywords: [foo]\n";
        std::fs::write(dir.path().join("synthetic.yaml"), bad).unwrap();
        let err = SubfieldCatalog::load_from_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("has stem"),
            "expected stem-mismatch error, got: {err}"
        );
    }

    #[test]
    fn catalog_rejects_empty_select_keywords() {
        let dir = tempfile::tempdir().unwrap();
        let bad = "schema_version: \"0.1\"\n\
                   id: synthetic\n\
                   persona_ref: synthetic.md\n\
                   model_tier: opus\n\
                   retrieval: recent\n\
                   select_keywords: []\n";
        std::fs::write(dir.path().join("synthetic.yaml"), bad).unwrap();
        let err = SubfieldCatalog::load_from_dir(dir.path()).unwrap_err();
        assert!(
            err.contains("failed schema validation"),
            "expected minItems violation, got: {err}"
        );
    }

    #[test]
    fn persona_path_joins_root_personas_and_ref() {
        let catalog =
            SubfieldCatalog::load_from_dir(&workspace_subfields_dir()).expect("catalog loads");
        let path = catalog.persona_path("immunology");
        assert_eq!(
            path,
            workspace_subfields_dir()
                .join("personas")
                .join("immunology.md")
        );
        assert!(
            path.exists(),
            "persona file must exist on disk: {}",
            path.display()
        );
    }

    #[test]
    fn every_persona_file_passes_the_honest_lens_lint() {
        let catalog =
            SubfieldCatalog::load_from_dir(&workspace_subfields_dir()).expect("catalog loads");
        for (id, lens) in catalog.iter() {
            let path = catalog.persona_path(id);
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("persona file missing for {id}: {}", path.display()));
            crate::ensemble_roster::lint_persona_text(&lens.id, &text)
                .unwrap_or_else(|e| panic!("persona {id} failed honesty lint: {e}"));
            assert!(
                text.contains("{entities}"),
                "persona {id} must carry the {{entities}} placeholder"
            );
        }
    }
}
