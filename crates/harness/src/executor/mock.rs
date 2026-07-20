//! MockExecutor — deterministic iteration sink for harness unit tests.
//!
//! Push a scripted sequence of `IterationOutcome`s via `new`; the harness
//! loop pulls them in order. Mirrors `MockLlmBackend` in the conversation
//! crate. Test-only; gated behind `cfg(test)` so release binaries never
//! link it.

use super::{Executor, IterationOutcome, RemoteExecutionInfo};
use anyhow::Result;
use ecaa_workflow_core::dag::{Task, TaskState, DAG};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;

pub struct MockExecutor {
    scripted: Vec<IterationOutcome>,
    cursor: usize,
    provision_calls: usize,
    release_calls: usize,
    pub fixed_stale: bool,
    /// Records `(task_id, ExecutorOverrides)` every time the harness
    /// calls `apply_overrides` on this executor. Lets tests assert the
    /// orchestration order (apply-before-run) and the overrides
    /// payload arrives intact.
    pub apply_overrides_log: Vec<(String, ecaa_workflow_core::remediation::ExecutorOverrides)>,
    /// Most recent iteration's observed input reads (design §5.2 C5),
    /// captured from the fixture's `runtime/outputs/<task_id>/reads.jsonl`
    /// manifest via `crate::observed_reads::capture_reads`. Drained by
    /// `take_observed_reads`.
    last_observed_reads: Vec<ecaa_workflow_core::provenance::ObservedRead>,
}

impl MockExecutor {
    pub fn new(scripted: Vec<IterationOutcome>) -> Self {
        Self {
            scripted,
            cursor: 0,
            provision_calls: 0,
            release_calls: 0,
            fixed_stale: false,
            apply_overrides_log: Vec::new(),
            last_observed_reads: Vec::new(),
        }
    }

    /// Convenience constructor: N successful iterations, all local.
    pub fn with_successes(n: usize) -> Self {
        let scripted = (0..n)
            .map(|_| IterationOutcome {
                agent_status: ExitStatus::from_raw(0),
                remote: None,
            })
            .collect();
        Self::new(scripted)
    }

    /// Build a scripted outcome with attached remote metadata — useful for
    /// asserting the harness threads remote info through to progress
    /// events correctly.
    pub fn remote_outcome(
        backend: &str,
        instance_id: &str,
        instance_type: &str,
    ) -> IterationOutcome {
        IterationOutcome {
            agent_status: ExitStatus::from_raw(0),
            remote: Some(RemoteExecutionInfo {
                backend: backend.into(),
                instance_id: instance_id.into(),
                instance_type: instance_type.into(),
            }),
        }
    }

    pub fn provision_calls(&self) -> usize {
        self.provision_calls
    }

    pub fn release_calls(&self) -> usize {
        self.release_calls
    }

    pub fn remaining(&self) -> usize {
        self.scripted.len().saturating_sub(self.cursor)
    }
}

