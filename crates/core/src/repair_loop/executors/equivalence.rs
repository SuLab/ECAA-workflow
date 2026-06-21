//! Equivalence-rerun executor.
//!
//! Routes an [`RepairClass::EquivalenceRerun`] failure to a reproduce-and-compare
//! re-execution. The directive deliberately instructs the agent to *confirm*
//! reproduction only -- never to edit outputs into agreement. Forging a pass
//! would defeat the equivalence check entirely.

use std::path::Path;

use crate::repair_loop::executor::{Executor, RepairOutcome};
use crate::repair_loop::failure::{Failure, RepairClass};
use crate::repair_loop::runner::{RepairDirective, TaskRunner};

/// Re-executes a recorded result deterministically and compares it against the
/// frozen record, without ever modifying the outputs to force a match.
pub struct EquivalenceRerun;

impl Executor for EquivalenceRerun {
    fn class(&self) -> RepairClass {
        RepairClass::EquivalenceRerun
    }

    fn repair(
        &self,
        f: &Failure,
        _pkg: &Path,
        _config_dir: &Path,
        _runner: &dyn TaskRunner,
    ) -> RepairOutcome {
        RepairOutcome::NeedsAgent(RepairDirective {
            task: f.task.clone(),
            instruction: format!(
                "Re-execute deterministically from env.lock + inputs and confirm \
                 the recorded result reproduces ({}). Report equivalence; do NOT \
                 modify outputs to force a match. Do not alter result tables.",
                f.detail
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair_loop::failure::FailureSource;

    /// Stub runner that is never invoked by [`EquivalenceRerun::repair`].
    struct NoRunner;
    impl TaskRunner for NoRunner {
        fn rerun(&self, _pkg: &Path, _directive: &RepairDirective) -> anyhow::Result<()> {
            panic!("EquivalenceRerun must not invoke the runner; it only emits a directive");
        }
    }

    #[test]
    fn class_is_equivalence_rerun() {
        assert_eq!(
            EquivalenceRerun.class(),
            RepairClass::EquivalenceRerun,
            "executor must claim its matching class"
        );
    }

    #[test]
    fn repair_needs_agent_with_task_and_detail() {
        let f = Failure::new(
            FailureSource::InvariantFailure("equivalence".to_string()),
            RepairClass::EquivalenceRerun,
            "deseq_contrast",
            "result_hash",
            "hash mismatch: expected a1b2 got c3d4",
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = EquivalenceRerun.repair(&f, dir.path(), dir.path(), &NoRunner);
        match outcome {
            RepairOutcome::NeedsAgent(directive) => {
                assert_eq!(
                    directive.task, f.task,
                    "directive must carry the failure's task verbatim"
                );
                assert!(
                    directive.instruction.contains(&f.detail),
                    "instruction must surface the failure detail, got: {}",
                    directive.instruction
                );
                assert!(
                    directive.instruction.contains("do NOT")
                        || directive.instruction.contains("Do not"),
                    "instruction must forbid forging a pass, got: {}",
                    directive.instruction
                );
            }
            other => panic!("expected NeedsAgent, got {other:?}"),
        }
    }
}
