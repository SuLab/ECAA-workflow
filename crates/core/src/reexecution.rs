//! Re-execution classification per PAR-26-040 §Aim 3A primary endpoint.
//!
//! Five buckets, in priority order (first match wins per artifact):
//! - `ByteIdentical`: SHA-256 of result artifact matches replay.
//! - `SemanticEquivalent`: per-modality numeric bounds satisfied. Bounds
//!   come from [`crate::reexecution_bounds::ModalityBoundsProvider`],
//!   resolved by the caller from the classified modality; the
//!   default-constructed `ModalityBounds` reproduces the historical ±5%
//!   relative band for unconfigured modalities. See [`classify_reexecution`].
//! - `AcknowledgedNonDeterminism`: artifact differs beyond the band, but the
//!   source package's `determinism-shim.json::non_deterministic_artifacts`
//!   declares a matching [`NonDetAck`](crate::determinism_shim::NonDetAck)
//!   that COVERS every diverging column (a whole-artifact ack, or a
//!   column-scoped ack listing all the diverged columns).
//! - `Unavailable`: replay artifact is missing.
//! - `Failed`: replay produced an error, diverges beyond
//!   semantic-equivalence bounds on an UN-acknowledged column, or diverges
//!   structurally (differing row/column shape) with no whole-artifact ack.
//!
//! Rec 1 (soundness): a blanket "the shim documents a no-seed / hashed source"
//! flag NO LONGER acknowledges arbitrary divergence — an out-of-band divergence
//! on a column with no matching `NonDetAck` FAILS. The `NonDetAck` set is the
//! single source shared with the audit-proof equivalence-failure invariant.
//!
//! The primary entry point is [`classify_reexecution`].

use crate::determinism_shim::{ack_for, DeterminismShimSidecar, NonDetAck};
use crate::hash_utils::sha256_hex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

