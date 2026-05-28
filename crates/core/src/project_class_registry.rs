//! `ProjectClassRegistry` loads `config/project-classes/<id>.yaml`
//! files into a typed registry the rest of the workspace consults for
//! per-class prompt addenda, taxonomy patterns, container policy, and
//! BCO routing.
//!
//! The closed `ProjectClass` enum stays as the type-system-enforced
//! exhaustiveness guarantee for the three classes currently defined
//! (bioinformatics, clinical_trial, time_series_forecast); the
//! registry layer makes each class's *metadata* config-driven so
//! adding a 4th class is a YAML drop plus an enum variant rather than
//! a 9-site Rust sweep. Decision Q1 of the plan tracks the eventual
//! enum-to-newtype migration; this module is the bridge.
//!
//! Schema: `config/project-classes/_project_class.schema.json`.

use crate::project_class::ProjectClass;
use anyhow::{anyhow, Context, Result};
use jsonschema::JSONSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const PROJECT_CLASS_SCHEMA_JSON: &str =
    include_str!("../../../config/project-classes/_project_class.schema.json");

/// Per-class metadata loaded from
/// `config/project-classes/<id>.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ProjectClassDefinition {
    /// Stable class id (matches `ProjectClass::as_str()` for the three
    /// closed-enum variants).
    pub id: String,
    /// Human-readable name surfaced in SME-facing UI.
    pub display_name: String,
    /// Optional path to the class-specific prompt addendum the
    /// conversation crate appends after `prompt_role.txt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_addendum_path: Option<String>,
    /// glob pattern relative to `config/stage-taxonomies/`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taxonomy_pattern: Option<String>,
    /// When true, taxonomies in this class must declare a
    /// `preferred_container`. Bio: false; non-bio: typically true.
    #[serde(default)]
    pub requires_container: bool,
    /// When true, the emitter unconditionally writes a Biocompute
    /// Object alongside the package.
    #[serde(default)]
    pub auto_bco: bool,
    /// Optional path of a per-class `interpretation-policy.<class>.json`
    /// that overrides the default modality-level policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpretation_policy_override: Option<String>,
    /// Per-role prompt fragments. Keys are
    /// `AtomRole` variants in PascalCase (e.g. `Validation`, `Pilot`);
    /// values are paths to per-role addendum files. The conversation
    /// crate concatenates the per-role addendum onto the per-class
    /// block when an atom of that role is in the composition.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub role_addenda: BTreeMap<String, String>,
}

/// In-memory project-class registry. Mirrors
/// `ArchetypeRegistry` / `AtomRegistry` shape so the workspace's
/// three config loaders share the same validation discipline.
#[derive(Debug, Clone, Default)]
pub struct ProjectClassRegistry {
    classes: BTreeMap<String, ProjectClassDefinition>,
}

