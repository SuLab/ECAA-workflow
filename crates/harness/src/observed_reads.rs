//! Harness-side capture of observed input reads (design §5.2 C5, tier
//! (b) instrument-and-reconcile).
//!
//! `crates/core::provenance::observed` owns the [`ObservedRead`] type
//! and the `reconcile` decision function; this module is responsible
//! for actually producing a task's `Vec<ObservedRead>` at the harness
//! dispatch site — the `Executor::run_iteration` call sites in
//! `crates/harness/src/executor/local.rs` and `.../mock.rs`, and the
//! main-loop dispatch in `crates/harness/src/main.rs`.
//!
//! Design §5.2/§10 lists two capture mechanisms under tier (b):
//! harness-side syscall tracing (`strace -f -e trace=openat` /
//! fanotify over the task process) or an agent-reported read manifest
//! that the harness reads back and cross-checks. This module ships the
//! manifest path: the agent runbook appends one JSON line per file it
//! opens to `runtime/outputs/<task_id>/reads.jsonl`; [`capture_reads`]
//! reads that manifest back once the task's process has exited and
//! filters it down to reads that plausibly matter for reconciliation.
//!
//! The manifest path is backend-agnostic — it needs no change to the
//! `LocalExecutor::run_iteration` spawn paths (stall monitor,
//! bubblewrap sandboxing, wall-clock kill loop) and works identically
//! for AWS/SLURM once their agent runbooks are updated to append the
//! same manifest. [`strace_available`] is the capability-check seam a
//! future syscall-trace tier would gate on for `LocalExecutor` — see
//! its doc comment for the wiring point that mechanism would need.

use ecaa_workflow_core::provenance::ObservedRead;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

/// Path (relative to the package root) of the read manifest a task's
/// agent runbook appends to as it opens declared input files.
fn manifest_path(package: &Path, task_id: &str) -> PathBuf {
    package
        .join("runtime/outputs")
        .join(task_id)
        .join("reads.jsonl")
}

/// Path prefixes never counted as an observed *input* read: OS/runtime
/// paths outside the declared graph's universe entirely. Mirrors the
/// intent of the `REQUIRED_INHERITED_KEYS` / secrets allowlists
/// elsewhere in this crate — an explicit, auditable list rather than a
/// heuristic.
const SYSTEM_PATH_PREFIXES: &[&str] = &[
    "/proc/", "/sys/", "/dev/", "/usr/", "/etc/", "/lib/", "/lib64/", "/bin/", "/sbin/", "/tmp/",
];

/// True when `path` lives under the task's own working area
/// (`runtime/outputs/<task_id>/…` or `runtime/scratch/<task_id>/…`).
/// A task opening its own output/scratch files is bookkeeping, not a
/// read of a declared producer's edge, so `reconcile` should never see
/// it.
fn is_own_workdir(path: &str, task_id: &str) -> bool {
    let own_output = format!("runtime/outputs/{task_id}/");
    let own_scratch = format!("runtime/scratch/{task_id}/");
    path.starts_with(&own_output) || path.starts_with(&own_scratch)
}

/// True when `path` should be dropped from the observed-read set before
/// it ever reaches `reconcile` — system paths or the task's own workdir.
fn is_ignored(path: &str, task_id: &str) -> bool {
    SYSTEM_PATH_PREFIXES.iter().any(|p| path.starts_with(p)) || is_own_workdir(path, task_id)
}

/// One line of the agent-reported read manifest
/// (`runtime/outputs/<task_id>/reads.jsonl`). `declared_port` is
/// optional — an agent runbook that doesn't know which input port a
/// read satisfies can still report the bare path.
#[derive(Debug, serde::Deserialize)]
struct ManifestLine {
    path: String,
    #[serde(default)]
    declared_port: Option<String>,
}

/// Read and filter `task_id`'s read manifest, if one exists. A missing
/// manifest returns an empty vec — most tasks/backends have not wired
/// their agent runbook to append one yet, and absence is the common
/// case, not an error (mirrors `reconcile`'s own `Untracked` verdict
/// for unmodeled reads). A malformed manifest line is skipped rather
/// than failing the whole read — one bad line shouldn't blind capture
/// for every other line the runbook wrote.
fn read_manifest(package: &Path, task_id: &str) -> Vec<ObservedRead> {
    let path = manifest_path(package, task_id);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ManifestLine>(l).ok())
        .filter(|entry| !is_ignored(&entry.path, task_id))
        .map(|entry| ObservedRead {
            task_id: task_id.to_string(),
            declared_port: entry.declared_port,
            path: entry.path,
        })
        .collect()
}

