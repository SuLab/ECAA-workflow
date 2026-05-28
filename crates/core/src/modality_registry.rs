//! `ModalityRegistry` loads `config/modalities/<id>.yaml`
//! files into a typed registry.
//!
//! The single source of truth for modality definition: classifier
//! keywords, EDAM IRI tuple, taxonomy/archetype routing, fixture
//! corpus rows. Succeeds the per-modality block in
//! `modality-keywords.yaml` (the drift detector flags disagreement
//! during the migration window).
//!
//! Migration shape:
//! - **Migration window:** both `modality-keywords.yaml` and
//!   `config/modalities/<id>.yaml` coexist; `Classifier::load`
//!   reads keywords.yaml (the legacy authoritative source) while
//!   a drift detector warns on disagreement.
//! - **Cutover complete:** the drift detector flips to hard-fail;
//!   legacy keywords.yaml is deleted; `Classifier::load_from_modality_dir`
//!   becomes the only path.
//!
//! This module ships the registry + drift detector. `Classifier`
//! migration is incremental: add a new entry point that consults
//! the registry while leaving the legacy `Classifier::load` path
//! in place.

use crate::blocker::BlockerKind;
use anyhow::{anyhow, Context, Result};
use jsonschema::JSONSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MODALITY_SCHEMA_JSON: &str = include_str!("../../../config/modalities/_modality.schema.json");

/// Schema-layout version this loader accepts. Bump on breaking
/// layout changes; additive optional fields stay on the same
/// version. Mirrors the `schema_version` enum on
/// `config/modalities/_modality.schema.json`.
///
/// Read at registry-load time: a manifest whose `schema_version`
/// disagrees with this constant produces a
/// [`crate::blocker::BlockerKind::SchemaVersionMismatch`] so the
/// SME's BlockerCard dispatch lights the recovery affordance.
pub const CURRENT_MODALITY_SCHEMA_VERSION: &str = "0.1";

/// Per-modality manifest loaded from
/// `config/modalities/<id>.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ModalityDefinition {
    /// Schema-layout version. Validated against
    /// [`CURRENT_MODALITY_SCHEMA_VERSION`] at registry-load time;
    /// mismatch surfaces a `BlockerKind::SchemaVersionMismatch`.
    pub schema_version: String,
    /// Stable modality id.
    pub id: String,
    /// Human-readable name surfaced in UI.
    pub display_name: String,
    /// Substring keywords the classifier scans against.
    pub keywords: Vec<String>,
    /// Edam topic.
    pub edam_topic: String,
    /// Edam operation.
    pub edam_operation: String,
    /// Legacy taxonomy path. The keyword classifier loads
    /// this when no archetype match is found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taxonomy_path: Option<String>,
    /// Composer-native archetype id (composer fast-path target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archetype_id: Option<String>,
    /// Names of fixture corpus rows that exercise this modality.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixture_corpus_rows: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Interpretation policy override.
    pub interpretation_policy_override: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Prompt addendum path.
    pub prompt_addendum_path: Option<String>,
}

/// In-memory modality registry.
#[derive(Debug, Clone, Default)]
pub struct ModalityRegistry {
    modalities: BTreeMap<String, ModalityDefinition>,
}

