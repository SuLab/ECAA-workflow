//! Agentic executors: classes whose repair requires an agent re-run rather than
//! a mechanical edit.
//!
//! Each executor emits a [`RepairOutcome::NeedsAgent`] directive scoped to the
//! failing task. Every directive forbids touching the frozen result tables --
//! the agent may correct narratives, citations, evidence, or re-run an analysis,
//! but it must never edit `runtime/outputs/<task>/*` to manufacture agreement.

use std::path::Path;

use crate::repair_loop::executor::{Executor, RepairOutcome};
use crate::repair_loop::failure::{Failure, RepairClass};
use crate::repair_loop::runner::{RepairDirective, TaskRunner};

/// Repairs a malformed or unresolved citation by re-resolving it from source.
pub struct CitationFix;

impl Executor for CitationFix {
    fn class(&self) -> RepairClass {
        RepairClass::CitationFix
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
                "Fix the citation for {} by re-resolving it from an authoritative \
                 source and correcting the reference in the narrative ({}). \
                 Do not alter result tables.",
                f.subject, f.detail
            ),
        })
    }
}

/// Closes a coverage gap by emitting the missing required claim.
pub struct CoverageGap;

impl Executor for CoverageGap {
    fn class(&self) -> RepairClass {
        RepairClass::CoverageGap
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
                "Close the coverage gap for {} by emitting the missing required \
                 claim, grounded in the existing frozen results ({}). \
                 Do not alter result tables.",
                f.subject, f.detail
            ),
        })
    }
}

/// Re-runs a failed analysis task as a recorded post-hoc deviation.
pub struct AnalysisRerun;

impl Executor for AnalysisRerun {
    fn class(&self) -> RepairClass {
        RepairClass::AnalysisRerun
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
                "Fix and re-run the analysis for {} as a recorded post-hoc \
                 deviation; the re-run must re-pass validation ({}). \
                 Do not alter result tables.",
                f.subject, f.detail
            ),
        })
    }
}

/// Records evidence missing from the claims evidence matrix.
pub struct EvidenceCompletion;

impl Executor for EvidenceCompletion {
    fn class(&self) -> RepairClass {
        RepairClass::EvidenceCompletion
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
                "Record the missing evidence for {} in the claims evidence matrix, \
                 citing the existing frozen results ({}). \
                 Do not alter result tables.",
                f.subject, f.detail
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair_loop::failure::FailureSource;

    /// Stub runner that must never be invoked by these executors.
    struct NoRunner;
    impl TaskRunner for NoRunner {
        fn rerun(&self, _pkg: &Path, _directive: &RepairDirective) -> anyhow::Result<()> {
            panic!("agentic executors must not invoke the runner; they only emit directives");
        }
    }

    fn failure_for(class: RepairClass) -> Failure {
        Failure::new(
            FailureSource::ClaimMismatch,
            class,
            "expression_analysis",
            "claim_42",
            "unresolved DOI 10.1234/bad",
        )
    }

    #[test]
    fn citation_fix_class_matches() {
        assert_eq!(
            CitationFix.class(),
            RepairClass::CitationFix,
            "CitationFix must claim CitationFix"
        );
    }

    #[test]
    fn citation_fix_needs_agent_with_task_and_detail() {
        let f = failure_for(RepairClass::CitationFix);
        let dir = tempfile::tempdir().expect("tempdir");
        let outcome = CitationFix.repair(&f, dir.path(), dir.path(), &NoRunner);
        match outcome {
            RepairOutcome::NeedsAgent(directive) => {
                assert_eq!(
                    directive.task, f.task,
                    "directive task must equal the failure task"
                );
                assert!(
                    directive.instruction.contains(&f.detail),
                    "instruction must mention the detail, got: {}",
                    directive.instruction
                );
                assert!(
                    directive.instruction.contains("Do not alter result tables"),
                    "instruction must forbid altering result tables, got: {}",
                    directive.instruction
                );
            }
            other => panic!("expected NeedsAgent, got {other:?}"),
        }
    }

    #[test]
    fn all_agentic_executors_emit_needs_agent_and_protect_tables() {
        let dir = tempfile::tempdir().expect("tempdir");
        let cases: Vec<(RepairClass, RepairOutcome)> = vec![
            (
                RepairClass::CitationFix,
                CitationFix.repair(
                    &failure_for(RepairClass::CitationFix),
                    dir.path(),
                    dir.path(),
                    &NoRunner,
                ),
            ),
            (
                RepairClass::CoverageGap,
                CoverageGap.repair(
                    &failure_for(RepairClass::CoverageGap),
                    dir.path(),
                    dir.path(),
                    &NoRunner,
                ),
            ),
            (
                RepairClass::AnalysisRerun,
                AnalysisRerun.repair(
                    &failure_for(RepairClass::AnalysisRerun),
                    dir.path(),
                    dir.path(),
                    &NoRunner,
                ),
            ),
            (
                RepairClass::EvidenceCompletion,
                EvidenceCompletion.repair(
                    &failure_for(RepairClass::EvidenceCompletion),
                    dir.path(),
                    dir.path(),
                    &NoRunner,
                ),
            ),
        ];
        for (class, outcome) in cases {
            match outcome {
                RepairOutcome::NeedsAgent(directive) => {
                    assert_eq!(
                        directive.task, "expression_analysis",
                        "{class:?} directive must carry the failure task"
                    );
                    assert!(
                        directive.instruction.contains("Do not alter result tables"),
                        "{class:?} instruction must protect result tables, got: {}",
                        directive.instruction
                    );
                }
                other => panic!("{class:?} expected NeedsAgent, got {other:?}"),
            }
        }
    }

    #[test]
    fn classes_match_their_structs() {
        assert_eq!(
            CoverageGap.class(),
            RepairClass::CoverageGap,
            "CoverageGap class"
        );
        assert_eq!(
            AnalysisRerun.class(),
            RepairClass::AnalysisRerun,
            "AnalysisRerun class"
        );
        assert_eq!(
            EvidenceCompletion.class(),
            RepairClass::EvidenceCompletion,
            "EvidenceCompletion class"
        );
    }
}
