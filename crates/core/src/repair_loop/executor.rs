//! Per-class repair executors and their registry.

use std::path::Path;

use super::failure::{Failure, RepairClass};
use super::runner::{RepairDirective, TaskRunner};

/// Result of attempting to repair a single failure.
#[derive(Debug, Clone, PartialEq)]
pub enum RepairOutcome {
    /// The repair was applied. `deterministic` records whether it was a
    /// mechanical fix; `note` describes what changed.
    Applied {
        /// Whether the fix was mechanical (no agent).
        deterministic: bool,
        /// Human-readable description of the change.
        note: String,
    },
    /// Some real, deterministic work was done, but the failure is NOT fully
    /// resolved: a residual obstacle remains that this executor cannot close
    /// (e.g. an offline-only validator). The driver MUST route this to review
    /// rather than counting it as resolved.
    PartiallyApplied {
        /// Whether the work that *was* done was mechanical (no agent).
        deterministic: bool,
        /// Human-readable description of the work that was applied.
        note: String,
        /// The residual obstacle that keeps the failure unresolved.
        residual: String,
    },
    /// The repair requires an agent re-run; carries the directive to route.
    NeedsAgent(RepairDirective),
    /// The failure could not be repaired; carries the reason.
    Unrepairable(String),
}

/// Repairs failures of a single [`RepairClass`].
pub trait Executor {
    /// The class this executor handles.
    fn class(&self) -> RepairClass;
    /// Attempt to repair `f` against the package at `pkg`, using `config_dir`
    /// for any configuration lookups and `runner` for agentic re-runs.
    fn repair(
        &self,
        f: &Failure,
        pkg: &Path,
        config_dir: &Path,
        runner: &dyn TaskRunner,
    ) -> RepairOutcome;
}

/// Registry mapping each [`RepairClass`] to at most one [`Executor`].
#[derive(Default)]
pub struct ExecutorRegistry {
    executors: Vec<Box<dyn Executor>>,
}

impl ExecutorRegistry {
    /// Register an executor. A later registration for the same class shadows
    /// earlier ones via [`ExecutorRegistry::for_class`] lookup order.
    pub fn register(&mut self, executor: Box<dyn Executor>) {
        self.executors.push(executor);
    }

    /// The most recently registered executor for `class`, if any.
    pub fn for_class(&self, class: RepairClass) -> Option<&dyn Executor> {
        self.executors
            .iter()
            .rev()
            .find(|e| e.class() == class)
            .map(|e| e.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair_loop::failure::FailureSource;

    /// Test runner that records nothing and always succeeds.
    pub(super) struct MockRunner;
    impl TaskRunner for MockRunner {
        fn rerun(&self, _pkg: &Path, _directive: &RepairDirective) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Test executor that reports `Applied` deterministically for its class.
    pub(super) struct MockExecutor {
        class: RepairClass,
    }
    impl Executor for MockExecutor {
        fn class(&self) -> RepairClass {
            self.class
        }
        fn repair(
            &self,
            _f: &Failure,
            _pkg: &Path,
            _config_dir: &Path,
            _runner: &dyn TaskRunner,
        ) -> RepairOutcome {
            RepairOutcome::Applied {
                deterministic: self.class.is_deterministic(),
                note: format!("mock repaired {:?}", self.class),
            }
        }
    }

    #[test]
    fn registry_dispatches_by_class() {
        let mut reg = ExecutorRegistry::default();
        reg.register(Box::new(MockExecutor {
            class: RepairClass::NarrativeCorrection,
        }));
        reg.register(Box::new(MockExecutor {
            class: RepairClass::CitationFix,
        }));

        assert!(
            reg.for_class(RepairClass::NarrativeCorrection).is_some(),
            "registered class must resolve"
        );
        assert!(
            reg.for_class(RepairClass::CoverageGap).is_none(),
            "unregistered class must not resolve"
        );

        let exec = reg
            .for_class(RepairClass::CitationFix)
            .expect("citation executor present");
        let f = Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::CitationFix,
            "t",
            "s",
            "d",
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = exec.repair(&f, dir.path(), dir.path(), &MockRunner);
        assert!(
            matches!(
                outcome,
                RepairOutcome::Applied {
                    deterministic: false,
                    ..
                }
            ),
            "citation fix is non-deterministic Applied in the mock, got {outcome:?}"
        );
    }
}
