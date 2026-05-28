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
pub mod registry_improvement;

pub use local_cwl::LocalCwlImporter;
pub use registry_improvement::{
    aggregate_unknowns, aggregate_unknowns_from_inputs, AggregatorInput, RegistryImprovementSignal,
};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

use crate::ingestion_safety::IngestionSafetyReport;
use crate::workflow_contracts::task_node::TaskNode;

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