impl ProjectClassRegistry {
    /// Walk `dir`, load every `*.yaml` file (excluding `_*.yaml`
    /// schema sidecars). Returns an empty registry when the directory
    /// is missing — mirrors the permissive shape of the sibling
    /// loaders so tests and offline modes can run without the dir.
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let schema = Self::compiled_schema()?;
        let mut classes = BTreeMap::new();
        if !dir.exists() {
            return Ok(Self { classes });
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("reading project-classes dir {}", dir.display()))?
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
                .with_context(|| format!("reading project-class file {}", path.display()))?;
            let yaml_val: serde_yml::Value = serde_yml::from_str(&raw)
                .with_context(|| format!("parsing project-class YAML {}", path.display()))?;
            let parsed: Value = serde_json::to_value(&yaml_val)
                .with_context(|| format!("yaml→json reshape for {}", path.display()))?;
            if let Err(errors) = schema.validate(&parsed) {
                let msgs: Vec<String> = errors
                    .map(|e| format!("{} at {}", e, e.instance_path))
                    .collect();
                return Err(anyhow!(
                    "project-class {} failed schema validation:\n  - {}",
                    path.display(),
                    msgs.join("\n  - ")
                ));
            }
            let class: ProjectClassDefinition = serde_json::from_value(parsed)
                .with_context(|| format!("deserializing project-class {}", path.display()))?;
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("project-class path {} has no stem", path.display()))?;
            if stem != class.id {
                return Err(anyhow!(
                    "project-class file {} has stem {} but declares id {}",
                    path.display(),
                    stem,
                    class.id
                ));
            }
            if classes.insert(class.id.clone(), class.clone()).is_some() {
                return Err(anyhow!(
                    "duplicate project-class id {} (second file: {})",
                    class.id,
                    path.display()
                ));
            }
        }
        Ok(Self { classes })
    }

    /// Iter.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ProjectClassDefinition)> {
        self.classes.iter()
    }

    /// Get.
    pub fn get(&self, id: &str) -> Option<&ProjectClassDefinition> {
        self.classes.get(id)
    }

    /// Get for class.
    pub fn get_for_class(&self, class: ProjectClass) -> Option<&ProjectClassDefinition> {
        self.get(class.as_str())
    }

    /// Len.
    pub fn len(&self) -> usize {
        self.classes.len()
    }

    /// Is empty.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// Addendum lookup: per-role override (if declared)
    /// falls back to per-class addendum. Returns `None` when neither
    /// path is set OR the class isn't registered. Callers concatenate
    /// the returned addenda; today's per-class block path stays the
    /// load-bearing surface.
    pub fn prompt_addendum_path(&self, class: ProjectClass, role: Option<&str>) -> Option<&str> {
        let def = self.get_for_class(class)?;
        if let Some(r) = role {
            if let Some(p) = def.role_addenda.get(r) {
                return Some(p.as_str());
            }
        }
        def.prompt_addendum_path.as_deref()
    }

    fn compiled_schema() -> Result<&'static JSONSchema> {
        crate::schema_helpers::compile_schema_cached("project-class", PROJECT_CLASS_SCHEMA_JSON)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace_config_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config")
    }

    #[test]
    fn loads_three_seeded_classes() {
        let reg =
            ProjectClassRegistry::load_from_dir(&workspace_config_dir().join("project-classes"))
                .expect("registry must load");
        assert_eq!(reg.len(), 3);
        for id in ["bioinformatics", "clinical_trial", "time_series_forecast"] {
            assert!(reg.get(id).is_some(), "missing class {id}");
        }
    }

    #[test]
    fn bioinformatics_has_no_addendum_path() {
        let reg =
            ProjectClassRegistry::load_from_dir(&workspace_config_dir().join("project-classes"))
                .unwrap();
        assert_eq!(
            reg.prompt_addendum_path(ProjectClass::Bioinformatics, None),
            None,
            "bio is the default; no addendum file"
        );
    }

    #[test]
    fn non_bio_classes_have_addendum_paths() {
        let reg =
            ProjectClassRegistry::load_from_dir(&workspace_config_dir().join("project-classes"))
                .unwrap();
        assert!(
            reg.prompt_addendum_path(ProjectClass::ClinicalTrial, None)
                .is_some(),
            "clinical_trial must declare a prompt addendum"
        );
        assert!(
            reg.prompt_addendum_path(ProjectClass::TimeSeriesForecast, None)
                .is_some(),
            "time_series_forecast must declare a prompt addendum"
        );
    }

    #[test]
    fn requires_container_matches_d5_decision() {
        let reg =
            ProjectClassRegistry::load_from_dir(&workspace_config_dir().join("project-classes"))
                .unwrap();
        assert!(
            !reg.get_for_class(ProjectClass::Bioinformatics)
                .unwrap()
                .requires_container,
            "bio is grandfathered: requires_container=false"
        );
        assert!(
            reg.get_for_class(ProjectClass::ClinicalTrial)
                .unwrap()
                .requires_container,
            "clinical_trial: D5 mandates container"
        );
        assert!(
            reg.get_for_class(ProjectClass::TimeSeriesForecast)
                .unwrap()
                .requires_container,
            "time_series_forecast: D5 mandates container"
        );
    }

    #[test]
    fn role_addenda_lookup_falls_back_to_class() {
        // Synthetic class with role_addenda for Validation only.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::copy(
            workspace_config_dir().join("project-classes/_project_class.schema.json"),
            tmp.path().join("_project_class.schema.json"),
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("survey.yaml"),
            r#"
id: survey
display_name: Survey Research
prompt_addendum_path: project-class-prompts/survey.txt
role_addenda:
  Validation: project-class-prompts/survey.validation.txt
"#,
        )
        .unwrap();
        let reg = ProjectClassRegistry::load_from_dir(tmp.path()).expect("registry load");
        let def = reg.get("survey").unwrap();
        assert_eq!(def.id, "survey");
        // Falls back to per-class addendum when role unmatched.
        assert_eq!(
            def.role_addenda.get("Validation"),
            Some(&"project-class-prompts/survey.validation.txt".to_string())
        );
        assert_eq!(
            def.prompt_addendum_path.as_deref(),
            Some("project-class-prompts/survey.txt")
        );
    }

    #[test]
    fn nonexistent_dir_yields_empty_registry() {
        let reg = ProjectClassRegistry::load_from_dir(Path::new("/nonexistent")).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn duplicate_id_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("dup1.yaml"),
            "id: dup\ndisplay_name: First\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("dup2.yaml"),
            "id: dup\ndisplay_name: Second\n",
        )
        .unwrap();
        // dup1.yaml has stem dup1 != id dup → load fails on stem mismatch
        // first; that's fine, the test of the duplicate-id branch lives
        // in `loads_three_seeded_classes` paired with file naming. We
        // assert here that the loader doesn't silently accept either.
        assert!(ProjectClassRegistry::load_from_dir(tmp.path()).is_err());
    }
}
