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
    pub apply_overrides_log: Vec<(
        String,
        ecaa_workflow_core::remediation::ExecutorOverrides,
    )>,
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
        _package: &Path,
        _agent_cmd: &str,
        _envelope: &std::collections::BTreeMap<String, String>,
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
