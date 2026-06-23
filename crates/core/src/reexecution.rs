//! Re-execution classification per PAR-26-040 §Aim 3A primary endpoint.
//!
//! Five buckets, in priority order (first match wins per artifact):
//! - `ByteIdentical`: SHA-256 of result artifact matches replay.
//! - `SemanticEquivalent`: per-modality numeric bounds satisfied. Bounds
//!   come from [`crate::reexecution_bounds::ModalityBoundsProvider`],
//!   resolved by the caller from the classified modality; the
//!   default-constructed `ModalityBounds` reproduces the historical ±5%
//!   relative band for unconfigured modalities. See [`classify_reexecution`].
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
/// Canonical definition lives in `ecaa-workflow-types::reexecution`.
/// Re-exported here for backward compatibility with existing call sites.
pub use ecaa_workflow_types::ReexecutionBucket;

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
    pub(crate) fn finalize_counts(&mut self) {
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

/// Classify every result table found in `parent_pkg` by comparing it
/// against the corresponding file in `replay_pkg`.
///
/// Candidate `*.{csv,tsv}` tables are gathered from BOTH locations,
/// deduplicated by their path relative to `parent_pkg`:
/// - `<parent_pkg>/runtime/outputs/` scanned **recursively** — the real
///   location, where per-task subdirs hold tables like
///   `runtime/outputs/differential_expression/de_results.tsv`, AND
/// - `<parent_pkg>/results/tables/` scanned **non-recursively** —
///   legacy/forward-compat, kept working.
///
/// When NEITHER location yields a table, an empty report is returned
/// (the historical behavior for an absent `results/tables`).
///
/// `policy_path` is the optional path to a `determinism-shim.json` sidecar
/// from the parent package. When `None`, the function looks for
/// `<parent_pkg>/runtime/determinism-shim.json` automatically.
///
/// `bounds` carries the per-modality semantic-equivalence tolerance,
/// resolved by the caller from `ModalityBoundsProvider` (the
/// default-constructed [`crate::reexecution_bounds::ModalityBounds`]
/// reproduces the historical ±5% relative placeholder). Callers without
/// a classified modality pass `ModalityBounds::default()`.
pub fn classify_reexecution(
    parent_pkg: &Path,
    replay_pkg: &Path,
    policy_path: Option<&Path>,
    bounds: crate::reexecution_bounds::ModalityBounds,
) -> io::Result<ReexecutionReport> {
    // Load the determinism shim from the parent package to detect
    // acknowledged non-determinism sources.
    let shim = load_determinism_shim(parent_pkg, policy_path);

    // Gather candidate parent tables from both locations, deduplicated by
    // their path relative to `parent_pkg`. `BTreeMap` keeps the per-artifact
    // ordering deterministic across runs (a hard dependency for the
    // byte-reproducibility contract).
    let mut parent_tables: BTreeMap<String, std::path::PathBuf> = BTreeMap::new();

    // (1) runtime/outputs/ — recursive (the real location).
    collect_tables_recursive(
        &parent_pkg.join("runtime").join("outputs"),
        parent_pkg,
        &mut parent_tables,
    )?;

    // (2) results/tables/ — non-recursive (legacy/forward-compat).
    let legacy_dir = parent_pkg.join("results").join("tables");
    if legacy_dir.exists() {
        for entry in fs::read_dir(&legacy_dir)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || !is_table_ext(&path) {
                continue;
            }
            insert_table(&path, parent_pkg, &mut parent_tables);
        }
    }

    // Neither location yielded a table — preserve the empty-report behavior.
    if parent_tables.is_empty() {
        return Ok(ReexecutionReport {
            schema_version: "0.1".to_string(),
            bucket_counts: BTreeMap::new(),
            per_artifact: vec![],
        });
    }

    let mut classifications: Vec<ArtifactClassification> = vec![];

    for (rel_path, path) in &parent_tables {
        // Resolve the replay file by the same relative path. Preserve the
        // existing fallback: when `path` is somehow not under `parent_pkg`,
        // join the relative string directly.
        let replay_path = replay_pkg.join(rel_path);

        let ac = classify_single_artifact(path, &replay_path, rel_path, shim.as_ref(), &bounds);
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

/// `true` when `path` has a `.csv`/`.tsv` extension (case-insensitive).
fn is_table_ext(path: &Path) -> bool {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    ext == "csv" || ext == "tsv"
}

/// Insert one parent table into `out`, keyed by its path relative to
/// `parent_pkg`. The fallback (path not under `parent_pkg`) keys by the
/// file name so the entry is never silently dropped.
fn insert_table(path: &Path, parent_pkg: &Path, out: &mut BTreeMap<String, std::path::PathBuf>) {
    let rel_path = path
        .strip_prefix(parent_pkg)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(path.file_name().unwrap_or_default())
        })
        .to_string_lossy()
        .to_string();
    out.insert(rel_path, path.to_path_buf());
}

