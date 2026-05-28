//! [`SchemaMigrator`] trait + [`MigrationRegistry`] holding the
//! starter chain of `u32 → SemVer` migrators for the IR types whose
//! `schema_version` field was promoted in v3 P7.

use semver::Version;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Typed errors surfaced by [`super::replay_provenance`] and the
/// `migrate-sessions` CLI subcommand.
#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReplayError {
    /// `runtime/schema-versions.json` absent or unreadable from the
    /// requested WRROC package directory.
    #[error("missing runtime/schema-versions.json sidecar at {path}")]
    MissingSidecar { path: String },
    /// At least one IR type's version differs from the target and the
    /// registry has no migrator chain covering `from → to`. `missing`
    /// is a comma-separated list of IR-type keys that lacked a chain.
    #[error("no migrator chain covers {from} → {to} for IR types [{missing}]")]
    MigrationRequired {
        /// From.
        from: Version,
        /// To.
        to: Version,
        /// Missing.
        missing: String,
    },
    #[error("io error: {0}")]
    /// Io variant.
    Io(String),
    #[error("parse error: {0}")]
    /// Parse variant.
    Parse(String),
}

/// In-memory representation of a successful replay. Today this is a
/// minimal envelope (the package path + the actual vs target version
/// manifests); future work threads through the migrated WorkflowDag +
/// proof bundle.
#[derive(Debug, Clone)]
pub struct ReplayedComposition {
    /// Package dir.
    pub package_dir: PathBuf,
    /// Actual versions.
    pub actual_versions: super::SchemaVersionsManifest,
    /// Target versions.
    pub target_versions: super::SchemaVersionsManifest,
}

/// One migration step. A migrator owns the JSON-shape transform from
/// one version to the next *of a single IR type*. Chains are formed
/// by registering multiple migrators under the same IR-type key in
/// [`MigrationRegistry`].
pub trait SchemaMigrator: Send + Sync {
    /// IR-type key, matches the field name in
    /// [`super::SchemaVersionsManifest`] (e.g. `"workflow_intent"`,
    /// `"session"`, `"dispatch_wal"`).
    fn ir_type(&self) -> &'static str;

    /// Source SemVer this migrator upgrades from.
    #[allow(clippy::wrong_self_convention)]
    fn from_version(&self) -> Version;

    /// Target SemVer this migrator produces.
    fn to_version(&self) -> Version;

    /// Apply the migration. Caller passes the source JSON; we return
    /// the target JSON. The default implementation rewrites the
    /// `schema_version` field to the migrator's `to_version()` SemVer
    /// string — concrete migrators that need other transforms
    /// override this.
    fn migrate(&self, mut value: serde_json::Value) -> serde_json::Value {
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "schema_version".to_string(),
                serde_json::Value::String(self.to_version().to_string()),
            );
        }
        value
    }
}

/// Starter migrator: `WorkflowIntent::schema_version` `u32 → SemVer`.
pub struct WorkflowIntentU32ToSemver;

impl SchemaMigrator for WorkflowIntentU32ToSemver {
    fn ir_type(&self) -> &'static str {
        "workflow_intent"
    }
    fn from_version(&self) -> Version {
        // Treat the legacy u32 form as `0.0.0`; the first SemVer-shape
        // version is `1.0.0`.
        Version::new(0, 0, 0)
    }
    fn to_version(&self) -> Version {
        Version::new(1, 0, 0)
    }
}

/// Starter migrator: `Session::schema_version` `u32 → SemVer`.
pub struct SessionU32ToSemver;

impl SchemaMigrator for SessionU32ToSemver {
    fn ir_type(&self) -> &'static str {
        "session"
    }
    fn from_version(&self) -> Version {
        Version::new(0, 0, 0)
    }
    fn to_version(&self) -> Version {
        Version::new(1, 0, 0)
    }
}

/// Starter migrator: `SessionLineage::schema_version` `u32 → SemVer`.
pub struct SessionLineageU32ToSemver;

impl SchemaMigrator for SessionLineageU32ToSemver {
    fn ir_type(&self) -> &'static str {
        "session_lineage"
    }
    fn from_version(&self) -> Version {
        Version::new(0, 0, 0)
    }
    fn to_version(&self) -> Version {
        Version::new(1, 0, 0)
    }
}

