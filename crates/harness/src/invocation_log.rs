//! M2 — `runtime/invocations.jsonl`: one validated-invocation record per
//! dispatched task. The runtime mirror of the compile-time
//! `runtime/proofs.jsonl` (which covers *edges*); this covers
//! *invocations* — the "validated invocation as the unit of
//! execution/provenance/approval".
//!
//! Written by the HARNESS at the dispatch site (sync, no tokio — the
//! harness uses `ureq`/blocking I/O). Mirrors `dispatch_wal::append_dispatch`:
//! `OpenOptions::append(true)` + a single `write_all` of `json + "\n"` +
//! `f.sync_data()` so a mid-dispatch crash leaves a durable record.
//!
//! NOT byte-reproducible (carries dispatch-time `started_at` + per-run
//! `epoch`); excluded from the emit byte-diff baseline allowlist.
//!
//! Design §5.2 C5 extends the record with `observed_reads` — the files
//! a task actually read, captured post-run via
//! `crate::observed_reads::capture_reads`. Because reads aren't known
//! until AFTER the agent process exits, the dispatch site writes TWO
//! lines per dispatched task when the agent reported any reads: the
//! original pre-dispatch record (unchanged, `observed_reads` empty —
//! preserving the crash-durability guarantee above) and a completion-time
//! follow-up carrying the same `(task_id, epoch, harness_run_id)` plus
//! `observed_reads` populated. Absent a read manifest (the common case
//! until every backend's agent runbook writes one), only the original
//! line is written, so existing single-line-per-dispatch consumers are
//! unaffected. `crates/conversation/src/emit/ro_crate.rs` folds every
//! line's `observed_reads` per task before calling
//! `ecaa_workflow_core::provenance::reconcile`.

use anyhow::Result;
use ecaa_workflow_core::atom::{SafetyPolicy, SandboxRequirement};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Current on-disk schema version for [`InvocationRecord`].
pub fn invocation_record_schema_version() -> semver::Version {
    // Local to the harness IR family; bump on shape changes. Mirrors the
    // dispatch_wal versioning discipline.
    semver::Version::new(0, 1, 0)
}

fn default_invocation_record_schema_version() -> semver::Version {
    invocation_record_schema_version()
}

/// One validated-invocation provenance object. Binds, for a single
/// dispatched task: the source atom (version proxy = atom id + the
/// package's WORKFLOW.json schema), the resolved container image
/// (resolved parameters proxy — the per-task pin from compose time),
/// whether port-typed inputs were satisfied at dispatch, and the
/// sandbox profile in force.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InvocationRecord {
    /// On-disk schema version. `#[serde(default)]` lets pre-field
    /// readers load with `0.1.0`.
    #[serde(
        default = "default_invocation_record_schema_version",
        with = "ecaa_workflow_core::migration::schema_version_serde"
    )]
    pub schema_version: semver::Version,
    /// Task id being invoked.
    pub task_id: String,
    /// Source atom id this task was emitted from (`Task::source_atom_id`).
    /// `None` for legacy taxonomy-built tasks with no atom backing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub atom_id: Option<String>,
    /// Monotonic dispatch epoch within the harness run (mirrors the
    /// dispatch WAL `epoch`, so an invocation record pairs 1:1 with a
    /// `DispatchRecord` by `(harness_run_id, epoch)`).
    pub epoch: u64,
    /// The harness process's unique id at dispatch time.
    pub harness_run_id: String,
    /// RFC-3339 dispatch timestamp (the `started_at` stamped onto the
    /// task's `Running` state this iteration).
    pub started_at: String,
    /// Resolved prerequisite task ids (`Task::depends_on`). The runtime
    /// statement that this invocation's port-typed inputs are produced
    /// upstream — when every prerequisite is Completed, the inputs are
    /// satisfied.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prerequisites: Vec<String>,
    /// True when every prerequisite of this task is Completed at dispatch
    /// time — the runtime mirror of the compile-time port-unification
    /// proof. The harness only dispatches `Ready` tasks (all deps
    /// Completed), so this is `true` on the happy path; the field is
    /// explicit so an auditor can read it without re-deriving readiness.
    pub port_typed_inputs_satisfied: bool,
    /// Sandbox requirement in force for this invocation
    /// (`Task::safety.sandbox`).
    pub sandbox: SandboxRequirement,
    /// Convenience boolean: `sandbox != None`. Lets a reviewer filter
    /// "every invocation ran sandboxed above the container default"
    /// without matching the enum tag.
    pub sandbox_required: bool,
    /// Network policy tag in force (`Task::safety.network` serialized).
    pub network_policy: serde_json::Value,
    /// True when the source atom operates on controlled-access data
    /// (`Task::safety.controlled_access`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub controlled_access: bool,
    /// Resolved container image pin for this task (the resolved-parameters
    /// proxy — compose-time container resolution output). `None` =
    /// host-mode / unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_image: Option<String>,
    /// Files this task was observed to read (design §5.2 C5), captured
    /// harness-side via `crate::observed_reads::capture_reads` AFTER the
    /// task's process exits. Empty on the pre-dispatch record written
    /// before the agent spawns (reads aren't known yet) — the dispatch
    /// site appends a SECOND record for the same
    /// `(task_id, epoch, harness_run_id)` once reads are captured, so
    /// this field is only ever non-empty on that follow-up line.
    /// Consumed by `ecaa_workflow_core::ro_crate::reconcile_ro_crate_edges`
    /// (`crates/conversation/src/emit/ro_crate.rs`) to resolve which
    /// member of a mutually-exclusive one-of input group actually ran.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observed_reads: Vec<ecaa_workflow_core::provenance::ObservedRead>,
}

