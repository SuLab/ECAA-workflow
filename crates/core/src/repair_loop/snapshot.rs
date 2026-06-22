//! Pre-repair snapshot of the package's WRITABLE surface, so a failed or
//! regressive repair round can be rolled back byte-for-byte.
//!
//! The snapshot captures ONLY files the repair loop is permitted to write:
//! per-task narratives (`report.md` / `*.txt`), `claims_evidence_matrix.csv`,
//! `ro-crate-metadata.json`, the BagIt manifests, and `decisions.jsonl` under
//! `runtime/`, plus the root-level `ro-crate-metadata.json` and
//! `manifest-sha512.txt`. It NEVER captures or restores frozen result tables
//! (`*.tsv` / arbitrary `*.csv` under `results/` or `runtime/outputs/`) — those
//! are inputs to verification and must remain immutable across a repair round.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Names captured wherever they appear under `runtime/` (recursively).
const RUNTIME_CAPTURE_NAMES: &[&str] = &[
    "report.md",
    "claims_evidence_matrix.csv",
    "ro-crate-metadata.json",
    "manifest-sha512.txt",
    "tagmanifest-sha512.txt",
    "decisions.jsonl",
];

/// Root-level files captured (the package-level RO-Crate + payload manifest).
const ROOT_CAPTURE_NAMES: &[&str] = &["ro-crate-metadata.json", "manifest-sha512.txt"];

/// True when `name` is a writable file the snapshot should capture under
/// `runtime/`: one of the named artifacts OR any `.txt` narrative. Result
/// tables (`*.tsv`, `*.csv` other than the evidence matrix) are excluded.
fn is_runtime_capture(name: &str) -> bool {
    RUNTIME_CAPTURE_NAMES.contains(&name) || name.ends_with(".txt")
}

/// An in-memory copy of the package's writable surface, keyed by path relative
/// to the package root.
pub struct Snapshot {
    files: BTreeMap<PathBuf, Vec<u8>>,
}

impl Snapshot {
    /// Number of files captured. Useful for assertions/diagnostics.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// True when nothing was captured.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Rewrite every captured file back to disk at `root`, restoring the
    /// pre-repair bytes. Consumes the snapshot — a snapshot is single-use.
    /// Files NOT captured (e.g. frozen result tables) are left untouched.
    pub fn rollback(self, root: &Path) -> anyhow::Result<()> {
        for (rel, bytes) in self.files {
            let dest = root.join(&rel);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, &bytes)?;
        }
        Ok(())
    }
}

/// Captures and restores the writable surface of a package.
pub struct Snapshotter;

impl Snapshotter {
    /// Capture the writable surface of the package at `root` into memory.
    /// NEVER reads result tables (`*.tsv` / non-evidence `*.csv`).
    pub fn take(root: &Path) -> anyhow::Result<Snapshot> {
        let mut files = BTreeMap::new();

        // Recursive walk of runtime/ for the named/`.txt` writable artifacts.
        let runtime = root.join("runtime");
        if runtime.is_dir() {
            capture_runtime(root, &runtime, &mut files)?;
        }

        // Root-level package artifacts.
        for name in ROOT_CAPTURE_NAMES {
            let path = root.join(name);
            if path.is_file() {
                let bytes = std::fs::read(&path)?;
                files.insert(PathBuf::from(name), bytes);
            }
        }

        Ok(Snapshot { files })
    }
}