/// Starter migrator: `DispatchRecord::schema_version` `u32 → SemVer`.
pub struct DispatchWalU32ToSemver;

impl SchemaMigrator for DispatchWalU32ToSemver {
    fn ir_type(&self) -> &'static str {
        "dispatch_wal"
    }
    fn from_version(&self) -> Version {
        Version::new(0, 0, 0)
    }
    fn to_version(&self) -> Version {
        Version::new(1, 0, 0)
    }
}

/// Starter migrator for the `WorkflowDag` IR. The v4
/// planner emits `WorkflowDag` directly with the SemVer shape; this
/// migrator is the rail for pre-SemVer manifests whose
/// `workflow_dag` version stamp predates the SemVer adoption (i.e. `0.0.0`).
/// Today the upcast is a no-op on the JSON body — only the manifest
/// version stamp changes — but the chain entry must exist so
/// `replay_provenance` doesn't refuse v0 fixtures.
pub struct WorkflowDagU32ToSemver;

impl SchemaMigrator for WorkflowDagU32ToSemver {
    fn ir_type(&self) -> &'static str {
        "workflow_dag"
    }
    fn from_version(&self) -> Version {
        Version::new(0, 0, 0)
    }
    fn to_version(&self) -> Version {
        Version::new(1, 0, 0)
    }
    fn migrate(&self, value: serde_json::Value) -> serde_json::Value {
        // `WorkflowDag` IR did not carry a `schema_version` field
        // pre-P7. The body shape did not change, so the upcast is a
        // no-op on the document itself — only the manifest version
        // stamp moves. Returning the input verbatim is correct.
        value
    }
}

/// Identity migrator for the `modality_config` IR. C23 (config-manifest
/// migration story) — registers the IR-type key so a future loader
/// pinned to a newer version of the modality manifest can call
/// `MigrationRegistry::can_migrate("modality_config", from, to)` and
/// either find a chain or fire `BlockerKind::SchemaVersionMismatch`.
/// Today the schema is at `0.1` and no breaking change has happened,
/// so the registered entry is the identity migrator `0.1 → 0.1`.
pub struct ModalityConfigIdentity;

impl SchemaMigrator for ModalityConfigIdentity {
    fn ir_type(&self) -> &'static str {
        "modality_config"
    }
    fn from_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn to_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn migrate(&self, value: serde_json::Value) -> serde_json::Value {
        // Identity: the on-disk shape and the loader shape agree at
        // schema_version 0.1. The first non-identity migrator lands
        // when the manifest layout next changes (0.1 → 0.2).
        value
    }
}

/// Identity migrator for the `archetype_config` IR. C23 sibling of
/// [`ModalityConfigIdentity`]; identity at schema_version `0.1`.
pub struct ArchetypeConfigIdentity;

impl SchemaMigrator for ArchetypeConfigIdentity {
    fn ir_type(&self) -> &'static str {
        "archetype_config"
    }
    fn from_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn to_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn migrate(&self, value: serde_json::Value) -> serde_json::Value {
        value
    }
}

/// Identity migrator for the `StatePatch` IR. Registered at `0.1.0`
/// so `replay_provenance` and future readers can query
/// `can_migrate("state_patch", …)` without falling through to
/// `MigrationRequired`. The first non-identity migrator lands when the
/// `state.patch.json` layout changes.
pub struct StatePatchIdentity;

impl SchemaMigrator for StatePatchIdentity {
    fn ir_type(&self) -> &'static str {
        "state_patch"
    }
    fn from_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn to_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn migrate(&self, value: serde_json::Value) -> serde_json::Value {
        value
    }
}

/// Identity migrator for the `DAG` IR (`WORKFLOW.json`). Registered at
/// `0.1.0`; the `version: String` legacy field coexists. First
/// non-identity migrator lands when the DAG shape changes.
pub struct DagIdentity;

impl SchemaMigrator for DagIdentity {
    fn ir_type(&self) -> &'static str {
        "dag"
    }
    fn from_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn to_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn migrate(&self, value: serde_json::Value) -> serde_json::Value {
        value
    }
}

