//! `scripps-workflow migrate-sessions` — apply v3 P7's `u32 → SemVer`
//! migrations to on-disk session JSON in place.
//!
//! Walks the session directory (default
//! `$ECAA_CHAT_SESSIONS_DIR` or `$HOME/.scripps-workflow/sessions`),
//! detects each session's current `schema_version` shape, and applies
//! the registered starter chain (`MigrationRegistry::with_starters()`)
//! when an upgrade is needed. `--dry-run` reports counts without
//! writing back.

use anyhow::{Context, Result};
use clap::Args;
use ecaa_workflow_core::migration::{current_session_version, MigrationRegistry};
use semver::Version;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub(crate) struct MigrateSessionsArgs {
    /// Override the sessions directory. Default:
    /// `$ECAA_CHAT_SESSIONS_DIR` or `$HOME/.scripps-workflow/sessions`.
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Walk + report counts only; do not write back.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Default)]
struct MigrationCounts {
    migrated: usize,
    skipped_already_current: usize,
    skipped_unrecognized_shape: usize,
    io_errors: usize,
}

pub(crate) fn run(args: MigrateSessionsArgs) -> Result<()> {
    let dir = args.dir.unwrap_or_else(default_sessions_dir);
    if !dir.exists() {
        println!("sessions dir not present (skipping): {}", dir.display());
        return Ok(());
    }
    let registry = MigrationRegistry::with_starters();
    let target = current_session_version();
    let mut counts = MigrationCounts::default();

    for entry in std::fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                counts.io_errors += 1;
                continue;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        match migrate_one(&path, &registry, &target, args.dry_run) {
            Ok(MigrateOutcome::Migrated) => counts.migrated += 1,
            Ok(MigrateOutcome::AlreadyCurrent) => counts.skipped_already_current += 1,
            Ok(MigrateOutcome::UnrecognizedShape) => counts.skipped_unrecognized_shape += 1,
            Err(_) => counts.io_errors += 1,
        }
    }

    println!(
        "migrate-sessions: dir={} migrated={} already-current={} skipped-unrecognized={} io-errors={} (dry_run={})",
        dir.display(),
        counts.migrated,
        counts.skipped_already_current,
        counts.skipped_unrecognized_shape,
        counts.io_errors,
        args.dry_run,
    );
    Ok(())
}

enum MigrateOutcome {
    Migrated,
    AlreadyCurrent,
    UnrecognizedShape,
}

fn migrate_one(
    path: &std::path::Path,
    registry: &MigrationRegistry,
    target: &Version,
    dry_run: bool,
) -> Result<MigrateOutcome> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let mut value: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return Ok(MigrateOutcome::UnrecognizedShape),
    };
    let detected = detect_session_version(&value);
    let from = match detected {
        Some(v) => v,
        None => return Ok(MigrateOutcome::UnrecognizedShape),
    };
    if &from == target {
        return Ok(MigrateOutcome::AlreadyCurrent);
    }
    let migrated = match registry.apply("session", &from, target, value.clone()) {
        Some(m) => m,
        None => return Ok(MigrateOutcome::UnrecognizedShape),
    };
    value = migrated;
    if !dry_run {
        let serialized = serde_json::to_vec_pretty(&value)
            .with_context(|| format!("serialize {}", path.display()))?;
        ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(path, &serialized)
            .with_context(|| format!("atomic write {}", path.display()))?;
    }
    Ok(MigrateOutcome::Migrated)
}

/// Read the on-disk `schema_version` field as either a bare `u64`
/// (legacy) or a SemVer string. Returns `None` when the value's shape
/// is unrecognised.
fn detect_session_version(value: &serde_json::Value) -> Option<Version> {
    let v = value.get("schema_version")?;
    if let Some(n) = v.as_u64() {
        return Some(Version::new(n, 0, 0));
    }
    if let Some(s) = v.as_str() {
        return s.parse::<Version>().ok();
    }
    None
}

fn default_sessions_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("ECAA_CHAT_SESSIONS_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(".scripps-workflow/sessions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_legacy_u32_is_n_zero_zero() {
        let v = serde_json::json!({ "schema_version": 3 });
        assert_eq!(detect_session_version(&v), Some(Version::new(3, 0, 0)));
    }

    #[test]
    fn detect_semver_string_parses() {
        let v = serde_json::json!({ "schema_version": "2.1.0" });
        assert_eq!(detect_session_version(&v), Some(Version::new(2, 1, 0)));
    }

    #[test]
    fn detect_missing_field_returns_none() {
        let v = serde_json::json!({ "id": "abc" });
        assert!(detect_session_version(&v).is_none());
    }

    #[test]
    fn detect_garbage_string_returns_none() {
        let v = serde_json::json!({ "schema_version": "not a version" });
        assert!(detect_session_version(&v).is_none());
    }

    #[test]
    fn migrate_one_rewrites_legacy_u32_session_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        let content = serde_json::json!({
            "id": "s_test",
            "schema_version": 0,
            "intake_prose": "test"
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&content).unwrap()).unwrap();
        let registry = MigrationRegistry::with_starters();
        let target = Version::new(1, 0, 0);
        let res = migrate_one(&path, &registry, &target, false).unwrap();
        assert!(matches!(res, MigrateOutcome::Migrated));
        // File now carries a SemVer string.
        let back: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            back.get("schema_version").and_then(|v| v.as_str()),
            Some("1.0.0"),
        );
    }

    #[test]
    fn migrate_one_dry_run_leaves_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        let original = serde_json::json!({
            "id": "s_test",
            "schema_version": 0,
            "intake_prose": "test"
        });
        let original_bytes = serde_json::to_vec_pretty(&original).unwrap();
        std::fs::write(&path, &original_bytes).unwrap();
        let registry = MigrationRegistry::with_starters();
        let target = Version::new(1, 0, 0);
        let res = migrate_one(&path, &registry, &target, true).unwrap();
        assert!(matches!(res, MigrateOutcome::Migrated));
        // Bytes unchanged on disk because --dry-run.
        let back = std::fs::read(&path).unwrap();
        assert_eq!(back, original_bytes);
    }

    #[test]
    fn migrate_one_already_current_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.json");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "id": "s_test",
                "schema_version": "1.0.0",
            }))
            .unwrap(),
        )
        .unwrap();
        let registry = MigrationRegistry::with_starters();
        let target = Version::new(1, 0, 0);
        let res = migrate_one(&path, &registry, &target, false).unwrap();
        assert!(matches!(res, MigrateOutcome::AlreadyCurrent));
    }

    #[test]
    fn run_handles_missing_dir() {
        let args = MigrateSessionsArgs {
            dir: Some(PathBuf::from("/nonexistent/path/sw-migrate-test")),
            dry_run: true,
        };
        // Must not error out — missing dir is treated as "nothing
        // to do".
        run(args).unwrap();
    }
}
