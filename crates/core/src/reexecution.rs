//! Re-execution classification per PAR-26-040 §Aim 3A primary endpoint.
//!
//! Five buckets, in priority order (first match wins per artifact):
//! - `ByteIdentical`: SHA-256 of result artifact matches replay.
//! - `SemanticEquivalent`: per-modality bounds satisfied. The placeholder
//!   implementation checks that numeric columns are within ±5% relative
//!   tolerance; actual per-modality bounds will be supplied by E20 via
//!   the `ModalityBoundsProvider` injection point. See
//!   [`classify_reexecution`] for the E20 hook comment.
//! - `AcknowledgedNonDeterminism`: artifact differs but the source package's
//!   `determinism-shim.json::env_capture` records a known non-determinism
//!   source (e.g. `PYTHONHASHSEED` absent from captured vars, or
//!   `random_seed` absent from `seed_policy`).
//! - `Unavailable`: replay artifact is missing.
//! - `Failed`: replay produced an error or output that diverges beyond
//!   semantic-equivalence bounds.
//!
//! The primary entry point is [`classify_reexecution`].

use crate::determinism_shim::DeterminismShimSidecar;
use crate::hash_utils::sha256_hex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

/// The five re-execution buckets per PAR-26-040 §Aim 3A primary endpoint.
///
/// Canonical definition lives in `scripps-workflow-ecaa-types::reexecution`.
/// Re-exported here for backward compatibility with existing call sites.
pub use scripps_workflow_ecaa_types::ReexecutionBucket;

/// Report aggregating per-artifact bucket assignments across a replay pair.
///
/// Written to `runtime/reexecution.json` by
/// `crates/conversation/src/emit/sidecars::write_reexecution_sidecar`.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct ReexecutionReport {
    /// Schema version.
    pub schema_version: String,
    /// Counts per bucket name (snake_case). `BTreeMap` for deterministic
    /// JSON key ordering.
    pub bucket_counts: BTreeMap<String, usize>,
    /// Per artifact.
    pub per_artifact: Vec<ArtifactClassification>,
}

/// Bucket assignment for a single artifact path.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct ArtifactClassification {
    /// Artifact path.
    pub artifact_path: String,
    /// Bucket.
    pub bucket: ReexecutionBucket,
    /// Reason.
    pub reason: Option<String>,
}

impl ReexecutionReport {
    /// Return an empty report used when ablation is engaged or no parent
    /// package is available.
    pub fn empty(schema_version: &str) -> Self {
        Self {
            schema_version: schema_version.to_string(),
            bucket_counts: BTreeMap::new(),
            per_artifact: vec![],
        }
    }

    /// Recompute `bucket_counts` from `per_artifact`. Called internally after
    /// classification is complete.
    fn finalize_counts(&mut self) {
        self.bucket_counts.clear();
        for ac in &self.per_artifact {
            let key = serde_json::to_value(&ac.bucket)
                .ok()
                .and_then(|v| v.as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown".to_string());
            *self.bucket_counts.entry(key).or_insert(0) += 1;
        }
    }
}

/// Classify every `results/tables/*.{csv,tsv}` artifact found in `parent_pkg`
/// by comparing it against the corresponding file in `replay_pkg`.
///
/// `policy_path` is the optional path to a `determinism-shim.json` sidecar
/// from the parent package. When `None`, the function looks for
/// `<parent_pkg>/runtime/determinism-shim.json` automatically.
///
/// **E20 hook:** per-modality semantic bounds are currently a placeholder
/// (±5% relative tolerance on numeric columns). Once E20 lands the
/// `ModalityBoundsProvider` registry, replace the call to
/// [`check_semantic_equivalence_placeholder`] with a modality-dispatch
/// against the registry. The function signature here is intentionally
/// left open for that injection.
pub fn classify_reexecution(
    parent_pkg: &Path,
    replay_pkg: &Path,
    policy_path: Option<&Path>,
) -> io::Result<ReexecutionReport> {
    // Load the determinism shim from the parent package to detect
    // acknowledged non-determinism sources.
    let shim = load_determinism_shim(parent_pkg, policy_path);

    let tables_dir = parent_pkg.join("results").join("tables");
    if !tables_dir.exists() {
        return Ok(ReexecutionReport {
            schema_version: "0.1".to_string(),
            bucket_counts: BTreeMap::new(),
            per_artifact: vec![],
        });
    }

    let mut classifications: Vec<ArtifactClassification> = vec![];

    for entry in fs::read_dir(&tables_dir)? {
        let entry = entry?;
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext != "csv" && ext != "tsv" {
            continue;
        }

        let rel_path = path
            .strip_prefix(parent_pkg)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        let replay_path = replay_pkg.join(
            path.strip_prefix(parent_pkg)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| {
                    Path::new("results")
                        .join("tables")
                        .join(path.file_name().unwrap_or_default())
                }),
        );

        let ac = classify_single_artifact(&path, &replay_path, &rel_path, shim.as_ref());
        classifications.push(ac);
    }