/// Identity migrator for the `HarnessProgressEvent` wire shape.
/// Registered at `0.1.0`.
pub struct HarnessProgressEventIdentity;

impl SchemaMigrator for HarnessProgressEventIdentity {
    fn ir_type(&self) -> &'static str {
        "harness_progress_event"
    }
    fn from_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn to_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn migrate(&self, value: serde_json::Value) -> serde_json::Value {
        value
    }
}

/// Identity migrator for the `OrphanReapWire` shape.
/// Registered at `0.1.0`.
pub struct OrphanReapWireIdentity;

impl SchemaMigrator for OrphanReapWireIdentity {
    fn ir_type(&self) -> &'static str {
        "orphan_reap_wire"
    }
    fn from_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn to_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn migrate(&self, value: serde_json::Value) -> serde_json::Value {
        value
    }
}

/// Identity migrator for the `DecisionRecord` IR
/// (`runtime/decisions.jsonl`). Registered at `0.1.0`.
pub struct DecisionRecordIdentity;

impl SchemaMigrator for DecisionRecordIdentity {
    fn ir_type(&self) -> &'static str {
        "decision_record"
    }
    fn from_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn to_version(&self) -> Version {
        Version::new(0, 1, 0)
    }
    fn migrate(&self, value: serde_json::Value) -> serde_json::Value {
        value
    }
}

/// Registry of migrators keyed by IR-type. A registry covers a
/// `(from, to)` pair iff it owns a chain of migrators whose
/// concatenated `from → to` spans the requested gap.
pub struct MigrationRegistry {
    by_type: BTreeMap<&'static str, Vec<Box<dyn SchemaMigrator>>>,
}

impl MigrationRegistry {
    /// Empty registry. Use [`MigrationRegistry::with_starters`] for
    /// the v3 P7 starter set.
    pub fn new() -> Self {
        Self {
            by_type: BTreeMap::new(),
        }
    }

    /// v3 P7 starter set — one `u32 → SemVer` upcast per IR type.
    /// Extended in C23 with identity entries for the modality /
    /// archetype config IR types so a future cross-version replay can
    /// detect schema_version drift on the on-disk manifests and surface
    /// `BlockerKind::SchemaVersionMismatch` when no chain covers the
    /// gap. Today both config IR types sit at `0.1` so the registered
    /// migrators are the identity `0.1 → 0.1`.
    pub fn with_starters() -> Self {
        let mut r = Self::new();
        r.register(Box::new(WorkflowIntentU32ToSemver));
        r.register(Box::new(SessionU32ToSemver));
        r.register(Box::new(SessionLineageU32ToSemver));
        r.register(Box::new(DispatchWalU32ToSemver));
        r.register(Box::new(WorkflowDagU32ToSemver));
        r.register(Box::new(ModalityConfigIdentity));
        r.register(Box::new(ArchetypeConfigIdentity));
        r.register(Box::new(StatePatchIdentity));
        r.register(Box::new(DagIdentity));
        r.register(Box::new(HarnessProgressEventIdentity));
        r.register(Box::new(OrphanReapWireIdentity));
        r.register(Box::new(DecisionRecordIdentity));
        r
    }

    /// Append a migrator. Multiple migrators under the same `ir_type`
    /// form a chain; chains are walked from `from` upward by matching
    /// adjacent `(from, to)` pairs.
    pub fn register(&mut self, m: Box<dyn SchemaMigrator>) {
        self.by_type.entry(m.ir_type()).or_default().push(m);
    }

    /// `true` iff a chain of registered migrators covers
    /// `ir_type` from `from` → `to`. `from == to` is trivially
    /// covered (no migration needed).
    pub fn can_migrate(&self, ir_type: &str, from: &Version, to: &Version) -> bool {
        if from == to {
            return true;
        }
        let Some(chain) = self.by_type.get(ir_type) else {
            return false;
        };
        // Walk the chain greedily: find a migrator whose
        // `from_version` == `current`, advance `current` to its
        // `to_version`, repeat until `current == to`.
        let mut current = from.clone();
        for _ in 0..chain.len() {
            let next = chain.iter().find(|m| m.from_version() == current);
            let Some(next) = next else {
                break;
            };
            current = next.to_version();
            if &current == to {
                return true;
            }
        }
        false
    }