impl ModalityRegistry {
    /// Walk `dir`, load every `*.yaml` file (excluding `_*.yaml`
    /// schema sidecars). Returns an empty registry when the dir is
    /// missing — mirrors `ProjectClassRegistry::load_from_dir` shape.
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let schema = Self::compiled_schema()?;
        let mut modalities = BTreeMap::new();
        if !dir.exists() {
            return Ok(Self { modalities });
        }
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("reading modalities dir {}", dir.display()))?
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
                .with_context(|| format!("reading modality file {}", path.display()))?;
            let yaml_val: serde_yml::Value = serde_yml::from_str(&raw)
                .with_context(|| format!("parsing modality YAML {}", path.display()))?;
            let parsed: Value = serde_json::to_value(&yaml_val)
                .with_context(|| format!("yaml→json reshape for {}", path.display()))?;
            // C23 — surface a typed schema_version_mismatch error
            // BEFORE the JSON Schema validator's generic `const`
            // failure. Callers that pre-load registries at startup
            // (`Classifier::load_from_modality_dir`) log + continue;
            // runtime loaders construct
            // `schema_version_mismatch_blocker` from the captured
            // `found` and route through the standard BlockerCard.
            if let Some(found) = parsed.get("schema_version").and_then(|v| v.as_str()) {
                if found != CURRENT_MODALITY_SCHEMA_VERSION {
                    return Err(anyhow!(
                        "modality {} schema_version_mismatch: \
                         expected {}, found {}",
                        path.display(),
                        CURRENT_MODALITY_SCHEMA_VERSION,
                        found,
                    ));
                }
            }
            if let Err(errors) = schema.validate(&parsed) {
                let msgs: Vec<String> = errors
                    .map(|e| format!("{} at {}", e, e.instance_path))
                    .collect();
                return Err(anyhow!(
                    "modality {} failed schema validation:\n  - {}",
                    path.display(),
                    msgs.join("\n  - ")
                ));
            }
            let modality: ModalityDefinition = serde_json::from_value(parsed)
                .with_context(|| format!("deserializing modality {}", path.display()))?;
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("modality path {} has no stem", path.display()))?;
            if stem != modality.id {
                return Err(anyhow!(
                    "modality file {} has stem {} but declares id {}",
                    path.display(),
                    stem,
                    modality.id
                ));
            }
            if modalities
                .insert(modality.id.clone(), modality.clone())
                .is_some()
            {
                return Err(anyhow!(
                    "duplicate modality id {} (second file: {})",
                    modality.id,
                    path.display()
                ));
            }
        }
        Ok(Self { modalities })
    }

    /// Iter.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ModalityDefinition)> {
        self.modalities.iter()
    }

    /// Get.
    pub fn get(&self, id: &str) -> Option<&ModalityDefinition> {
        self.modalities.get(id)
    }

    /// Len.
    pub fn len(&self) -> usize {
        self.modalities.len()
    }

    /// Is empty.
    pub fn is_empty(&self) -> bool {
        self.modalities.is_empty()
    }

    fn compiled_schema() -> Result<&'static JSONSchema> {
        crate::schema_helpers::compile_schema_cached("modality", MODALITY_SCHEMA_JSON)
    }

    /// Process-wide cached load. See `AtomRegistry::load_cached`.
    pub fn load_cached(dir: &Path) -> Result<Arc<Self>> {
        use std::collections::HashMap;
        use std::sync::OnceLock;
        static CACHE: OnceLock<std::sync::Mutex<HashMap<PathBuf, Arc<ModalityRegistry>>>> =
            OnceLock::new();
        let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        if let Ok(guard) = cache.lock() {
            if let Some(reg) = guard.get(&key) {
                return Ok(reg.clone());
            }
        }
        let reg = Arc::new(Self::load_from_dir(dir)?);
        if let Ok(mut guard) = cache.lock() {
            guard.insert(key, reg.clone());
        }
        Ok(reg)
    }
}

/// Build a typed [`BlockerKind::SchemaVersionMismatch`] for a
/// modality-config schema-version mismatch. C23 — wired through the
/// existing blocker mechanism so the SME-facing `BlockerCard` lights
/// the recovery affordance ("I've migrated — retry load") via the
/// same dispatch path that handles every other blocker.
///
/// Caller responsibility: pull `found` from the on-disk
/// `schema_version` field BEFORE the JSON Schema validator rejects
/// the document; the `const: "0.1"` keyword on
/// `_modality.schema.json` produces a generic schema-validation
/// error that the SME can't act on without operator intervention.
pub fn schema_version_mismatch_blocker(found: impl Into<String>) -> BlockerKind {
    BlockerKind::SchemaVersionMismatch {
        config_kind: "modality_config".to_string(),
        expected: CURRENT_MODALITY_SCHEMA_VERSION.to_string(),
        found: found.into(),
    }
}

/// Drift detector. Compares the manifest's modality set
/// and per-modality fields against the legacy `modality-keywords.yaml`
/// and returns the divergences. Empty Vec means no drift.
///
/// **Migration window:** callers warn-log this without failing.
/// **Cutover:** the same call becomes a hard CI failure on any drift.
#[derive(Debug, Clone, PartialEq)]
pub enum ModalityDrift {
    /// InManifestOnly variant.
    InManifestOnly {
        /// Id.
        id: String,
    },
    /// InLegacyOnly variant.
    InLegacyOnly {
        /// Id.
        id: String,
    },
    /// KeywordsDiverge variant.
    KeywordsDiverge {
        /// Id.
        id: String,
        /// Manifest only.
        manifest_only: Vec<String>,
        /// Legacy only.
        legacy_only: Vec<String>,
    },
    /// EdamTopicMismatch variant.
    EdamTopicMismatch {
        /// Id.
        id: String,
        /// Manifest.
        manifest: String,
        /// Legacy.
        legacy: String,
    },
    /// EdamOperationMismatch variant.
    EdamOperationMismatch {
        /// Id.
        id: String,
        /// Manifest.
        manifest: String,
        /// Legacy.
        legacy: String,
    },
}

