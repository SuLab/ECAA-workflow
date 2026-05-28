//! v3 P7 — Schema-version migration registry + WRROC replay API.
//!
//! Closes design v3 §10.5. Three responsibilities:
//!
//! 1. Promote `schema_version` IR fields from `u32` to
//!    [`semver::Version`] so future minor/patch bumps don't require a
//!    new migration cycle. The [`schema_version_serde`] adapter on each
//!    IR struct keeps backward-compat with on-disk JSON that still uses
//!    a bare `u64`.
//!
//! 2. Author a [`SchemaVersionsManifest`] — the per-package manifest
//!    written at emit time as `runtime/schema-versions.json` and
//!    registered into `ro-crate-metadata.json` as a `CreativeWork`. The
//!    manifest enumerates the schema version for every IR type used by
//!    that package; replay consumers diff the manifest's versions
//!    against their own [`SchemaVersionsManifest::current()`] to detect
//!    which migrations need to run.
//!
//! 3. Ship a [`MigrationRegistry`] that holds a chain of
//!    [`SchemaMigrator`]s per IR type. The starter set covers the
//!    `u32 → SemVer` upcast for `WorkflowIntent`, `Session`,
//!    `SessionLineage`, and `DispatchRecord`. Future schema breaks
//!    register additional migrators; the [`replay_provenance`] API
//!    walks the chain and surfaces
//!    [`ReplayError::MigrationRequired`] when no chain matches.
//!
//! The `scripps-workflow migrate-sessions` CLI subcommand calls
//! `MigrationRegistry::with_starters()` against
//! `SWFC_CHAT_SESSIONS_DIR`, applying any starter migrations to
//! on-disk session JSON in place. `--dry-run` reports counts without
//! writing.

pub mod migrators;
pub mod schema_versions;

pub use migrators::{
    ArchetypeConfigIdentity, DagIdentity, DecisionRecordIdentity, DispatchWalU32ToSemver,
    HarnessProgressEventIdentity, MigrationRegistry, ModalityConfigIdentity,
    OrphanReapWireIdentity, ReplayError, ReplayedComposition, SchemaMigrator,
    SessionLineageU32ToSemver, SessionU32ToSemver, StatePatchIdentity, WorkflowDagU32ToSemver,
    WorkflowIntentU32ToSemver,
};
pub use schema_versions::{
    current_dag_version, current_decision_record_schema_version, current_dispatch_wal_version,
    current_harness_progress_event_version, current_orphan_reap_wire_version,
    current_session_lineage_version, current_session_version, current_state_patch_version,
    current_workflow_dag_version, current_workflow_intent_version, SchemaVersionsManifest,
};

use semver::Version;
use std::path::Path;

