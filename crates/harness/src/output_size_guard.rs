//! Aggregate output-directory size cap for completed tasks.
//!
//! Complements `swfc_io`'s per-file read cap (100 MiB) with an
//! aggregate-size gate on `runtime/outputs/<task_id>/`:
//!
//! - **Per-file cap** (`swfc_io::read_capped_default`): prevents OOM from a
//!   single giant blob (result.json, state.patch.json, error.json,
//!   WORKFLOW.json).
//! - **Aggregate cap** (this module): prevents disk exhaustion from many
//!   medium-sized blobs — e.g. hundreds of per-sample CSV/parquet files that
//!   each fit under the per-file limit but collectively fill the host disk.
//!
//! The harness calls [`check_output_size`] after task completion, before
//! merging `state.patch.json`. An `Err((observed, threshold))` return causes
//! the harness to skip the patch merge and transition the task to
//! `BlockerKind::OutputSizeExceeded`.
//!
//! The threshold is controlled by the `SWFC_TASK_OUTPUT_MAX_MB` environment
//! variable (default 5120 = 5 GiB). Set to `0` to disable the cap entirely
//! (not recommended in production).

use std::path::Path;

/// Default cap: 5 GiB expressed in mebibytes.
const DEFAULT_MAX_MB: u64 = 5120;

/// Name of the env var that overrides the default cap.
const ENV_VAR: &str = "SWFC_TASK_OUTPUT_MAX_MB";

/// Read the configured threshold from `SWFC_TASK_OUTPUT_MAX_MB`.
/// Falls back to [`DEFAULT_MAX_MB`] on parse error or if unset.
fn threshold_bytes() -> u64 {
    std::env::var(ENV_VAR)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_MB)
        .saturating_mul(1024 * 1024)
}

/// Walk `dir` recursively, summing regular file sizes.
/// Symlinks are not followed — only regular files are counted.
/// Returns an `Err` only on a fatal I/O error that prevents the walk
/// from completing; missing-or-not-a-dir is `Ok(0)`.
fn dir_size_bytes(dir: &Path) -> std::io::Result<u64> {
    let mut total: u64 = 0;
    // Treat a missing directory as empty rather than an error; the output
    // dir may not exist yet for tasks that produced no outputs.
    if !dir.exists() {
        return Ok(0);
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let rd = match std::fs::read_dir(&current) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotADirectory => {
                // Raced with a deletion or path component is a file;
                // treat as empty subtree.
                continue;
            }
            Err(e) => return Err(e),
        };
        for entry in rd {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_symlink() {
                // Do not follow symlinks — the target may live outside the
                // package root and could be arbitrarily large.
                continue;
            }
            if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                total = total.saturating_add(entry.metadata()?.len());
            }
        }
    }
    Ok(total)
}