    let mut report = ReexecutionReport {
        schema_version: "0.1".to_string(),
        bucket_counts: BTreeMap::new(),
        per_artifact: classifications,
    };
    report.finalize_counts();
    Ok(report)
}

/// Classify a single artifact by comparing `parent_artifact` to `replay_artifact`.
fn classify_single_artifact(
    parent_artifact: &Path,
    replay_artifact: &Path,
    rel_path: &str,
    shim: Option<&DeterminismShimSidecar>,
) -> ArtifactClassification {
    // Unavailable: replay artifact missing.
    if !replay_artifact.exists() {
        return ArtifactClassification {
            artifact_path: rel_path.to_string(),
            bucket: ReexecutionBucket::Unavailable,
            reason: Some("replay artifact missing".to_string()),
        };
    }

    // Read both files; a read error on the replay side → Failed.
    let parent_bytes = match fs::read(parent_artifact) {
        Ok(b) => b,
        Err(e) => {
            return ArtifactClassification {
                artifact_path: rel_path.to_string(),
                bucket: ReexecutionBucket::Failed,
                reason: Some(format!("failed to read parent artifact: {e}")),
            };
        }
    };
    let replay_bytes = match fs::read(replay_artifact) {
        Ok(b) => b,
        Err(e) => {
            return ArtifactClassification {
                artifact_path: rel_path.to_string(),
                bucket: ReexecutionBucket::Failed,
                reason: Some(format!("failed to read replay artifact: {e}")),
            };
        }
    };

    // ByteIdentical: SHA-256 match.
    if sha256_hex(&parent_bytes) == sha256_hex(&replay_bytes) {
        return ArtifactClassification {
            artifact_path: rel_path.to_string(),
            bucket: ReexecutionBucket::ByteIdentical,
            reason: None,
        };
    }

    // AcknowledgedNonDeterminism: differs but a known non-determinism
    // source is declared in the parent's determinism shim.
    if let Some(shim) = shim {
        if has_acknowledged_nondeterminism(shim) {
            return ArtifactClassification {
                artifact_path: rel_path.to_string(),
                bucket: ReexecutionBucket::AcknowledgedNonDeterminism,
                reason: Some(
                    "differs but non-determinism source documented in determinism-shim.json"
                        .to_string(),
                ),
            };
        }
    }

    // SemanticEquivalent: placeholder ±5% relative tolerance on numeric
    // columns. E20 replaces this call with modality-dispatched bounds.
    match check_semantic_equivalence_placeholder(&parent_bytes, &replay_bytes) {
        Ok(true) => ArtifactClassification {
            artifact_path: rel_path.to_string(),
            bucket: ReexecutionBucket::SemanticEquivalent,
            reason: Some(
                "numeric columns within ±5% relative tolerance (placeholder; E20 supplies per-modality bounds)"
                    .to_string(),
            ),
        },
        Ok(false) => ArtifactClassification {
            artifact_path: rel_path.to_string(),
            bucket: ReexecutionBucket::Failed,
            reason: Some("numeric divergence exceeds semantic-equivalence bounds".to_string()),
        },
        Err(e) => ArtifactClassification {
            artifact_path: rel_path.to_string(),
            bucket: ReexecutionBucket::Failed,
            reason: Some(format!("semantic equivalence check error: {e}")),
        },
    }
}