impl InvocationRecord {
    /// Build a record from the dispatch-time facts. The harness call site
    /// supplies the per-task fields read off the `Task` + the dispatch
    /// epoch/run-id already computed for the WAL. `observed_reads` is
    /// always empty at construction — reads aren't known until after the
    /// agent process exits; see [`InvocationRecord::with_observed_reads`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        task_id: &str,
        atom_id: Option<&str>,
        epoch: u64,
        harness_run_id: &str,
        started_at: &str,
        prerequisites: &[String],
        port_typed_inputs_satisfied: bool,
        safety: &SafetyPolicy,
        container_image: Option<&str>,
    ) -> Self {
        Self {
            schema_version: invocation_record_schema_version(),
            task_id: task_id.to_string(),
            atom_id: atom_id.map(str::to_string),
            epoch,
            harness_run_id: harness_run_id.to_string(),
            started_at: started_at.to_string(),
            prerequisites: prerequisites.to_vec(),
            port_typed_inputs_satisfied,
            sandbox: safety.sandbox,
            sandbox_required: !matches!(safety.sandbox, SandboxRequirement::None),
            network_policy: serde_json::to_value(&safety.network)
                .unwrap_or(serde_json::Value::Null),
            controlled_access: safety.controlled_access,
            container_image: container_image.map(str::to_string),
            observed_reads: Vec::new(),
        }
    }

    /// Clone this record with `observed_reads` populated, for the
    /// completion-time follow-up append (design §5.2 C5). Keeps the
    /// pre-dispatch record's crash-durability guarantee intact — that
    /// record is written unchanged, before the agent spawns; this
    /// produces a SECOND, enriched line rather than mutating the first.
    #[must_use]
    pub fn with_observed_reads(
        &self,
        observed_reads: Vec<ecaa_workflow_core::provenance::ObservedRead>,
    ) -> Self {
        Self {
            observed_reads,
            ..self.clone()
        }
    }
}

/// Path of the invocation log under the package root.
pub fn invocation_log_path(package_root: &Path) -> PathBuf {
    package_root.join("runtime/invocations.jsonl")
}