impl ModalityDrift {
    /// Id.
    pub fn id(&self) -> &str {
        match self {
            ModalityDrift::InManifestOnly { id } => id,
            ModalityDrift::InLegacyOnly { id } => id,
            ModalityDrift::KeywordsDiverge { id, .. } => id,
            ModalityDrift::EdamTopicMismatch { id, .. } => id,
            ModalityDrift::EdamOperationMismatch { id, .. } => id,
        }
    }
}

/// Compare manifest registry against legacy `modality-keywords.yaml`
/// content. Returns the list of disagreements. Used by the drift
/// detector script + tests.
pub fn detect_drift_against_legacy(
    manifest: &ModalityRegistry,
    legacy_modalities: &[(String, String, String, Vec<String>)], // (id, edam_topic, edam_operation, keywords)
) -> Vec<ModalityDrift> {
    let mut drifts = Vec::new();
    let manifest_ids: std::collections::BTreeSet<&str> =
        manifest.modalities.keys().map(String::as_str).collect();
    let legacy_ids: std::collections::BTreeSet<&str> = legacy_modalities
        .iter()
        .map(|(id, _, _, _)| id.as_str())
        .collect();

    for id in &manifest_ids - &legacy_ids {
        drifts.push(ModalityDrift::InManifestOnly { id: id.to_string() });
    }
    for id in &legacy_ids - &manifest_ids {
        drifts.push(ModalityDrift::InLegacyOnly { id: id.to_string() });
    }
    for id in &manifest_ids & &legacy_ids {
        let manifest_def = manifest.get(id).unwrap();
        let legacy = legacy_modalities
            .iter()
            .find(|(lid, _, _, _)| lid == id)
            .unwrap();
        if manifest_def.edam_topic != legacy.1 {
            drifts.push(ModalityDrift::EdamTopicMismatch {
                id: id.to_string(),
                manifest: manifest_def.edam_topic.clone(),
                legacy: legacy.1.clone(),
            });
        }
        if manifest_def.edam_operation != legacy.2 {
            drifts.push(ModalityDrift::EdamOperationMismatch {
                id: id.to_string(),
                manifest: manifest_def.edam_operation.clone(),
                legacy: legacy.2.clone(),
            });
        }
        let manifest_kw: std::collections::BTreeSet<&str> =
            manifest_def.keywords.iter().map(String::as_str).collect();
        let legacy_kw: std::collections::BTreeSet<&str> =
            legacy.3.iter().map(String::as_str).collect();
        let in_manifest_only: Vec<String> = (&manifest_kw - &legacy_kw)
            .into_iter()
            .map(String::from)
            .collect();
        let in_legacy_only: Vec<String> = (&legacy_kw - &manifest_kw)
            .into_iter()
            .map(String::from)
            .collect();
        if !in_manifest_only.is_empty() || !in_legacy_only.is_empty() {
            drifts.push(ModalityDrift::KeywordsDiverge {
                id: id.to_string(),
                manifest_only: in_manifest_only,
                legacy_only: in_legacy_only,
            });
        }
    }
    drifts
}