/// Placeholder semantic-equivalence check: every numeric cell in the replay
/// must be within ±5% relative tolerance of the corresponding parent cell.
/// Non-numeric cells must match exactly (case-insensitive trim).
///
/// Returns `Ok(true)` when all cells satisfy the bounds, `Ok(false)` when
/// any cell diverges, and `Err` on parse failure.
///
/// **E20 hook:** replace this function's logic with modality-dispatched
/// bounds loaded via `ModalityBoundsProvider` — see [`classify_reexecution`].
fn check_semantic_equivalence_placeholder(parent: &[u8], replay: &[u8]) -> Result<bool, String> {
    let parent_str = std::str::from_utf8(parent).map_err(|e| e.to_string())?;
    let replay_str = std::str::from_utf8(replay).map_err(|e| e.to_string())?;

    let parent_rows: Vec<Vec<&str>> = parent_str
        .lines()
        .map(|l| l.split('\t').collect())
        .collect();
    let replay_rows: Vec<Vec<&str>> = replay_str
        .lines()
        .map(|l| l.split('\t').collect())
        .collect();

    if parent_rows.len() != replay_rows.len() {
        return Ok(false);
    }

    for (pr, rr) in parent_rows.iter().zip(replay_rows.iter()) {
        if pr.len() != rr.len() {
            return Ok(false);
        }
        for (pc, rc) in pr.iter().zip(rr.iter()) {
            let pc = pc.trim();
            let rc = rc.trim();
            // Try numeric comparison first.
            match (pc.parse::<f64>(), rc.parse::<f64>()) {
                (Ok(pv), Ok(rv)) => {
                    if !numerics_within_tolerance(pv, rv, 0.05) {
                        return Ok(false);
                    }
                }
                // Both non-numeric: exact (case-insensitive) match required.
                (Err(_), Err(_)) => {
                    if !pc.eq_ignore_ascii_case(rc) {
                        return Ok(false);
                    }
                }
                // One numeric, one not: divergent.
                _ => return Ok(false),
            }
        }
    }
    Ok(true)
}

/// Check whether `|a - b| / max(|a|, |b|, 1e-9)` is within `tolerance`.
/// Handles the zero-denominator case (when both are ~0, they are equivalent).
fn numerics_within_tolerance(a: f64, b: f64, tolerance: f64) -> bool {
    if a == b {
        return true;
    }
    let denom = a.abs().max(b.abs()).max(1e-9);
    (a - b).abs() / denom <= tolerance
}

/// Returns `true` when the shim records a known source of non-determinism:
/// - `PYTHONHASHSEED` is absent from `captured_env_vars` (not set at
///   emit time, meaning Python hash randomization was active), or
/// - `seed_policy.random_seed` is `None` (no explicit seed was committed).
fn has_acknowledged_nondeterminism(shim: &DeterminismShimSidecar) -> bool {
    let pythonhashseed_absent = !shim
        .env_capture
        .captured_env_vars
        .iter()
        .any(|v| v == "PYTHONHASHSEED");
    let random_seed_absent = shim.seed_policy.random_seed.is_none();
    pythonhashseed_absent || random_seed_absent
}

/// Load the determinism shim from the parent package's runtime directory, or
/// from `explicit_path` when provided. Soft-returns `None` on any error
/// (missing file, parse error) — the classification continues without it.
fn load_determinism_shim(
    parent_pkg: &Path,
    explicit_path: Option<&Path>,
) -> Option<DeterminismShimSidecar> {
    let path = match explicit_path {
        Some(p) => p.to_path_buf(),
        None => parent_pkg.join("runtime").join("determinism-shim.json"),
    };
    let bytes = fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}
