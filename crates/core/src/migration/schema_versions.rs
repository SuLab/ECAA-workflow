//! Schema-versions manifest authored at emit time.
//!
//! Each emitted package carries `runtime/schema-versions.json` listing
//! the SemVer of every IR type the package was produced with. Replay
//! consumers diff this manifest against their own
//! [`SchemaVersionsManifest::current()`] to decide which migrations
//! they need to run before consuming the package.

use semver::Version;
use serde::{Deserialize, Serialize};

/// Current SemVer of the [`WorkflowIntent`] IR. Bump on every breaking
/// shape change; `MAJOR.0.0` only when JSON deserialization breaks.
///
/// [`WorkflowIntent`]: crate::workflow_contracts::workflow_intent::WorkflowIntent
pub fn current_workflow_intent_version() -> Version {
    Version::new(1, 0, 0)
}

/// Current SemVer of the `Session` IR (lives in `conversation` crate;
/// `core` exposes the canonical version constant so the manifest can
/// stay byte-stable when read from a `core`-only context).
pub fn current_session_version() -> Version {
    Version::new(1, 0, 0)
}

/// Current SemVer of the [`WorkflowDag`] IR.
///
/// [`WorkflowDag`]: crate::workflow_contracts::task_node::WorkflowDag
pub fn current_workflow_dag_version() -> Version {
    Version::new(1, 0, 0)
}

/// Current SemVer of the `SessionLineage` IR.
pub fn current_session_lineage_version() -> Version {
    Version::new(1, 0, 0)
}

/// Current SemVer of the `DispatchRecord` (DispatchWAL) IR.
pub fn current_dispatch_wal_version() -> Version {
    Version::new(1, 0, 0)
}

/// Current SemVer of the `StatePatch` IR.
pub fn current_state_patch_version() -> Version {
    Version::new(0, 1, 0)
}

/// Current SemVer of the `DAG` IR (the DAG shape persisted as
/// `WORKFLOW.json`). The `version: String` field pre-dates SemVer
/// adoption and is preserved for back-compat; this constant tracks the
/// separate `schema_version` field added in §9.3.
pub fn current_dag_version() -> Version {
    Version::new(0, 1, 0)
}

/// Current SemVer of the `HarnessProgressEvent` wire shape.
pub fn current_harness_progress_event_version() -> Version {
    Version::new(0, 1, 0)
}

/// Current SemVer of the `OrphanReapWire` shape.
pub fn current_orphan_reap_wire_version() -> Version {
    Version::new(0, 1, 0)
}

/// Current SemVer of the `DecisionRecord` IR.
pub fn current_decision_record_schema_version() -> Version {
    Version::new(0, 1, 0)
}

/// On-disk shape of `runtime/schema-versions.json`. One entry per IR
/// type; future additions append (semver compatibility) rather than
/// renaming.
///
/// `ontology_snapshot` and `registry_snapshot` carry the optional
/// content-hashes of the ontology + adapter-registry catalogs used at
/// emit time (v4 substrate authoring populates these; v3 emit leaves
/// them `None`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SchemaVersionsManifest {
    /// `WorkflowIntent` JSON shape.
    pub workflow_intent: Version,
    /// `Session` JSON shape.
    pub session: Version,
    /// `WorkflowDag` IR shape.
    pub workflow_dag: Version,
    /// `SessionLineage` shape (carried inside child sessions).
    pub session_lineage: Version,
    /// `DispatchRecord` shape in `runtime/dispatch.wal.jsonl`.
    pub dispatch_wal: Version,
    /// `StatePatch` shape in `runtime/outputs/<id>/state.patch.json`.
    #[serde(default = "current_state_patch_version")]
    pub state_patch: Version,
    /// `DAG` shape in `WORKFLOW.json`.
    #[serde(default = "current_dag_version")]
    pub dag: Version,
    /// `HarnessProgressEvent` wire shape POSTed to the server.
    #[serde(default = "current_harness_progress_event_version")]
    pub harness_progress_event: Version,
    /// `OrphanReapWire` shape nested inside `HarnessProgressEvent`.
    #[serde(default = "current_orphan_reap_wire_version")]
    pub orphan_reap_wire: Version,
    /// `DecisionRecord` shape in `runtime/decisions.jsonl`.
    #[serde(default = "current_decision_record_schema_version")]
    pub decision_record: Version,
    /// Optional content hash of the ontology snapshot used at emit
    /// time. Populated by v4 substrate authoring; absent on v3 emits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ontology_snapshot: Option<String>,
    /// Optional content hash of the adapter/registry snapshot used at
    /// emit time. Populated by v4 substrate authoring; absent on v3
    /// emits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_snapshot: Option<String>,
}

impl SchemaVersionsManifest {
    /// Manifest representing the schema versions this build of the
    /// compiler emits. Updated whenever an IR type's schema-version
    /// constant bumps.
    pub fn current() -> Self {
        Self {
            workflow_intent: current_workflow_intent_version(),
            session: current_session_version(),
            workflow_dag: current_workflow_dag_version(),
            session_lineage: current_session_lineage_version(),
            dispatch_wal: current_dispatch_wal_version(),
            state_patch: current_state_patch_version(),
            dag: current_dag_version(),
            harness_progress_event: current_harness_progress_event_version(),
            orphan_reap_wire: current_orphan_reap_wire_version(),
            decision_record: current_decision_record_schema_version(),
            ontology_snapshot: None,
            registry_snapshot: None,
        }
    }
}

impl Default for SchemaVersionsManifest {
    fn default() -> Self {
        Self::current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_manifest_round_trips() {
        let m = SchemaVersionsManifest::current();
        let json = serde_json::to_string(&m).unwrap();
        let back: SchemaVersionsManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(m, back);
    }

    #[test]
    fn manifest_serializes_versions_as_semver_strings() {
        let m = SchemaVersionsManifest::current();
        let v = serde_json::to_value(&m).unwrap();
        // workflow_intent must serialize as a string ("1.0.0"), not a
        // number. The semver crate's `serde` feature handles this
        // automatically; the assertion documents the contract.
        assert!(v.get("workflow_intent").unwrap().is_string());
    }

    #[test]
    fn manifest_default_matches_current() {
        assert_eq!(
            SchemaVersionsManifest::default(),
            SchemaVersionsManifest::current()
        );
    }
}
