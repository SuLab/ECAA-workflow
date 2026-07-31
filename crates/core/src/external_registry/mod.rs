//! External registry ingestion (skeleton).
//!
//! Per design §16, the composer should be able to import external
//! workflow / tool registries (bio.tools, Dockstore, WorkflowHub,
//! GA4GH TRS, local CWL/WDL/Nextflow/Snakemake). This module ships:
//!
//! - `ExternalRegistryRef` — typed pointer carrying registry kind,
//!   id, version, and provenance metadata.
//! - `ExternalImporter` trait — pluggable per-registry importer
//!   producing a `TaskNode` from an external entry.
//! - `LocalCwlImporter` — minimal local-CWL fixture importer
//!   demonstrating the pattern. Imports stay quarantined as
//!   `LifecycleState::Contracted` with `TrustLevel::Unverified`
//!   until local validation promotes them.
//! - `RegistrySnapshot` — deterministic snapshot id used in
//!   `CompatibilityProof.evidence` and the planner's cache key
//!   so external registry refresh is observable in provenance.
//!
//! Network access is **not** required for deterministic
//! re-emission (alignment plan acceptance). The importers are
//! sync, side-effect-free, and consume already-cached snapshots.

pub mod local_cwl;
pub mod publish;
pub mod registry_improvement;
pub mod to_atom;

pub use local_cwl::LocalCwlImporter;
pub use registry_improvement::{
    aggregate_unknowns, aggregate_unknowns_from_inputs, AggregatorInput, RegistryImprovementSignal,
};
pub use to_atom::imported_node_to_atom;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

use crate::ingestion_safety::IngestionSafetyReport;
use crate::workflow_contracts::task_node::TaskNode;

/// Trust tier of an external registry. `Community` is the conservative
/// default; `Curated` is operator-declared (see `ECAA_EXTERNAL_CURATED_DIRS`).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RegistryTier {
    /// Community-sourced; caps at Unverified/Contracted.
    #[default]
    Community,
    /// Operator-curated; may reach StaticChecked after validation.
    Curated,
}

impl RegistryTier {
    /// Trust band a freshly-imported entry of this tier may enter at.
    /// Community always Unverified; Curated still enters Unverified but
    /// MAY be lifted to StaticChecked once it passes
    /// `LocalCwlImporter::validate_for_executable` (see `allows_static_checked`).
    pub fn entry_trust(self) -> crate::workflow_contracts::lifecycle::TrustLevel {
        crate::workflow_contracts::lifecycle::TrustLevel::Unverified
    }

    /// True when an entry of this tier may be promoted to
    /// `TrustLevel::StaticChecked` after schema + safety + executable
    /// validation. Community caps below this so community tools cannot
    /// reach production execution without explicit human promotion.
    pub fn allows_static_checked(self) -> bool {
        matches!(self, RegistryTier::Curated)
    }

    /// Promotion authority recorded on the imported node's provenance.
    pub fn promotion_authority(
        self,
        registry_id: &str,
    ) -> crate::workflow_contracts::lifecycle::PromotionAuthority {
        crate::workflow_contracts::lifecycle::PromotionAuthority {
            kind: "external_registry".into(),
            id: format!("{}:{}", self.canonical_name(), registry_id),
            at: String::new(), // Clock-free: federation provenance carries no timestamp.
        }
    }

    /// Stable snake_case key for provenance.
    pub fn canonical_name(self) -> &'static str {
        match self {
            RegistryTier::Community => "community",
            RegistryTier::Curated => "curated",
        }
    }
}

/// Stable reference to an external registry entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ExternalRegistryRef {
    /// Registry kind (`bio_tools`, `dockstore`, `workflowhub`,
    /// `trs`, `local_cwl`, `local_wdl`, `local_nextflow`,
    /// `local_snakemake`, `local_institutional`).
    pub registry: String,
    /// Entry id within the registry.
    pub id: String,
    /// Optional version pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    /// Optional URL for human inspection / provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
}

/// Cached registry snapshot. Stored on disk under
/// `~/.ecaa-workflow/external-snapshots/<registry>/<id>.json`
/// so determinism tests can replay against a stable bytes set.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RegistrySnapshot {
    /// Stable snapshot id (e.g. `2026-05-08T12:00:00Z`).
    pub snapshot_id: String,
    /// Registry kind.
    pub registry: String,
    /// Entry id.
    pub id: String,
    /// Free-form metadata blob the importer parses.
    #[ts(type = "Record<string, unknown>")]
    pub metadata: serde_json::Value,
}