/// Recursively walk `dir` (under the package `root`), capturing writable files.
/// `dir` is always inside `root`, so the strip_prefix is infallible in practice;
/// a defensive fallback keeps an absolute path rather than panicking.
fn capture_runtime(
    root: &Path,
    dir: &Path,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            capture_runtime(root, &path, files)?;
            continue;
        }
        if !file_type.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !is_runtime_capture(name) {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.clone());
        let bytes = std::fs::read(&path)?;
        files.insert(rel, bytes);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollback_restores_report_and_leaves_result_table_untouched() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        // A writable narrative nested under runtime/<task>/.
        let task_dir = root.join("runtime").join("task_de");
        std::fs::create_dir_all(&task_dir).expect("mk task dir");
        let report = task_dir.join("report.md");
        std::fs::write(&report, b"original narrative\n").expect("write report");

        // A FROZEN result table sitting right next to the narrative (the
        // faithful twin whose bytes must never change): it must NOT be captured
        // and must survive byte-identical even though we mutate it after the
        // snapshot.
        let result_table = task_dir.join("de.tsv");
        let frozen_bytes = b"gene\tlog2fc\nCRISPLD2\t-2.1\n";
        std::fs::write(&result_table, frozen_bytes).expect("write table");

        // Root-level RO-Crate that should be captured.
        let root_crate = root.join("ro-crate-metadata.json");
        std::fs::write(&root_crate, b"{\"@graph\":[]}").expect("write crate");

        let snap = Snapshotter::take(root).expect("snapshot");
        assert!(
            snap.len() >= 2,
            "must capture at least the report.md and the root RO-Crate, got {}",
            snap.len()
        );

        // Mutate BOTH the writable report and the frozen table after the snapshot.
        std::fs::write(&report, b"CORRUPTED by a bad repair round\n").expect("corrupt report");
        let table_after_mutation = b"gene\tlog2fc\nHACKED\t9.9\n";
        std::fs::write(&result_table, table_after_mutation).expect("mutate table");

        Snapshotter::take(root).ok(); // exercise a second take without side effects
        Snapshotter::take(root).expect("re-take ok"); // no panic

        // Roll back from the ORIGINAL snapshot.
        snap.rollback(root).expect("rollback");

        // The report is restored to its pre-repair bytes.
        let restored = std::fs::read(&report).expect("read report");
        assert_eq!(
            restored, b"original narrative\n",
            "rollback must restore the writable narrative byte-for-byte"
        );

        // The result table is LEFT AS THE MUTATION (snapshot never touched it):
        // proves the snapshot neither captured nor restored a frozen table.
        let table_now = std::fs::read(&result_table).expect("read table");
        assert_eq!(
            table_now, table_after_mutation,
            "snapshot must NOT restore frozen result tables — de.tsv is left as-is"
        );
        assert_ne!(
            table_now.as_slice(),
            frozen_bytes.as_slice(),
            "frozen-table integrity: if rollback had captured de.tsv it would equal the original bytes"
        );

        // The root RO-Crate is restored too.
        let crate_now = std::fs::read(&root_crate).expect("read crate");
        assert_eq!(crate_now, b"{\"@graph\":[]}", "root RO-Crate restored");
    }

    #[test]
    fn take_skips_tsv_and_stray_csv_under_runtime() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let outputs = root.join("runtime").join("outputs").join("task_de");
        std::fs::create_dir_all(&outputs).expect("mk outputs");
        // Frozen tables that must never be captured.
        std::fs::write(outputs.join("summary.tsv"), b"a\tb\n").expect("tsv");
        std::fs::write(outputs.join("counts.csv"), b"x,y\n").expect("csv");
        // The one allowed CSV is the evidence matrix.
        std::fs::write(
            outputs.join("claims_evidence_matrix.csv"),
            b"claim,evidence\n",
        )
        .expect("matrix");

        let snap = Snapshotter::take(root).expect("snapshot");
        let captured: Vec<String> = snap
            .files
            .keys()
            .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
            .collect();
        assert!(
            captured.contains(&"claims_evidence_matrix.csv".to_string()),
            "the evidence matrix CSV must be captured, got {captured:?}"
        );
        assert!(
            !captured.contains(&"summary.tsv".to_string()),
            "*.tsv result tables must NOT be captured"
        );
        assert!(
            !captured.contains(&"counts.csv".to_string()),
            "stray *.csv result tables must NOT be captured"
        );
    }
}