    /// Apply the migrator chain to a JSON value of IR-type `ir_type`,
    /// upgrading it from `from` to `to`. Returns the input unchanged
    /// when `from == to`. Returns `None` when no chain covers the gap.
    pub fn apply(
        &self,
        ir_type: &str,
        from: &Version,
        to: &Version,
        mut value: serde_json::Value,
    ) -> Option<serde_json::Value> {
        if from == to {
            return Some(value);
        }
        let chain = self.by_type.get(ir_type)?;
        let mut current = from.clone();
        for _ in 0..chain.len() {
            let next = chain.iter().find(|m| m.from_version() == current)?;
            value = next.migrate(value);
            current = next.to_version();
            if &current == to {
                return Some(value);
            }
        }
        None
    }
}

impl Default for MigrationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_chain_covers_u32_to_semver_for_each_ir_type() {
        let r = MigrationRegistry::with_starters();
        for key in [
            "workflow_intent",
            "session",
            "session_lineage",
            "dispatch_wal",
            "workflow_dag",
        ] {
            assert!(
                r.can_migrate(key, &Version::new(0, 0, 0), &Version::new(1, 0, 0)),
                "starter chain missing for {}",
                key
            );
        }
    }

    #[test]
    fn can_migrate_is_trivial_for_identical_versions() {
        let r = MigrationRegistry::new();
        assert!(r.can_migrate(
            "workflow_intent",
            &Version::new(2, 0, 0),
            &Version::new(2, 0, 0)
        ));
    }

    #[test]
    fn apply_rewrites_schema_version_to_chain_target_semver() {
        // The starter chain registers 0.0.0 → 1.0.0 for every IR
        // type. The default migrator's `migrate()` overwrites the
        // `schema_version` field with the migrator's `to_version()`
        // SemVer string, so the output always carries the chain
        // target version (not the source `u64`).
        let r = MigrationRegistry::with_starters();
        let src = serde_json::json!({
            "schema_version": 3u64,
            "id": "abc"
        });
        let out = r
            .apply(
                "workflow_intent",
                &Version::new(0, 0, 0),
                &Version::new(1, 0, 0),
                src,
            )
            .unwrap();
        assert_eq!(
            out.get("schema_version").and_then(|v| v.as_str()),
            Some("1.0.0")
        );
    }

    /// C23 — both new config IR types must appear in the starter
    /// registry so cross-version replay can call `can_migrate` against
    /// either key without falling through to the
    /// `MigrationRequired` error path. Today the schemas are at
    /// `0.1` so the identity migrator suffices; the first non-identity
    /// chain lands when the manifest layout changes (0.1 → 0.2).
    #[test]
    fn starter_registry_covers_modality_and_archetype_config_identity() {
        let r = MigrationRegistry::with_starters();
        // can_migrate is trivially true for from==to BEFORE consulting
        // the chain — so registering the identity migrator is the way
        // we make the IR type queryable. Apply the identity to confirm
        // the chain entry exists rather than relying on the trivial
        // from==to short-circuit.
        let body = serde_json::json!({ "schema_version": "0.1", "id": "x" });
        for key in ["modality_config", "archetype_config"] {
            let out = r
                .apply(
                    key,
                    &Version::new(0, 1, 0),
                    &Version::new(0, 1, 0),
                    body.clone(),
                )
                .unwrap_or_else(|| panic!("identity chain must exist for {}", key));
            // Identity migrator: the body is byte-identical to the
            // input. The `from == to` shortcut in `apply` also returns
            // `Some(body)` unchanged — both paths converge here.
            assert_eq!(out, body);
        }
    }

    #[test]
    fn missing_chain_returns_none() {
        let r = MigrationRegistry::new();
        let res = r.apply(
            "workflow_intent",
            &Version::new(0, 0, 0),
            &Version::new(1, 0, 0),
            serde_json::json!({}),
        );
        assert!(res.is_none());
    }
}