/// Append a single invocation record. Sync; ensures the parent dir
/// exists; composes `json + "\n"` into one buffer for a single atomic
/// `write_all` (POSIX O_APPEND atomicity below PIPE_BUF), then fsyncs
/// so the record is durable before the agent subprocess spawns. Mirrors
/// `dispatch_wal::append_dispatch`.
pub fn append_invocation(package_root: &Path, record: &InvocationRecord) -> Result<()> {
    let path = invocation_log_path(package_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    f.write_all(line.as_bytes())?;
    f.sync_data()?; // durable before agent spawn
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::atom::SafetyPolicy;

    #[test]
    fn append_writes_one_jsonl_line_per_invocation() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path();
        let rec = InvocationRecord::new(
            "qc_preprocessing",
            Some("quality_control"),
            1,
            "run-abc",
            "2026-06-02T00:00:00Z",
            &["data_acquisition".to_string()],
            true,
            &SafetyPolicy::default(),
            Some("ecaa/bio-min:latest"),
        );
        append_invocation(pkg, &rec).unwrap();
        append_invocation(pkg, &rec).unwrap();

        let path = invocation_log_path(pkg);
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "one JSONL line per append");
        let parsed: InvocationRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(parsed.task_id, "qc_preprocessing");
        assert_eq!(parsed.atom_id.as_deref(), Some("quality_control"));
        assert_eq!(parsed.epoch, 1);
        assert!(parsed.port_typed_inputs_satisfied);
        assert_eq!(parsed.prerequisites, vec!["data_acquisition".to_string()]);
    }

    #[test]
    fn record_captures_sandbox_required_flag() {
        let mut sp = SafetyPolicy::default();
        sp.sandbox = SandboxRequirement::ProcessIsolation;
        let rec = InvocationRecord::new(
            "align_reads",
            Some("align_reads"),
            2,
            "run-xyz",
            "2026-06-02T00:00:01Z",
            &[],
            true,
            &sp,
            None,
        );
        assert!(
            rec.sandbox_required,
            "a non-None sandbox requirement must set the flag"
        );
    }

    #[test]
    fn default_sandbox_is_not_required() {
        let rec = InvocationRecord::new(
            "data_import",
            None,
            3,
            "run-def",
            "2026-06-02T00:00:02Z",
            &[],
            true,
            &SafetyPolicy::default(),
            None,
        );
        assert!(
            !rec.sandbox_required,
            "the default (None) sandbox requirement must not set the flag"
        );
    }

    #[test]
    fn new_record_has_no_observed_reads() {
        let rec = InvocationRecord::new(
            "differential_expression",
            Some("differential_expression"),
            4,
            "run-ghi",
            "2026-06-02T00:00:03Z",
            &["quantification".to_string(), "normalisation".to_string()],
            true,
            &SafetyPolicy::default(),
            None,
        );
        assert!(rec.observed_reads.is_empty());
        // Empty observed_reads must not serialize (keeps the on-disk
        // shape unchanged for every task whose agent runbook reports no
        // read manifest — the common case).
        let json = serde_json::to_string(&rec).unwrap();
        assert!(
            !json.contains("observed_reads"),
            "empty observed_reads must be omitted from the serialized record"
        );
    }

    #[test]
    fn with_observed_reads_appends_a_second_enriched_line() {
        use ecaa_workflow_core::provenance::ObservedRead;

        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path();
        let base = InvocationRecord::new(
            "differential_expression",
            Some("differential_expression"),
            5,
            "run-jkl",
            "2026-06-02T00:00:04Z",
            &["quantification".to_string(), "normalisation".to_string()],
            true,
            &SafetyPolicy::default(),
            None,
        );
        append_invocation(pkg, &base).unwrap();

        let reads = vec![ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: "runtime/outputs/quantification/count_matrix.tsv".into(),
        }];
        let enriched = base.with_observed_reads(reads.clone());
        append_invocation(pkg, &enriched).unwrap();

        let path = invocation_log_path(pkg);
        let body = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "pre-dispatch line + completion-time follow-up");

        let first: InvocationRecord = serde_json::from_str(lines[0]).unwrap();
        assert!(first.observed_reads.is_empty(), "pre-dispatch line carries no reads");

        let second: InvocationRecord = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second.task_id, "differential_expression");
        assert_eq!(second.epoch, base.epoch);
        assert_eq!(second.observed_reads, reads);
    }
}
