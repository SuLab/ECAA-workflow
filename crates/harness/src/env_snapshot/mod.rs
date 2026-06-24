//! Compute-environment snapshot — shared types consumed by Tasks 2–6.
//!
//! `cache_scan` detects whether the session cache already contains installed
//! packages before the snapshot build is attempted.

use std::path::PathBuf;

pub mod cache_scan;

/// Options controlling whether and where a snapshot is captured.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotOpts {
    pub enabled: bool,
    pub registry: Option<String>,
    pub base_digest: String,
    pub source_date_epoch: i64,
    pub cache_dir: PathBuf,
}

/// Where a captured snapshot was stored.
#[derive(Debug, Clone, PartialEq)]
pub enum StoreLocation {
    Registry(String),
    LocalCas(PathBuf),
}

/// Outcome of a snapshot attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum SnapshotOutcome {
    Captured {
        digest: String,
        location: StoreLocation,
        note: Option<String>,
    },
    SkippedNoInstalls,
    Failed {
        reason: String,
    },
}