/// Recursively collect `*.{csv,tsv}` files under `dir` into `out`, keyed by
/// their path relative to `parent_pkg`. A missing `dir` is a no-op (the
/// always-emits discipline: an absent runtime/outputs must not error).
fn collect_tables_recursive(
    dir: &Path,
    parent_pkg: &Path,
    out: &mut BTreeMap<String, std::path::PathBuf>,
) -> io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_tables_recursive(&path, parent_pkg, out)?;
        } else if file_type.is_file() && is_table_ext(&path) {
            insert_table(&path, parent_pkg, out);
        }
    }
    Ok(())
}

/// Classify a single artifact by comparing `parent_artifact` to `replay_artifact`.
fn classify_single_artifact(
    parent_artifact: &Path,
    replay_artifact: &Path,
    rel_path: &str,
    shim: Option<&DeterminismShimSidecar>,
    bounds: &crate::reexecution_bounds::ModalityBounds,
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

    // SemanticEquivalent: numeric cells within per-modality bounds. Checked
    // BEFORE AcknowledgedNonDeterminism on purpose: a within-band reproduction
    // is a NON-divergent outcome (spec §5.6, "byte_identical / semantic_equivalent
    // / unavailable are non-divergent and need no ack") and must not be relabeled
    // as a divergent acknowledged outcome merely because the parent shim happens
    // to document a non-determinism source. The default is the historical ±5%
    // relative band.
    match check_semantic_equivalence(&parent_bytes, &replay_bytes, bounds) {
        Ok(true) => {
            return ArtifactClassification {
                artifact_path: rel_path.to_string(),
                bucket: ReexecutionBucket::SemanticEquivalent,
                reason: Some(format!(
                    "numeric columns within per-modality bounds (rel {:.3}, abs {:.4})",
                    bounds.relative_tolerance, bounds.absolute_tolerance
                )),
            };
        }
        // Diverges beyond the band, or is not numerically comparable: fall
        // through to the acknowledged-source / hard-failure decision.
        Ok(false) | Err(_) => {}
    }

    // AcknowledgedNonDeterminism: diverges beyond the semantic band but a known
    // non-determinism source is declared in the parent's determinism shim.
    if let Some(shim) = shim {
        if has_acknowledged_nondeterminism(shim) {
            return ArtifactClassification {
                artifact_path: rel_path.to_string(),
                bucket: ReexecutionBucket::AcknowledgedNonDeterminism,
                reason: Some(
                    "differs beyond semantic bounds but a non-determinism source is documented in determinism-shim.json"
                        .to_string(),
                ),
            };
        }
    }

    // Failed: diverges beyond bounds with no acknowledged non-determinism source.
    ArtifactClassification {
        artifact_path: rel_path.to_string(),
        bucket: ReexecutionBucket::Failed,
        reason: Some(
            "numeric divergence exceeds per-modality semantic-equivalence bounds with no acknowledged non-determinism source".to_string(),
        ),
    }
}