/// Per-registry importer trait. Each impl converts a snapshot
/// into a `TaskNode` whose lifecycle/trust defaults to the
/// quarantine band.
pub trait ExternalImporter {
    /// Registry kind.
    fn registry_kind(&self) -> &'static str;
    /// Import.
    fn import(&self, snapshot: &RegistrySnapshot) -> Result<TaskNode, ExternalImportError>;
}

/// Errors during external import. These map to
/// `BlockerKind::ExternalImportFailed` when a session references
/// an entry that fails to import.
#[derive(
    thiserror::Error, Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ExternalImportError {
    /// Registry snapshot missing on disk.
    #[error("registry snapshot {snapshot_id:?} not found")]
    SnapshotNotFound { snapshot_id: String },
    /// Required field missing in the metadata blob.
    #[error("required field {field:?} missing in metadata")]
    MissingField { field: String },
    /// Container digest required but absent (executable nodes
    /// can't reach `Production`).
    #[error("container digest required but absent")]
    ContainerDigestMissing,
    /// License is unacceptable for the active policy bundle.
    #[error("license {license:?} is unacceptable for the active policy bundle")]
    LicenseUnacceptable { license: String },
    /// v3 P11 — ingestion-time injection scan returned a
    /// `Refuse` verdict. The carrier `IngestionSafetyReport`
    /// names the firing pattern + offending field for the SME
    /// surface.
    #[error("ingestion refused by injection scan ({} detections)", report.detections.len())]
    IngestionRefused { report: IngestionSafetyReport },
    /// Generic free-text fallback.
    #[error("{message}")]
    Other { message: String },
}