/// Resolve the workspace-root `config/` directory.
///
/// Uses `env!("CARGO_MANIFEST_DIR")` resolved against `crates/core` at
/// compile time, so the returned path is `<workspace>/config`
/// regardless of which crate (or test binary) calls into it. Callers
/// should consolidate per-crate ad-hoc helpers onto this single
/// function. The function deliberately uses `unwrap()` on the two
/// `.parent()` hops because the workspace layout is a load-bearing
/// invariant — a missing parent means the workspace is corrupt or
/// the crate is relocated out of `crates/`, both of which are bugs
/// that should panic loudly at test startup.
pub fn workspace_config_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/core must live under <workspace>/crates/")
        .parent()
        .expect("crates/core must live under <workspace>/crates/")
        .join("config")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sanity check on the live registry: the seed modalities the
    /// classifier depends on are still present. Count is the full
    /// registry size; new modalities will bump this when they ship.
    #[test]
    fn loads_seeded_modalities() {
        let reg = ModalityRegistry::load_from_dir(&workspace_config_dir().join("modalities"))
            .expect("registry must load");
        assert!(
            reg.len() >= 19,
            "registry should carry at least the 19 keyword-routable modalities; got {}",
            reg.len()
        );
        for id in [
            "bulk_rnaseq",
            "single_cell_rnaseq",
            "variant_calling",
            "chip_seq",
            "atac_seq",
            "metagenomics",
            "proteomics",
            "generic_omics",
        ] {
            assert!(reg.get(id).is_some(), "missing modality {id}");
        }
    }

    #[test]
    fn drift_detector_surfaces_keyword_divergence() {
        // Build a synthetic in-manifest-only modality with a keyword
        // not present in the legacy block.
        let mut reg = ModalityRegistry::default();
        reg.modalities.insert(
            "synthetic".into(),
            ModalityDefinition {
                schema_version: CURRENT_MODALITY_SCHEMA_VERSION.into(),
                id: "synthetic".into(),
                display_name: "Synth".into(),
                keywords: vec!["foo".into(), "bar".into()],
                edam_topic: "topic:0001".into(),
                edam_operation: "operation:0001".into(),
                taxonomy_path: None,
                archetype_id: None,
                fixture_corpus_rows: vec![],
                interpretation_policy_override: None,
                prompt_addendum_path: None,
            },
        );
        let legacy = vec![(
            "synthetic".to_string(),
            "topic:0001".to_string(),
            "operation:0001".to_string(),
            vec!["foo".to_string(), "baz".to_string()],
        )];
        let drifts = detect_drift_against_legacy(&reg, &legacy);
        assert_eq!(drifts.len(), 1);
        match &drifts[0] {
            ModalityDrift::KeywordsDiverge {
                id,
                manifest_only,
                legacy_only,
            } => {
                assert_eq!(id, "synthetic");
                assert_eq!(manifest_only, &["bar".to_string()]);
                assert_eq!(legacy_only, &["baz".to_string()]);
            }
            other => panic!("expected KeywordsDiverge, got {other:?}"),
        }
    }

    #[test]
    fn drift_detector_surfaces_set_divergence() {
        let manifest = ModalityRegistry::default();
        let legacy = vec![(
            "extra".to_string(),
            "topic:0001".to_string(),
            "operation:0001".to_string(),
            vec![],
        )];
        let drifts = detect_drift_against_legacy(&manifest, &legacy);
        assert_eq!(drifts.len(), 1);
        assert!(matches!(
            drifts[0],
            ModalityDrift::InLegacyOnly { ref id } if id == "extra"
        ));
    }

    #[test]
    fn nonexistent_dir_yields_empty_registry() {
        let reg = ModalityRegistry::load_from_dir(Path::new("/nonexistent")).unwrap();
        assert!(reg.is_empty());
    }

    /// C23 — a modality YAML carrying a `schema_version` the loader
    /// doesn't recognize must surface as a typed
    /// `schema_version_mismatch:` error BEFORE the JSON Schema
    /// validator's generic `const` failure. Operators / callers that
    /// want a SME-facing card promote via
    /// [`schema_version_mismatch_blocker`].
    #[test]
    fn schema_version_mismatch_surfaces_typed_error() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        // Author a synthetic modality YAML with a bogus schema_version
        // that the loader hasn't been compiled against.
        let path = dir.path().join("synth.yaml");
        let mut f = std::fs::File::create(&path).unwrap();
        write!(
            f,
            "schema_version: \"0.99\"\n\
             id: synth\n\
             display_name: Synth\n\
             keywords: [synth]\n\
             edam_topic: \"topic:0001\"\n\
             edam_operation: \"operation:0001\"\n",
        )
        .unwrap();
        let err = ModalityRegistry::load_from_dir(dir.path())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("schema_version_mismatch"),
            "expected typed schema_version_mismatch error, got: {}",
            err
        );
        assert!(err.contains("0.99"), "error must echo the found version");
        assert!(
            err.contains(CURRENT_MODALITY_SCHEMA_VERSION),
            "error must echo the expected version"
        );

        // The typed BlockerKind constructor mirrors the error
        // detail so callers can promote without re-parsing.
        let blocker = schema_version_mismatch_blocker("0.99");
        match blocker {
            BlockerKind::SchemaVersionMismatch {
                config_kind,
                expected,
                found,
            } => {
                assert_eq!(config_kind, "modality_config");
                assert_eq!(expected, CURRENT_MODALITY_SCHEMA_VERSION);
                assert_eq!(found, "0.99");
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
    }
}