impl Executor for MockExecutor {
    fn name(&self) -> &'static str {
        "mock"
    }

    fn provision(&mut self, _dag: &DAG) -> Result<()> {
        self.provision_calls += 1;
        Ok(())
    }

    fn run_iteration(
        &mut self,
        package: &Path,
        _agent_cmd: &str,
        envelope: &std::collections::BTreeMap<String, String>,
    ) -> Result<IterationOutcome> {
        if self.cursor >= self.scripted.len() {
            anyhow::bail!(
                "MockExecutor exhausted: cursor={}, scripted_len={}",
                self.cursor,
                self.scripted.len()
            );
        }
        // `IterationOutcome` is not Clone (carries a raw ExitStatus) — pop
        // by index via swap_remove would reorder; instead build a fresh
        // replacement referencing the same exit code.
        let idx = self.cursor;
        self.cursor += 1;
        let old = self.scripted.get(idx).unwrap();
        // Observed-reads capture (design §5.2 C5): for the mock backend,
        // "the fixture" is whatever `reads.jsonl` manifest the test wrote
        // under `package` before dispatch — there is no real agent
        // runbook to append one, so tests seed it directly.
        self.last_observed_reads = match envelope.get(crate::executor::hardware_envelope::TASK_ID_ENV) {
            Some(task_id) if !task_id.is_empty() => {
                let (_status, reads) =
                    crate::observed_reads::capture_reads(package, task_id, || old.agent_status);
                reads
            }
            _ => Vec::new(),
        };
        Ok(IterationOutcome {
            agent_status: old.agent_status,
            remote: old.remote.clone(),
        })
    }

    fn is_task_stale(&self, task: &Task, _now_secs: u64) -> bool {
        if self.fixed_stale {
            return matches!(task.state, TaskState::Running { .. });
        }
        false
    }

    fn apply_overrides(
        &mut self,
        task_id: &str,
        ov: &ecaa_workflow_core::remediation::ExecutorOverrides,
    ) -> Result<()> {
        self.apply_overrides_log
            .push((task_id.to_string(), ov.clone()));
        Ok(())
    }

    fn release(&mut self) {
        self.release_calls += 1;
    }

    fn take_observed_reads(&mut self) -> Vec<ecaa_workflow_core::provenance::ObservedRead> {
        std::mem::take(&mut self.last_observed_reads)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn empty_dag() -> DAG {
        DAG {
            version: "1.0".into(),
            schema_version: ecaa_workflow_core::dag::current_dag_schema_version(),
            workflow_id: "mock".into(),
            current_task: None,
            tasks: Default::default(),
            reverse_deps: Default::default(),
            run_id: None,
            execution_order: Vec::new(),
        }
    }

    #[test]
    fn dispatches_scripted_outcomes_in_order() {
        let mut m = MockExecutor::new(vec![
            IterationOutcome {
                agent_status: ExitStatus::from_raw(0),
                remote: None,
            },
            IterationOutcome {
                agent_status: ExitStatus::from_raw(256), // exit code 1
                remote: None,
            },
        ]);
        let path = PathBuf::from("/tmp/pkg");
        m.provision(&empty_dag()).unwrap();
        let first = m
            .run_iteration(&path, "agent", &std::collections::BTreeMap::new())
            .unwrap();
        assert!(first.agent_status.success());
        let second = m
            .run_iteration(&path, "agent", &std::collections::BTreeMap::new())
            .unwrap();
        assert!(!second.agent_status.success());
        assert_eq!(m.provision_calls(), 1);
    }

    #[test]
    fn exhaustion_errors() {
        let mut m = MockExecutor::with_successes(1);
        let path = PathBuf::from("/tmp/pkg");
        m.run_iteration(&path, "agent", &std::collections::BTreeMap::new())
            .unwrap();
        let err = m
            .run_iteration(&path, "agent", &std::collections::BTreeMap::new())
            .err()
            .expect("should exhaust");
        assert!(err.to_string().contains("exhausted"));
    }

    #[test]
    fn release_is_idempotent_and_counts() {
        let mut m = MockExecutor::with_successes(0);
        m.release();
        m.release();
        assert_eq!(m.release_calls(), 2);
    }

    /// TDD anchor for design §5.2 C5 (task 10): a Mock-executor task that
    /// declares (via its fixture manifest) that it read a specific file
    /// surfaces that read through `take_observed_reads` after
    /// `run_iteration`.
    #[test]
    fn run_iteration_surfaces_fixture_declared_read() {
        use std::io::Write;

        let dir = tempfile::tempdir().unwrap();
        let task_id = "differential_expression";
        let out_dir = dir.path().join("runtime/outputs").join(task_id);
        std::fs::create_dir_all(&out_dir).unwrap();
        let mut f = std::fs::File::create(out_dir.join("reads.jsonl")).unwrap();
        writeln!(
            f,
            r#"{{"path":"runtime/outputs/quantification/count_matrix.tsv","declared_port":"raw_counts"}}"#
        )
        .unwrap();
        drop(f);

        let mut envelope = std::collections::BTreeMap::new();
        envelope.insert(
            crate::executor::hardware_envelope::TASK_ID_ENV.to_string(),
            task_id.to_string(),
        );

        let mut m = MockExecutor::with_successes(1);
        m.run_iteration(dir.path(), "agent", &envelope).unwrap();
        let reads = m.take_observed_reads();
        assert_eq!(reads.len(), 1);
        assert_eq!(
            reads[0],
            ecaa_workflow_core::provenance::ObservedRead {
                task_id: task_id.to_string(),
                declared_port: Some("raw_counts".to_string()),
                path: "runtime/outputs/quantification/count_matrix.tsv".to_string(),
            }
        );
        // Draining is destructive — a second call sees nothing left.
        assert!(m.take_observed_reads().is_empty());
    }

    #[test]
    fn run_iteration_with_no_task_id_in_envelope_has_no_observed_reads() {
        let mut m = MockExecutor::with_successes(1);
        let path = PathBuf::from("/tmp/pkg");
        m.run_iteration(&path, "agent", &std::collections::BTreeMap::new())
            .unwrap();
        assert!(m.take_observed_reads().is_empty());
    }

    #[test]
    fn apply_overrides_records_invocation() {
        use ecaa_workflow_core::remediation::{ExecutorOverrides, ResourceTarget};
        let mut m = MockExecutor::with_successes(0);
        let ov = ExecutorOverrides {
            resources: Some(ResourceTarget {
                memory_gb: Some(64),
                ..Default::default()
            }),
            ..Default::default()
        };
        m.apply_overrides("alignment", &ov).unwrap();
        assert_eq!(m.apply_overrides_log.len(), 1);
        assert_eq!(m.apply_overrides_log[0].0, "alignment");
        assert_eq!(
            m.apply_overrides_log[0]
                .1
                .resources
                .as_ref()
                .unwrap()
                .memory_gb,
            Some(64)
        );
    }
}