impl ExternalImportError {
    /// Lift this import failure into the SME-facing `BlockerKind`,
    /// tagged with the registry + entry id the session referenced.
    /// `IngestionRefused` folds the firing injection-pattern detections
    /// into the reason so the BlockerCard surfaces the offending field.
    pub fn into_blocker(self, registry: &str, id: &str) -> crate::blocker::BlockerKind {
        let reason = match &self {
            ExternalImportError::IngestionRefused { report } => format!(
                "ingestion refused by injection scan: {} detection(s) [{}]",
                report.detections.len(),
                report
                    .detections
                    .iter()
                    .map(|d| d.pattern_id.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            other => other.to_string(),
        };
        crate::blocker::BlockerKind::ExternalImportFailed {
            registry: registry.to_string(),
            id: id.to_string(),
            reason,
        }
    }
}

/// Minimal in-memory cache of registry snapshots.
/// Real on-disk snapshot loading is wired separately; today the
/// store is API-stable and tests inject fixtures.
#[derive(Debug, Clone, Default)]
pub struct ExternalRegistryStore {
    snapshots: BTreeMap<(String, String), RegistrySnapshot>,
}

impl ExternalRegistryStore {
    /// New.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert.
    pub fn insert(&mut self, snapshot: RegistrySnapshot) {
        self.snapshots
            .insert((snapshot.registry.clone(), snapshot.id.clone()), snapshot);
    }

    /// Get.
    pub fn get(&self, registry: &str, id: &str) -> Option<&RegistrySnapshot> {
        self.snapshots.get(&(registry.to_string(), id.to_string()))
    }

    /// Iter.
    pub fn iter(&self) -> impl Iterator<Item = ((&String, &String), &RegistrySnapshot)> {
        self.snapshots.iter().map(|((r, i), s)| ((r, i), s))
    }

    /// Walk `dir` for `<registry>/<id>.json` snapshot files and load
    /// them into a deterministic in-memory store. Subdirectory names
    /// are registry kinds; each `*.json` file is one `RegistrySnapshot`.
    /// A missing `dir` yields an empty store (mirrors
    /// `AtomRegistry::load_from_dir`). Entries are read in sorted path
    /// order so two loads over the same tree are byte-identical. No
    /// network access — these are pre-cached bytes (honors the module
    /// doc's offline contract).
    pub fn load_from_dir(dir: &std::path::Path) -> anyhow::Result<Self> {
        use anyhow::Context;
        let mut snapshots: BTreeMap<(String, String), RegistrySnapshot> = BTreeMap::new();
        if !dir.exists() {
            return Ok(Self { snapshots });
        }
        let mut registry_dirs: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .with_context(|| format!("reading external-snapshots dir {}", dir.display()))?
            .filter_map(|r| r.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        registry_dirs.sort();
        for reg_dir in registry_dirs {
            let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&reg_dir)
                .with_context(|| format!("reading registry dir {}", reg_dir.display()))?
                .filter_map(|r| r.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
                .collect();
            files.sort();
            for path in files {
                let raw = crate::fs_helpers::read_to_string_ctx(&path)?;
                let snap: RegistrySnapshot = serde_json::from_str(&raw)
                    .with_context(|| format!("parsing snapshot {}", path.display()))?;
                snapshots.insert((snap.registry.clone(), snap.id.clone()), snap);
            }
        }
        Ok(Self { snapshots })
    }
}

/// Map of `registry_kind -> importer`. Built-in registers the
/// `LocalCwlImporter`; sites add others. Deterministic iteration
/// (BTreeMap-keyed by kind).
#[derive(Default)]
pub struct ImporterRegistry {
    importers: BTreeMap<&'static str, Box<dyn ExternalImporter>>,
}

impl ImporterRegistry {
    /// Empty.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the built-in importers (local CWL today).
    pub fn with_builtin() -> Self {
        let mut me = Self::new();
        me.register(Box::new(LocalCwlImporter::default()));
        me
    }

    /// Register an importer under its `registry_kind`.
    pub fn register(&mut self, importer: Box<dyn ExternalImporter>) {
        self.importers.insert(importer.registry_kind(), importer);
    }

    /// Look up the importer for a registry kind.
    pub fn get(&self, kind: &str) -> Option<&dyn ExternalImporter> {
        self.importers.get(kind).map(|b| b.as_ref())
    }
}

impl ExternalRegistryStore {
    /// Resolve a batch of external refs into validated-on-import
    /// `AtomDefinition`s. Each ref's snapshot is dispatched through the
    /// matching `ExternalImporter` (by `registry`), then F1's
    /// `imported_node_to_atom`. Returns partials: one `AtomDefinition`
    /// per importable snapshot plus a typed `ExternalImportError` per
    /// failure, so one bad snapshot does not sink the batch. Sync,
    /// side-effect-free (no network). Caller passes the returned atoms
    /// to `AtomRegistry::with_external_overlay` for the schema + safety
    /// gate before composition.
    pub fn resolve(
        &self,
        refs: &[ExternalRegistryRef],
        importers: &ImporterRegistry,
    ) -> (Vec<crate::atom::AtomDefinition>, Vec<ExternalImportError>) {
        let mut atoms = Vec::new();
        let mut errors = Vec::new();
        for r in refs {
            let Some(snapshot) = self.get(&r.registry, &r.id) else {
                errors.push(ExternalImportError::SnapshotNotFound {
                    snapshot_id: format!("{}:{}", r.registry, r.id),
                });
                continue;
            };
            let Some(importer) = importers.get(&r.registry) else {
                errors.push(ExternalImportError::Other {
                    message: format!("no importer registered for registry kind {}", r.registry),
                });
                continue;
            };
            match importer.import(snapshot) {
                Ok(node) => match crate::external_registry::imported_node_to_atom(&node, r) {
                    Ok(atom) => atoms.push(atom),
                    Err(e) => errors.push(e),
                },
                Err(e) => errors.push(e),
            }
        }
        (atoms, errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::lifecycle::TrustLevel;
    use std::path::Path;

    #[test]
    fn tier_defaults_to_community_on_tierless_ref() {
        assert_eq!(RegistryTier::default(), RegistryTier::Community);
    }

    #[test]
    fn community_tier_entry_band_is_unverified() {
        assert_eq!(
            RegistryTier::Community.entry_trust(),
            TrustLevel::Unverified
        );
    }

    #[test]
    fn curated_tier_may_reach_static_checked() {
        // Curated is the only tier whose entry band is allowed to lift
        // to StaticChecked after validate_for_executable passes.
        assert!(RegistryTier::Curated.allows_static_checked());
        assert!(!RegistryTier::Community.allows_static_checked());
    }

    #[test]
    fn load_from_dir_is_id_sorted_and_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let reg_dir = tmp.path().join("local_cwl");
        std::fs::create_dir_all(&reg_dir).unwrap();
        for id in ["zebra", "alpha", "mango"] {
            let snap = RegistrySnapshot {
                snapshot_id: "2026-05-08T12:00:00Z".into(),
                registry: "local_cwl".into(),
                id: id.into(),
                metadata: serde_json::json!({"id": id}),
            };
            std::fs::write(
                reg_dir.join(format!("{id}.json")),
                serde_json::to_vec_pretty(&snap).unwrap(),
            )
            .unwrap();
        }
        let store_a = ExternalRegistryStore::load_from_dir(tmp.path()).unwrap();
        let store_b = ExternalRegistryStore::load_from_dir(tmp.path()).unwrap();
        let ids_a: Vec<&String> = store_a.iter().map(|((_, i), _)| i).collect();
        assert_eq!(ids_a, vec!["alpha", "mango", "zebra"]);
        // Determinism: two loads serialize identically.
        let a: Vec<_> = store_a.iter().map(|(k, _)| k.clone()).collect();
        let b: Vec<_> = store_b.iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn load_from_dir_missing_dir_is_empty() {
        let store = ExternalRegistryStore::load_from_dir(Path::new("/nonexistent/x")).unwrap();
        assert_eq!(store.iter().count(), 0);
    }

    #[test]
    fn store_round_trips_snapshot() {
        let mut store = ExternalRegistryStore::new();
        let snap = RegistrySnapshot {
            snapshot_id: "2026-05-08T12:00:00Z".into(),
            registry: "local_cwl".into(),
            id: "rnaseq".into(),
            metadata: serde_json::json!({"cwlVersion": "v1.2"}),
        };
        store.insert(snap.clone());
        assert_eq!(store.get("local_cwl", "rnaseq"), Some(&snap));
        assert!(store.get("local_cwl", "missing").is_none());
    }

    #[test]
    fn external_registry_ref_round_trips() {
        let r = ExternalRegistryRef {
            registry: "dockstore".into(),
            id: "scripps/dna-seq".into(),
            version: Some("v1.0.0".into()),
            url: Some("https://dockstore.org/...".into()),
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: ExternalRegistryRef = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn import_error_round_trips() {
        let e = ExternalImportError::ContainerDigestMissing;
        let json = serde_json::to_string(&e).unwrap();
        let back: ExternalImportError = serde_json::from_str(&json).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn resolve_returns_atoms_for_importable_and_errors_for_rest() {
        let mut store = ExternalRegistryStore::new();
        store.insert(RegistrySnapshot {
            snapshot_id: "s1".into(),
            registry: "local_cwl".into(),
            id: "good".into(),
            metadata: serde_json::json!({
                "id": "good", "label": "Good tool",
                "outputs": [{"id": "bam", "type": "edam:data_0863"}]
            }),
        });
        store.insert(RegistrySnapshot {
            snapshot_id: "s2".into(),
            registry: "local_cwl".into(),
            id: "bad".into(),
            metadata: serde_json::json!({}), // missing id -> MissingField
        });
        let importers = ImporterRegistry::with_builtin();
        let refs = [
            ExternalRegistryRef {
                registry: "local_cwl".into(),
                id: "good".into(),
                version: None,
                url: None,
            },
            ExternalRegistryRef {
                registry: "local_cwl".into(),
                id: "bad".into(),
                version: None,
                url: None,
            },
        ];
        let (atoms, errors) = store.resolve(&refs, &importers);
        assert_eq!(atoms.len(), 1);
        assert_eq!(atoms[0].id, "good");
        assert_eq!(errors.len(), 1);
        assert!(matches!(
            errors[0],
            ExternalImportError::MissingField { .. }
        ));
    }

    #[test]
    fn import_error_maps_to_blocker() {
        let e = ExternalImportError::SnapshotNotFound {
            snapshot_id: "snap-1".into(),
        };
        let blocker = e.into_blocker("local_cwl", "align_reads");
        match blocker {
            crate::blocker::BlockerKind::ExternalImportFailed {
                registry,
                id,
                reason,
            } => {
                assert_eq!(registry, "local_cwl");
                assert_eq!(id, "align_reads");
                assert!(reason.contains("snap-1"));
            }
            other => panic!("expected ExternalImportFailed, got {other:?}"),
        }
    }
}