/// Check whether `<package_root>/runtime/outputs/<task_id>/` exceeds the
/// configured aggregate size cap.
///
/// Returns `Ok(())` when:
/// - the directory is missing or empty (no outputs yet — not a violation),
/// - the total byte count is ≤ threshold.
///
/// Returns `Err((observed_bytes, threshold_bytes))` when the total exceeds
/// the threshold, carrying both values so the caller can populate
/// `BlockerKind::OutputSizeExceeded`.
pub fn check_output_size(package_root: &Path, task_id: &str) -> Result<(), (u64, u64)> {
    let threshold = threshold_bytes();
    // A threshold of zero disables the cap entirely.
    if threshold == 0 {
        return Ok(());
    }
    let output_dir = package_root.join("runtime").join("outputs").join(task_id);
    match dir_size_bytes(&output_dir) {
        Ok(observed) if observed > threshold => Err((observed, threshold)),
        Ok(_) => Ok(()),
        Err(e) => {
            // A walk failure is not treated as a size violation — log and
            // pass. The harness will still merge the patch; a corrupt/
            // unreadable output directory surfaces as a missing-artifact
            // blocker on the next cycle. W1.2: also tick the silent-skip
            // counter so a run with many walk errors shows up in
            // harness-health.json even when individual lines are quiet.
            crate::_observability::note_silent_skip(
                crate::_observability::SkipCategory::OutputSizeWalkError,
                &format!("failed to walk output directory: {}", e),
                Some(task_id),
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: create a package-root-shaped temp dir with
    /// `runtime/outputs/<task_id>/` populated by the given list of
    /// (relative_path, bytes) pairs.
    fn make_package(task_id: &str, files: &[(&str, u64)]) -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let out_dir = root.join("runtime").join("outputs").join(task_id);
        fs::create_dir_all(&out_dir).unwrap();
        for (rel, size) in files {
            let p = out_dir.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            let data = vec![0u8; *size as usize];
            fs::write(&p, &data).unwrap();
        }
        (tmp, root)
    }

    #[test]
    fn under_cap_returns_ok() {
        // 1 MiB file; default cap is 5 GiB — well under.
        let (_tmp, root) = make_package("task_a", &[("result.csv", 1024 * 1024)]);
        assert!(check_output_size(&root, "task_a").is_ok());
    }

    #[test]
    fn over_cap_returns_err_with_observed_and_threshold() {
        // Use a very small cap (1 byte) via env-var override so the test
        // doesn't need to write gigabytes of data.
        let (_tmp, root) = make_package("task_b", &[("a.csv", 100), ("b.csv", 100)]);
        // Set the env var to 0 would disable; set to a small non-zero value.
        std::env::set_var(ENV_VAR, "0");
        // 0 disables — pass. Let's use 1 byte expressed as a fraction of MB.
        // 0 means disabled so use a trick: temporarily set to a value that
        // yields a threshold below 200 bytes. 1 MB = 1_048_576 bytes, so any
        // file sum < 1 MiB will fit. Instead of 0, we need threshold < 200.
        // threshold_bytes() = mb * 1024 * 1024, so we can't go below 1 MiB
        // with this design. Write 2 MiB total and cap at 1 MiB.
        std::env::set_var(ENV_VAR, "1"); // 1 MiB threshold
        let (_tmp2, root2) = make_package("task_c", &[("big.csv", 1024 * 1024 + 1)]); // 1 MiB + 1 byte
        let result = check_output_size(&root2, "task_c");
        // Restore env before asserting (so other tests aren't affected).
        std::env::remove_var(ENV_VAR);
        let (observed, threshold) = result.expect_err("should exceed 1 MiB cap");
        assert!(
            observed > threshold,
            "observed={observed} should exceed threshold={threshold}"
        );
        assert_eq!(threshold, 1024 * 1024);
        assert!(observed >= 1024 * 1024 + 1);
    }

    #[test]
    fn missing_dir_returns_ok() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        // No runtime/outputs/nonexistent_task/ directory created.
        assert!(check_output_size(&root, "nonexistent_task").is_ok());
    }

    #[test]
    fn multiple_files_sum_correctly() {
        // Three 1 MiB files = 3 MiB total; cap at 2 MiB should trigger.
        let (_tmp, root) = make_package(
            "task_d",
            &[
                ("a.parquet", 1024 * 1024),
                ("b.parquet", 1024 * 1024),
                ("c.parquet", 1024 * 1024),
            ],
        );
        std::env::set_var(ENV_VAR, "2"); // 2 MiB cap
        let result = check_output_size(&root, "task_d");
        std::env::remove_var(ENV_VAR);
        assert!(result.is_err(), "3 MiB total should exceed 2 MiB cap");
    }

    // TODO: does_not_follow_symlinks — create a symlink to a large file and
    // verify it is not counted toward the aggregate. Skipped here because
    // creating a multi-GiB file in a test would be impractical; the
    // `dir_size_bytes` implementation explicitly skips `ft.is_symlink()` entries
    // so the invariant is enforced at the code level.
    #[test]
    fn symlinks_are_not_counted() {
        // Create a real file, then a symlink pointing at it.  Only the symlink
        // lives inside the output dir; the real file lives elsewhere.
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let out_dir = root.join("runtime").join("outputs").join("task_sym");
        fs::create_dir_all(&out_dir).unwrap();

        // Real file outside the output dir.
        let real_file = tmp.path().join("big_external.bin");
        fs::write(&real_file, vec![0u8; 5 * 1024 * 1024]).unwrap(); // 5 MiB

        // Symlink inside the output dir pointing at the real file.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_file, out_dir.join("link.bin")).unwrap();

        // Cap at 1 MiB; if symlink were followed, 5 MiB would exceed it.
        std::env::set_var(ENV_VAR, "1");
        let result = check_output_size(&root, "task_sym");
        std::env::remove_var(ENV_VAR);

        // On non-Unix platforms the symlink creation above is skipped, so
        // the directory is empty and the check always passes. On Unix the
        // symlink is present but must NOT be counted.
        assert!(
            result.is_ok(),
            "symlink target bytes must not be counted toward the cap"
        );
    }
}