/// Run `run` — the task dispatch closure, already resolved to the
/// task's process exit status by the time this is called — and capture
/// the set of files the task actually read.
///
/// Capture happens strictly AFTER `run` returns, so it never perturbs
/// the dispatched process: it reads back
/// `runtime/outputs/<task_id>/reads.jsonl` under `package` (the agent
/// runbook's read manifest, see module doc) and filters it through
/// [`is_ignored`]. Sync end to end — no tokio, matching the harness's
/// sync contract (the harness uses `ureq`, not an async runtime).
pub fn capture_reads(
    package: &Path,
    task_id: &str,
    run: impl FnOnce() -> ExitStatus,
) -> (ExitStatus, Vec<ObservedRead>) {
    let status = run();
    let reads = read_manifest(package, task_id);
    (status, reads)
}

/// Capability probe for the syscall-trace tier of read capture
/// (`strace -f -e trace=openat`), checking whether `strace` is on
/// `PATH`. Not yet wired into any executor's spawn path — this is the
/// capability-check seam a future patch would gate on before choosing
/// syscall tracing over the manifest path for `LocalExecutor`, mirroring
/// the existing `which bwrap` probe pattern
/// (`executor::local::detect_default_sandbox`) already used for
/// bubblewrap detection. `AwsExecutor`/`SlurmExecutor` always fall back
/// to the manifest — tracing a remote process over SSM/SSH is out of
/// scope for tier (b)'s first cut.
pub fn strace_available() -> bool {
    std::process::Command::new("which")
        .arg("strace")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::process::ExitStatusExt;

    fn write_manifest(package: &Path, task_id: &str, lines: &[&str]) {
        let out_dir = package.join("runtime/outputs").join(task_id);
        std::fs::create_dir_all(&out_dir).unwrap();
        let mut f = std::fs::File::create(out_dir.join("reads.jsonl")).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    #[test]
    fn capture_reads_returns_declared_read_from_manifest() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "differential_expression",
            &[r#"{"path":"runtime/outputs/quantification/count_matrix.tsv","declared_port":"raw_counts"}"#],
        );
        let (status, reads) = capture_reads(dir.path(), "differential_expression", || {
            ExitStatus::from_raw(0)
        });
        assert!(status.success());
        assert_eq!(reads.len(), 1);
        assert_eq!(
            reads[0],
            ObservedRead {
                task_id: "differential_expression".into(),
                declared_port: Some("raw_counts".into()),
                path: "runtime/outputs/quantification/count_matrix.tsv".into(),
            }
        );
    }

    #[test]
    fn capture_reads_returns_empty_when_no_manifest_present() {
        let dir = tempfile::tempdir().unwrap();
        let (status, reads) =
            capture_reads(dir.path(), "no_manifest_task", || ExitStatus::from_raw(0));
        assert!(status.success());
        assert!(reads.is_empty());
    }

    #[test]
    fn capture_reads_ignores_own_workdir_and_system_paths() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "quantification",
            &[
                // The task's own output — not a read of a declared producer.
                r#"{"path":"runtime/outputs/quantification/count_matrix.tsv"}"#,
                // System path — outside the declared graph's universe.
                r#"{"path":"/etc/hostname"}"#,
                // Legitimate cross-task read.
                r#"{"path":"runtime/outputs/data_acquisition/reads.fastq.gz"}"#,
            ],
        );
        let (_status, reads) =
            capture_reads(dir.path(), "quantification", || ExitStatus::from_raw(0));
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].path, "runtime/outputs/data_acquisition/reads.fastq.gz");
    }

    #[test]
    fn capture_reads_skips_unparseable_lines_without_failing() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "alignment",
            &[
                "not json at all",
                r#"{"path":"runtime/outputs/data_acquisition/reads.fastq.gz"}"#,
            ],
        );
        let (_status, reads) = capture_reads(dir.path(), "alignment", || ExitStatus::from_raw(0));
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].path, "runtime/outputs/data_acquisition/reads.fastq.gz");
    }

    #[test]
    fn strace_available_does_not_panic() {
        // Either answer is valid depending on the host; the seam just
        // needs to resolve without panicking.
        let _ = strace_available();
    }
}