/// Semantic-equivalence check: every numeric cell in the replay must be
/// within the supplied per-modality `bounds` of the corresponding parent
/// cell. Non-numeric cells must match exactly (case-insensitive trim).
///
/// Returns `Ok(true)` when all cells satisfy the bounds, `Ok(false)` when
/// any cell diverges, and `Err` on parse failure. The default-constructed
/// `bounds` reproduces the historical ±5% relative band.
fn check_semantic_equivalence(
    parent: &[u8],
    replay: &[u8],
    bounds: &crate::reexecution_bounds::ModalityBounds,
) -> Result<bool, String> {
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
                    if !bounds.within(pv, rv) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reexecution_bounds::ModalityBounds;

    /// Helper: write a file, creating parent dirs as needed.
    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs for test fixture");
        }
        fs::write(path, contents).expect("write test fixture file");
    }

    /// Locate the single artifact classification matching `rel_path`.
    fn classification_for<'a>(
        report: &'a ReexecutionReport,
        rel_path: &str,
    ) -> &'a ArtifactClassification {
        report
            .per_artifact
            .iter()
            .find(|ac| ac.artifact_path == rel_path)
            .unwrap_or_else(|| panic!("no classification for {rel_path} in {report:?}"))
    }

    #[test]
    fn runtime_outputs_tables_are_compared() {
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "runtime/outputs/differential_expression/de_results.tsv";
        let body = "gene\tlog2fc\tpadj\nGENE1\t1.5\t0.01\nGENE2\t-2.0\t0.04\n";
        write_file(&parent.path().join(rel), body);
        // Byte-identical replay copy.
        write_file(&replay.path().join(rel), body);

        let report = classify_reexecution(
            parent.path(),
            replay.path(),
            None,
            ModalityBounds::default(),
        )
        .expect("classify_reexecution must succeed");

        assert!(
            !report.per_artifact.is_empty(),
            "runtime/outputs tables must be compared, got empty report: {report:?}"
        );
        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::ByteIdentical,
            "identical runtime/outputs table must classify ByteIdentical, got {:?}",
            ac.bucket
        );
    }

    #[test]
    fn semantic_equivalent_within_band() {
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "runtime/outputs/differential_expression/de_results.tsv";
        // Parent value 100.0; replay value 102.0 → 2% change, inside the
        // default ±5% relative band but not byte-identical.
        write_file(
            &parent.path().join(rel),
            "gene\tlog2fc\nGENE1\t100.0\n",
        );
        write_file(
            &replay.path().join(rel),
            "gene\tlog2fc\nGENE1\t102.0\n",
        );

        let report = classify_reexecution(
            parent.path(),
            replay.path(),
            None,
            ModalityBounds::default(),
        )
        .expect("classify_reexecution must succeed");

        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::SemanticEquivalent,
            "within-band numeric change must classify SemanticEquivalent, got {:?} ({:?})",
            ac.bucket,
            ac.reason
        );
    }

    // Shim JSON that triggers `has_acknowledged_nondeterminism`
    // (random_seed: null, PYTHONHASHSEED not captured) — mirrors the deposited
    // Himes package's determinism-shim.json structure so it deserializes.
    const ACK_SHIM_JSON: &str = "{\"schema_version\":\"1\",\"env_capture\":{\"captured_env_vars\":[\"LANG\"],\"redacted_env_vars\":[]},\"seed_policy\":{\"random_seed\":null,\"seed_source\":\"process-default\"},\"temp_path_policy\":{\"strategy\":\"stable-by-task-id\",\"root\":\"runtime/scratch\"},\"locale\":\"en_US.UTF-8\",\"timezone\":\"UTC\",\"ablation_engaged\":false}";

    #[test]
    fn semantic_equivalent_beats_acknowledged_when_shim_present() {
        // Regression: a within-band reproduction must classify SemanticEquivalent
        // even when the parent's determinism shim documents a non-determinism
        // source. Previously the shim short-circuited every non-identical table
        // to AcknowledgedNonDeterminism before the semantic check could run.
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "runtime/outputs/differential_expression/de_results.tsv";
        write_file(&parent.path().join(rel), "gene\tlog2fc\nGENE1\t100.0\n");
        write_file(&replay.path().join(rel), "gene\tlog2fc\nGENE1\t102.0\n"); // 2%, in band
        write_file(&parent.path().join("runtime/determinism-shim.json"), ACK_SHIM_JSON);

        let report =
            classify_reexecution(parent.path(), replay.path(), None, ModalityBounds::default())
                .expect("classify_reexecution must succeed");
        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::SemanticEquivalent,
            "within-band change must be SemanticEquivalent even with a non-determinism shim, got {:?} ({:?})",
            ac.bucket,
            ac.reason
        );
    }

    #[test]
    fn out_of_band_with_shim_is_acknowledged() {
        // Negative: beyond the semantic band, a documented non-determinism
        // source still yields AcknowledgedNonDeterminism (fallback preserved).
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "runtime/outputs/pathway_enrichment/enrichment.tsv";
        write_file(&parent.path().join(rel), "pathway\tnes\nP1\t1.0\n");
        write_file(&replay.path().join(rel), "pathway\tnes\nP1\t2.0\n"); // 100%, out of band
        write_file(&parent.path().join("runtime/determinism-shim.json"), ACK_SHIM_JSON);

        let report =
            classify_reexecution(parent.path(), replay.path(), None, ModalityBounds::default())
                .expect("classify_reexecution must succeed");
        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::AcknowledgedNonDeterminism,
            "out-of-band change with a non-determinism shim must be Acknowledged, got {:?}",
            ac.bucket
        );
    }

    #[test]
    fn replay_missing_table_is_unavailable() {
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "runtime/outputs/differential_expression/de_results.tsv";
        write_file(
            &parent.path().join(rel),
            "gene\tlog2fc\nGENE1\t1.0\n",
        );
        // Replay deliberately lacks the file.

        let report = classify_reexecution(
            parent.path(),
            replay.path(),
            None,
            ModalityBounds::default(),
        )
        .expect("classify_reexecution must succeed");

        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::Unavailable,
            "missing replay table must classify Unavailable, got {:?}",
            ac.bucket
        );
    }

    #[test]
    fn legacy_results_tables_still_scanned() {
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "results/tables/x.tsv";
        let body = "gene\tvalue\nGENE1\t42\n";
        write_file(&parent.path().join(rel), body);
        write_file(&replay.path().join(rel), body);

        let report = classify_reexecution(
            parent.path(),
            replay.path(),
            None,
            ModalityBounds::default(),
        )
        .expect("classify_reexecution must succeed");

        assert!(
            !report.per_artifact.is_empty(),
            "legacy results/tables must still be scanned, got empty report: {report:?}"
        );
        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::ByteIdentical,
            "identical legacy table must classify ByteIdentical, got {:?}",
            ac.bucket
        );
    }

    #[test]
    fn no_tables_anywhere_is_empty() {
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        // Neither runtime/outputs nor results/tables exists.

        let report = classify_reexecution(
            parent.path(),
            replay.path(),
            None,
            ModalityBounds::default(),
        )
        .expect("classify_reexecution must succeed");

        assert!(
            report.per_artifact.is_empty(),
            "no tables anywhere must yield empty per_artifact, got {report:?}"
        );
    }
}