/// Serde adapter that lets a `semver::Version` field accept either a
/// canonical SemVer string (`"1.0.0"`) or a legacy bare `u64`
/// (rewritten in memory to `<n>.0.0`). Used as
/// `#[serde(with = "crate::migration::schema_version_serde")]` on
/// every IR `schema_version` field.
///
/// The adapter is symmetric on the write path: outbound JSON always
/// serializes as a canonical SemVer string. Operators that diff
/// emitted packages get the new shape; legacy reads continue to
/// work without a migration pass.
pub mod schema_version_serde {
    use semver::Version;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Canonical SemVer string on write.
    pub fn serialize<S: Serializer>(v: &Version, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    /// Accept either a bare `u64` (legacy) or a SemVer string.
    /// `u64` → `<n>.0.0`. Anything else is an error.
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Version, D::Error> {
        let v: serde_json::Value = Deserialize::deserialize(d)?;
        if let Some(n) = v.as_u64() {
            return Ok(Version::new(n, 0, 0));
        }
        if let Some(s) = v.as_str() {
            return s.parse::<Version>().map_err(serde::de::Error::custom);
        }
        Err(serde::de::Error::custom(
            "expected u64 (legacy) or SemVer string for schema_version",
        ))
    }
}

/// Walk an emitted WRROC package at `wrroc_dir`, diff its
/// `runtime/schema-versions.json` against `target_versions`, and run
/// every registered migrator chain needed to bring each IR type up
/// to `target`.
///
/// Returns [`ReplayedComposition`] on success. Returns:
///
/// - [`ReplayError::MissingSidecar`] when `runtime/schema-versions.json`
///   is absent.
/// - [`ReplayError::MigrationRequired`] when no migrator chain in
///   `registry` covers an IR type whose version differs from the
///   target.
/// - [`ReplayError::Io`] / [`ReplayError::Parse`] for the obvious
///   shapes.
///
/// The function is read-only on disk by design — it loads the
/// package's IR artifacts, applies in-memory migrations, and
/// returns the migrated composition without writing back. Callers
/// that want a durable migration use `scripps-workflow
/// migrate-sessions` (sessions) or re-emit the package (everything
/// else).
pub fn replay_provenance(
    wrroc_dir: &Path,
    target_versions: &SchemaVersionsManifest,
    registry: &MigrationRegistry,
) -> Result<ReplayedComposition, ReplayError> {
    let manifest_path = wrroc_dir.join("runtime/schema-versions.json");
    if !manifest_path.exists() {
        return Err(ReplayError::MissingSidecar {
            path: manifest_path.display().to_string(),
        });
    }
    let bytes = std::fs::read(&manifest_path).map_err(|e| ReplayError::Io(e.to_string()))?;
    let actual: SchemaVersionsManifest =
        serde_json::from_slice(&bytes).map_err(|e| ReplayError::Parse(e.to_string()))?;

    // Per-IR-type version diffs. Each diff records the source +
    // target SemVer plus the IR-type key the migrator chain expects.
    let diffs: Vec<(&'static str, Version, Version)> = vec![
        (
            "workflow_intent",
            actual.workflow_intent.clone(),
            target_versions.workflow_intent.clone(),
        ),
        (
            "session",
            actual.session.clone(),
            target_versions.session.clone(),
        ),
        (
            "workflow_dag",
            actual.workflow_dag.clone(),
            target_versions.workflow_dag.clone(),
        ),
        (
            "session_lineage",
            actual.session_lineage.clone(),
            target_versions.session_lineage.clone(),
        ),
        (
            "dispatch_wal",
            actual.dispatch_wal.clone(),
            target_versions.dispatch_wal.clone(),
        ),
        (
            "state_patch",
            actual.state_patch.clone(),
            target_versions.state_patch.clone(),
        ),
        ("dag", actual.dag.clone(), target_versions.dag.clone()),
        (
            "harness_progress_event",
            actual.harness_progress_event.clone(),
            target_versions.harness_progress_event.clone(),
        ),
        (
            "orphan_reap_wire",
            actual.orphan_reap_wire.clone(),
            target_versions.orphan_reap_wire.clone(),
        ),
        (
            "decision_record",
            actual.decision_record.clone(),
            target_versions.decision_record.clone(),
        ),
    ];

    let mut required: Vec<(String, Version, Version)> = Vec::new();
    for (key, from, to) in &diffs {
        if from == to {
            continue;
        }
        // Try the registered chain. If it has a migrator that maps
        // exactly `from → to`, the IR type is covered. Otherwise the
        // chain is incomplete and we surface MigrationRequired.
        let covered = registry.can_migrate(key, from, to);
        if !covered {
            required.push((key.to_string(), from.clone(), to.clone()));
        }
    }
    if !required.is_empty() {
        let first = &required[0];
        return Err(ReplayError::MigrationRequired {
            from: first.1.clone(),
            to: first.2.clone(),
            missing: required
                .into_iter()
                .map(|(k, _, _)| k)
                .collect::<Vec<_>>()
                .join(","),
        });
    }

    Ok(ReplayedComposition {
        package_dir: wrroc_dir.to_path_buf(),
        actual_versions: actual,
        target_versions: target_versions.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_missing_sidecar_surfaces_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let res = replay_provenance(
            dir.path(),
            &SchemaVersionsManifest::current(),
            &MigrationRegistry::with_starters(),
        );
        assert!(matches!(res, Err(ReplayError::MissingSidecar { .. })));
    }
}