/// Synthetic diverging-column identifier used when two tables differ in shape
/// (differing row count, or a row with a differing field count, or a parse
/// error). A structural divergence can only be acknowledged by a WHOLE-artifact
/// ack — a column-scoped ack never lists this token, so it stays `Failed`.
const STRUCTURE_SENTINEL: &str = "<table-shape>";

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

    // Absolute-path roots used to normalize text artifacts before the semantic
    // comparison. The recorded run baked its own package root into any output
    // that names a file (e.g. a validation `detail` column); the replay re-runs
    // under the scratch root. Rewriting both to a common placeholder keeps a
    // path-only difference from reading as an analytical divergence. Empty when
    // unknown (then normalization is a no-op).
    let recorded_root = crate::replay::read_recorded_env(parent_pkg).0;
    let scratch_root = replay_pkg.to_string_lossy().to_string();

    for (rel_path, path) in &parent_tables {
        // Resolve the replay file by the same relative path. Preserve the
        // existing fallback: when `path` is somehow not under `parent_pkg`,
        // join the relative string directly.
        let replay_path = replay_pkg.join(rel_path);

        let ac = classify_single_artifact(
            path,
            &replay_path,
            rel_path,
            shim.as_ref(),
            &bounds,
            &recorded_root,
            &scratch_root,
        );
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
#[allow(clippy::too_many_arguments)]
fn classify_single_artifact(
    parent_artifact: &Path,
    replay_artifact: &Path,
    rel_path: &str,
    shim: Option<&DeterminismShimSidecar>,
    bounds: &crate::reexecution_bounds::ModalityBounds,
    recorded_root: &str,
    scratch_root: &str,
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
    // as a divergent acknowledged outcome. The default is the historical ±5%
    // relative band. The delimiter is derived from the artifact extension
    // (Rec 2: `.csv` → comma, else tab) so a comma-delimited table is no longer
    // mis-parsed as a single tab-delimited column.
    // Path-normalize BEFORE the semantic comparison (but after the byte-identical
    // check above): a text artifact that embeds the absolute package root — a
    // validation-report `detail` column naming an input file, a logged output
    // path — differs between the recorded run and the replay only in that
    // environmental prefix. Rewrite the recorded run's root (in the parent) and
    // the replay scratch's root (in the replay) to a common placeholder so a
    // path-only difference classifies `semantic_equivalent`, not a spurious
    // `failed`. Mirrors the `recorded_root → scratch_root` rewrite the replay
    // applies to scripts. A byte-identical artifact never reaches here.
    let parent_norm = normalize_root(&parent_bytes, recorded_root);
    let replay_norm = normalize_root(&replay_bytes, scratch_root);
    let delimiter = delimiter_for(parent_artifact);
    let diverging = match check_semantic_equivalence(&parent_norm, &replay_norm, delimiter, bounds)
    {
        Ok(cols) => cols,
        // A parse failure (e.g. invalid UTF-8) is a structural divergence: only
        // a whole-artifact ack can cover it.
        Err(_e) => BTreeSet::from([STRUCTURE_SENTINEL.to_string()]),
    };

    if diverging.is_empty() {
        return ArtifactClassification {
            artifact_path: rel_path.to_string(),
            bucket: ReexecutionBucket::SemanticEquivalent,
            reason: Some(format!(
                "numeric columns within per-modality bounds (rel {:.3}, abs {:.4})",
                bounds.relative_tolerance, bounds.absolute_tolerance
            )),
        };
    }

    // AcknowledgedNonDeterminism: diverges beyond the semantic band on one or
    // more columns, and the parent's determinism shim declares a matching
    // `NonDetAck` that COVERS every diverging column. A whole-artifact ack
    // (`columns: None`) covers everything; a column-scoped ack covers only its
    // listed columns. This is the ONLY path to the acknowledged bucket — a
    // no-seed / hashed-source flag no longer masks an undeclared divergence.
    if let Some(shim) = shim {
        if let Some(ack) = ack_for(shim, rel_path) {
            if ack_covers(ack, &diverging) {
                return ArtifactClassification {
                    artifact_path: rel_path.to_string(),
                    bucket: ReexecutionBucket::AcknowledgedNonDeterminism,
                    reason: Some(format!(
                        "diverging column(s) [{}] acknowledged by determinism-shim.json ({:?}: {})",
                        diverging.iter().cloned().collect::<Vec<_>>().join(", "),
                        ack.kind,
                        ack.reason
                    )),
                };
            }
        }
    }

    // Failed: diverges on a column with no covering acknowledgment.
    ArtifactClassification {
        artifact_path: rel_path.to_string(),
        bucket: ReexecutionBucket::Failed,
        reason: Some(format!(
            "diverging column(s) [{}] exceed per-modality semantic-equivalence bounds with no covering non-determinism acknowledgment",
            diverging.iter().cloned().collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Derive the delimiter for a tabular artifact from its extension:
/// `.csv` → comma, `.tsv` / anything else → tab (the historical default).
fn delimiter_for(path: &Path) -> u8 {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("csv") => b',',
        _ => b'\t',
    }
}

/// Replace every occurrence of the absolute package root `root` with a stable
/// placeholder so a text artifact that embeds it (a validation `detail` path, a
/// logged output path) compares equal across the recorded run and the replay
/// scratch. No-op when `root` is empty or the bytes are not valid UTF-8 text (a
/// non-text artifact won't contain the root substring, and a binary buffer must
/// not be lossily rewritten). Used only for the semantic comparison, never the
/// byte-identical check.
fn normalize_root(bytes: &[u8], root: &str) -> Vec<u8> {
    if root.is_empty() {
        return bytes.to_vec();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) if s.contains(root) => s.replace(root, "<PKG_ROOT>").into_bytes(),
        _ => bytes.to_vec(),
    }
}

/// True when `ack` covers every diverging column identifier: a whole-artifact
/// ack (`columns: None`) covers all; a column-scoped ack covers a divergence
/// only when every diverging identifier is in its `columns` list.
fn ack_covers(ack: &NonDetAck, diverging: &BTreeSet<String>) -> bool {
    match &ack.columns {
        None => true,
        Some(cols) => {
            let covered: BTreeSet<&str> = cols.iter().map(String::as_str).collect();
            diverging.iter().all(|d| covered.contains(d.as_str()))
        }
    }
}

/// Column-aware semantic-equivalence check (Rec 1 + Rec 2). Parses BOTH sides
/// with the `csv` crate using the supplied `delimiter` (so quoted fields with
/// embedded delimiters are handled correctly), then compares cell-by-cell:
/// numeric cells must be within `bounds`; non-numeric cells must match exactly
/// (case-insensitive trim).
///
/// Returns the SET of diverging column identifiers — the header name when the
/// first row is a header (any non-numeric cell in row 0), else the 0-based
/// column index as a string. An empty set means fully equivalent. A structural
/// mismatch (differing row count, or a row with a differing field count) yields
/// the [`STRUCTURE_SENTINEL`] token. `Err` only on a reader error.
fn check_semantic_equivalence(
    parent: &[u8],
    replay: &[u8],
    delimiter: u8,
    bounds: &crate::reexecution_bounds::ModalityBounds,
) -> Result<BTreeSet<String>, String> {
    let parent_rows = parse_delimited(parent, delimiter)?;
    let replay_rows = parse_delimited(replay, delimiter)?;

    let mut diverging: BTreeSet<String> = BTreeSet::new();

    if parent_rows.len() != replay_rows.len() {
        diverging.insert(STRUCTURE_SENTINEL.to_string());
        return Ok(diverging);
    }

    // Header presence: the first parent row is a header when it carries any
    // cell that does not parse as a number.
    let has_header = parent_rows
        .first()
        .map(|r| r.iter().any(|c| c.trim().parse::<f64>().is_err()))
        .unwrap_or(false);

    let col_id = |idx: usize| -> String {
        if has_header {
            parent_rows[0]
                .get(idx)
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| idx.to_string())
        } else {
            idx.to_string()
        }
    };

    for (row_idx, (pr, rr)) in parent_rows.iter().zip(replay_rows.iter()).enumerate() {
        if pr.len() != rr.len() {
            diverging.insert(STRUCTURE_SENTINEL.to_string());
            continue;
        }
        for (col_idx, (pc, rc)) in pr.iter().zip(rr.iter()).enumerate() {
            let pc = pc.trim();
            let rc = rc.trim();
            // Header row: exact (case-insensitive) match required per cell.
            if has_header && row_idx == 0 {
                if !pc.eq_ignore_ascii_case(rc) {
                    diverging.insert(col_id(col_idx));
                }
                continue;
            }
            match (pc.parse::<f64>(), rc.parse::<f64>()) {
                (Ok(pv), Ok(rv)) => {
                    if !bounds.within(pv, rv) {
                        diverging.insert(col_id(col_idx));
                    }
                }
                // Both non-numeric: exact (case-insensitive) match required.
                (Err(_), Err(_)) => {
                    if !pc.eq_ignore_ascii_case(rc) {
                        diverging.insert(col_id(col_idx));
                    }
                }
                // One numeric, one not: divergent.
                _ => {
                    diverging.insert(col_id(col_idx));
                }
            }
        }
    }
    Ok(diverging)
}

/// Parse delimited bytes into rows of owned string fields using the `csv`
/// crate. `has_headers(false)` keeps every row (we detect the header
/// ourselves); `flexible(true)` tolerates ragged rows so a shape mismatch is
/// surfaced as divergence rather than a hard parse error.
fn parse_delimited(bytes: &[u8], delimiter: u8) -> Result<Vec<Vec<String>>, String> {
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(bytes);
    let mut rows: Vec<Vec<String>> = Vec::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|e| e.to_string())?;
        rows.push(rec.iter().map(str::to_string).collect());
    }
    Ok(rows)
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

    #[test]
    fn normalize_root_rewrites_embedded_package_path() {
        let b = b"check,status,detail\nx,PASS,/runs/abc/runtime/outputs/y.csv\n";
        assert_eq!(
            normalize_root(b, "/runs/abc"),
            b"check,status,detail\nx,PASS,<PKG_ROOT>/runtime/outputs/y.csv\n".to_vec()
        );
        // No-op on empty root or a root not present in the bytes.
        assert_eq!(normalize_root(b, ""), b.to_vec());
        assert_eq!(normalize_root(b, "/other/root"), b.to_vec());
    }

    /// A validation report whose only difference is the absolute package root
    /// embedded in a `detail` column must classify `semantic_equivalent` after
    /// per-side root normalization — not a spurious `failed`. Without
    /// normalization the same difference diverges.
    #[test]
    fn path_only_divergence_is_semantic_equivalent_after_normalization() {
        let parent =
            b"check,status,detail\nm,PASS,/runs/orig/runtime/outputs/m.csv\nn,PASS,rows=57\n";
        let replay =
            b"check,status,detail\nm,PASS,/scratch/xyz/runtime/outputs/m.csv\nn,PASS,rows=57\n";
        let bounds = ModalityBounds::default();

        let raw = check_semantic_equivalence(parent, replay, b',', &bounds).unwrap();
        assert!(!raw.is_empty(), "raw absolute-path difference should diverge");

        let pn = normalize_root(parent, "/runs/orig");
        let rn = normalize_root(replay, "/scratch/xyz");
        let normed = check_semantic_equivalence(&pn, &rn, b',', &bounds).unwrap();
        assert!(
            normed.is_empty(),
            "path-only difference must not diverge after normalization; got {normed:?}"
        );
    }

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

    /// Build a full determinism-shim JSON string with the given
    /// `non_deterministic_artifacts` array literal spliced in.
    fn shim_json_with_acks(acks: &str) -> String {
        format!(
            "{{\"schema_version\":\"1\",\"env_capture\":{{\"captured_env_vars\":[\"LANG\"],\
             \"redacted_env_vars\":[]}},\"seed_policy\":{{\"random_seed\":null,\
             \"seed_source\":\"process-default\"}},\"temp_path_policy\":{{\
             \"strategy\":\"stable-by-task-id\",\"root\":\"runtime/scratch\"}},\
             \"locale\":\"en_US.UTF-8\",\"timezone\":\"UTC\",\"ablation_engaged\":false,\
             \"non_deterministic_artifacts\":{acks}}}"
        )
    }

    #[test]
    fn semantic_equivalent_beats_acknowledged_when_shim_present() {
        // Regression: a within-band reproduction must classify SemanticEquivalent
        // even when the parent's determinism shim declares a NonDetAck for the
        // artifact — the ack is never consulted when nothing diverges.
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "runtime/outputs/differential_expression/de_results.tsv";
        write_file(&parent.path().join(rel), "gene\tlog2fc\nGENE1\t100.0\n");
        write_file(&replay.path().join(rel), "gene\tlog2fc\nGENE1\t102.0\n"); // 2%, in band
        write_file(
            &parent.path().join("runtime/determinism-shim.json"),
            &shim_json_with_acks(
                "[{\"artifact\":\"de_results.tsv\",\"columns\":[\"log2fc\"],\
                 \"kind\":\"adaptive_shrinkage\",\"reason\":\"apeglm shrinkage\"}]",
            ),
        );

        let report =
            classify_reexecution(parent.path(), replay.path(), None, ModalityBounds::default())
                .expect("classify_reexecution must succeed");
        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::SemanticEquivalent,
            "within-band change must be SemanticEquivalent even with a NonDetAck present, got {:?} ({:?})",
            ac.bucket,
            ac.reason
        );
    }

    #[test]
    fn out_of_band_with_shim_is_acknowledged() {
        // Beyond the semantic band, a NonDetAck that COVERS the diverging column
        // yields AcknowledgedNonDeterminism.
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "runtime/outputs/pathway_enrichment/enrichment.tsv";
        write_file(&parent.path().join(rel), "pathway\tnes\nP1\t1.0\n");
        write_file(&replay.path().join(rel), "pathway\tnes\nP1\t2.0\n"); // 100%, out of band
        write_file(
            &parent.path().join("runtime/determinism-shim.json"),
            &shim_json_with_acks(
                "[{\"artifact\":\"enrichment.tsv\",\"columns\":[\"nes\"],\
                 \"kind\":\"unseeded_rng\",\"reason\":\"GSEA permutation seed unset\"}]",
            ),
        );

        let report =
            classify_reexecution(parent.path(), replay.path(), None, ModalityBounds::default())
                .expect("classify_reexecution must succeed");
        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::AcknowledgedNonDeterminism,
            "out-of-band change on a covered column must be Acknowledged, got {:?}",
            ac.bucket
        );
    }

    #[test]
    fn out_of_band_without_matching_ack_fails() {
        // SOUNDNESS: an out-of-band divergence on a column with NO covering ack
        // must FAIL — even when a no-seed shim is present. Previously the blanket
        // no-seed / hashed-source flag masked this as AcknowledgedNonDeterminism.
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "runtime/outputs/pathway_enrichment/enrichment.tsv";
        write_file(&parent.path().join(rel), "pathway\tnes\nP1\t1.0\n");
        write_file(&replay.path().join(rel), "pathway\tnes\nP1\t2.0\n"); // 100%, out of band
                                                                         // No-seed shim, but NO NonDetAck for this artifact.
        write_file(
            &parent.path().join("runtime/determinism-shim.json"),
            &shim_json_with_acks("[]"),
        );

        let report =
            classify_reexecution(parent.path(), replay.path(), None, ModalityBounds::default())
                .expect("classify_reexecution must succeed");
        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::Failed,
            "un-acknowledged out-of-band divergence must FAIL under a no-seed shim, got {:?} ({:?})",
            ac.bucket,
            ac.reason
        );
    }

    #[test]
    fn column_scoped_ack_fails_on_unacked_column() {
        // COLUMN-SCOPED: an ack that covers only `log2FoldChange` acknowledges a
        // divergence there, but a divergence in an un-acked column (`stat`) must
        // still FAIL.
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "runtime/outputs/differential_expression/de_results.tsv";
        // log2FoldChange diverges (acked); stat diverges (NOT acked).
        write_file(
            &parent.path().join(rel),
            "gene\tlog2FoldChange\tstat\nGENE1\t1.00\t3.00\n",
        );
        write_file(
            &replay.path().join(rel),
            "gene\tlog2FoldChange\tstat\nGENE1\t2.00\t9.00\n",
        );
        write_file(
            &parent.path().join("runtime/determinism-shim.json"),
            &shim_json_with_acks(
                "[{\"artifact\":\"de_results.tsv\",\"columns\":[\"log2FoldChange\"],\
                 \"kind\":\"adaptive_shrinkage\",\"reason\":\"apeglm shrinkage\"}]",
            ),
        );

        let report =
            classify_reexecution(parent.path(), replay.path(), None, ModalityBounds::default())
                .expect("classify_reexecution must succeed");
        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::Failed,
            "divergence in an un-acked column must FAIL, got {:?} ({:?})",
            ac.bucket,
            ac.reason
        );
    }

    #[test]
    fn column_scoped_ack_acknowledges_when_only_acked_column_diverges() {
        // COLUMN-SCOPED (positive): only the acked `log2FoldChange` column
        // diverges → AcknowledgedNonDeterminism.
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "runtime/outputs/differential_expression/de_results.tsv";
        write_file(
            &parent.path().join(rel),
            "gene\tlog2FoldChange\tstat\nGENE1\t1.00\t3.00\n",
        );
        write_file(
            &replay.path().join(rel),
            "gene\tlog2FoldChange\tstat\nGENE1\t2.00\t3.00\n",
        );
        write_file(
            &parent.path().join("runtime/determinism-shim.json"),
            &shim_json_with_acks(
                "[{\"artifact\":\"de_results.tsv\",\"columns\":[\"log2FoldChange\"],\
                 \"kind\":\"adaptive_shrinkage\",\"reason\":\"apeglm shrinkage\"}]",
            ),
        );

        let report =
            classify_reexecution(parent.path(), replay.path(), None, ModalityBounds::default())
                .expect("classify_reexecution must succeed");
        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::AcknowledgedNonDeterminism,
            "divergence only in the acked column must be Acknowledged, got {:?} ({:?})",
            ac.bucket,
            ac.reason
        );
    }

    #[test]
    fn csv_within_band_is_semantic_equivalent() {
        // CSV (Rec 2): a comma-delimited `.csv` with a within-band numeric cell
        // must classify SemanticEquivalent — it must NOT be parsed as a single
        // tab-delimited column (which would make the whole row one string cell
        // and diverge).
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "runtime/outputs/differential_expression/de_results.csv";
        write_file(&parent.path().join(rel), "gene,log2FC\nGENE1,2.00\n");
        write_file(&replay.path().join(rel), "gene,log2FC\nGENE1,2.05\n"); // 2.5%, in band
        let report =
            classify_reexecution(parent.path(), replay.path(), None, ModalityBounds::default())
                .expect("classify_reexecution must succeed");
        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::SemanticEquivalent,
            "within-band comma-delimited CSV must be SemanticEquivalent, got {:?} ({:?})",
            ac.bucket,
            ac.reason
        );
    }

    #[test]
    fn csv_quoted_field_with_embedded_comma_parses_correctly() {
        // CSV (Rec 2): a quoted field containing a comma must be parsed as a
        // single field by the csv crate, so column alignment is preserved and a
        // within-band numeric value still classifies SemanticEquivalent.
        let parent = tempfile::tempdir().expect("parent tempdir");
        let replay = tempfile::tempdir().expect("replay tempdir");
        let rel = "runtime/outputs/differential_expression/de_results.csv";
        write_file(
            &parent.path().join(rel),
            "gene,note,log2FC\nGENE1,\"hello, world\",2.00\n",
        );
        write_file(
            &replay.path().join(rel),
            "gene,note,log2FC\nGENE1,\"hello, world\",2.05\n",
        );
        let report =
            classify_reexecution(parent.path(), replay.path(), None, ModalityBounds::default())
                .expect("classify_reexecution must succeed");
        let ac = classification_for(&report, rel);
        assert_eq!(
            ac.bucket,
            ReexecutionBucket::SemanticEquivalent,
            "quoted CSV field with embedded comma must parse correctly, got {:?} ({:?})",
            ac.bucket,
            ac.reason
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
