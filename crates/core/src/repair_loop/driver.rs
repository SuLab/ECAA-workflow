//! The repair-loop driver: a bounded state machine that assesses a package,
//! dispatches per-class [`Executor`]s, and converges to a [`RepairStatus`].
//!
//! The core [`run_loop`] takes an injected `assess` closure so it is unit-
//! testable with synthetic failure sequences and mock executors; the
//! production entry point [`run_repair_loop`] wires the real
//! [`assess_package`](super::assess::assess_package) and [`default_registry`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::assess::assess_package;
use super::executor::ExecutorRegistry;
use super::executors;
use super::failure::{
    default_budget, FailureSet, FailureStatus, GLOBAL_ROUND_CAP,
};
use super::executor::RepairOutcome;
use super::provenance::{append_repair_log, RepairLogEntry};
use super::runner::TaskRunner;
use super::snapshot::Snapshotter;
use super::status::{from_final, RepairStatus};

/// Status verdicts below `MostlyPassing` are never produced by the driver
/// unless an absurd number of failures remain; pick a threshold large enough
/// that any realistic residue routes to review as `MostlyPassing`.
const FAILING_THRESHOLD: usize = 1000;

/// Drive the repair state machine to convergence.
///
/// `assess` is called once per round to (re)compute the current
/// [`FailureSet`] from the package on disk; injecting it keeps the loop
/// unit-testable. The loop is bounded by [`GLOBAL_ROUND_CAP`], a per-class
/// [`default_budget`], an oscillation guard (no change in failure ids across
/// two consecutive rounds), and a regression guard (a round that grows the
/// distinct failure set is rolled back and its targets routed to review).
pub fn run_loop(
    root: &Path,
    config_dir: &Path,
    assess: &mut dyn FnMut() -> FailureSet,
    registry: &ExecutorRegistry,
    runner: &dyn TaskRunner,
) -> RepairStatus {
    // Retry counts carried across rounds, keyed by stable failure id. Each
    // re-assessment rebuilds failures with retry_count=0, so we re-apply the
    // accumulated counts before deciding eligibility.
    let mut retry_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut prev_ids: Option<BTreeSet<String>> = None;
    let mut rounds: usize = 0;

    // The final failure set, carried out of the loop for status synthesis.
    let mut last_fs = FailureSet::default();

    for _round in 0..GLOBAL_ROUND_CAP {
        let mut fs = assess();

        // Re-apply carried retry counts; mark over-budget Open failures InReview.
        for f in fs.0.iter_mut() {
            if let Some(&rc) = retry_count.get(&f.id) {
                f.retry_count = rc;
            }
            if f.status == FailureStatus::Open
                && f.retry_count >= default_budget(f.class)
            {
                f.status = FailureStatus::InReview;
            }
        }

        last_fs = fs.clone();

        if fs.all_resolved() {
            let status = from_final(&fs, rounds, FAILING_THRESHOLD);
            persist_status(&status, root);
            return status;
        }

        // Eligible-for-repair failures this round.
        let open_ids: Vec<String> =
            fs.open(default_budget).iter().map(|f| f.id.clone()).collect();
        if open_ids.is_empty() {
            break;
        }

        // Oscillation guard: identical failure-id set two rounds running and we
        // still have open work means we are making no progress — stop.
        let cur_ids = fs.ids();
        if prev_ids.as_ref() == Some(&cur_ids) {
            break;
        }

        // Snapshot the writable surface before mutating anything this round so a
        // regressive round can be rolled back byte-for-byte.
        let snapshot = Snapshotter::take(root).ok();

        // Targets we attempt this round (the open set). Tracked so the
        // regression guard can decide whether they were resolved.
        let round_targets: BTreeSet<String> = open_ids.iter().cloned().collect();

        // Dispatch each open failure to its class executor.
        for f in fs.open(default_budget) {
            let outcome = match registry.for_class(f.class) {
                Some(exec) => exec.repair(f, root, config_dir, runner),
                None => RepairOutcome::Unrepairable(format!(
                    "no executor registered for {:?}",
                    f.class
                )),
            };

            let (outcome_tag, note) = match &outcome {
                RepairOutcome::Applied { deterministic, note } => (
                    "applied",
                    format!("deterministic={deterministic}: {note}"),
                ),
                RepairOutcome::NeedsAgent(directive) => {
                    // Route the agentic need; surfaced for review if offline.
                    let routed = runner.rerun(root, directive);
                    let note = match routed {
                        Ok(()) => format!("routed: {}", directive.instruction),
                        Err(e) => format!("route failed: {e}"),
                    };
                    ("needs_agent", note)
                }
                RepairOutcome::Unrepairable(reason) => {
                    ("unrepairable", reason.clone())
                }
            };

            // Spend one attempt against this failure id.
            let entry = retry_count.entry(f.id.clone()).or_insert(0);
            *entry += 1;

            let log = RepairLogEntry {
                round: rounds,
                failure_id: f.id.clone(),
                class: serde_class(f.class),
                outcome: outcome_tag.to_string(),
                note,
            };
            // Provenance is best-effort; a log write failure must not abort the
            // loop (it would otherwise be unrecoverable mid-round).
            let _ = append_repair_log(root, &log);
        }

        rounds += 1;

        // Regression guard: re-assess and see whether the round *grew* the
        // distinct failure set while leaving its own targets unresolved. If so,
        // the round did net harm — roll the snapshot back and route the targets
        // to review so we do not thrash on them again.
        let next_fs = assess();
        let next_ids = next_fs.ids();
        let grew = next_ids.len() > cur_ids.len();
        let targets_unresolved = round_targets.iter().any(|id| {
            next_fs
                .0
                .iter()
                .any(|f| &f.id == id && f.status != FailureStatus::Resolved)
        });
        if grew && targets_unresolved {
            if let Some(snap) = snapshot {
                let _ = snap.rollback(root);
            }
            // Exhaust the targets' budgets so they are forced to review on the
            // next assessment (over-budget Open -> InReview above).
            for id in &round_targets {
                retry_count
                    .entry(id.clone())
                    .and_modify(|rc| *rc = (*rc).max(GLOBAL_ROUND_CAP))
                    .or_insert(GLOBAL_ROUND_CAP);
            }
        }

        prev_ids = Some(cur_ids);
    }

    // Post-loop: mark any remaining Open failure as InReview, then synthesize.
    for f in last_fs.0.iter_mut() {
        if f.status == FailureStatus::Open {
            f.status = FailureStatus::InReview;
        }
    }
    let status = from_final(&last_fs, rounds, FAILING_THRESHOLD);
    persist_status(&status, root);
    status
}

/// Persist status best-effort; a failed write must not change the verdict the
/// caller observes (the loop already converged in memory).
fn persist_status(status: &RepairStatus, root: &Path) {
    let _ = status.persist(root);
}

/// Stable string tag for a class, matching its serde snake_case rendering.
fn serde_class(class: super::failure::RepairClass) -> String {
    serde_json::to_value(class)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{class:?}"))
}

/// The production executor registry: one executor per repairable class.
pub fn default_registry() -> ExecutorRegistry {
    let mut reg = ExecutorRegistry::default();
    reg.register(Box::new(executors::narrative::NarrativeCorrection));
    reg.register(Box::new(executors::conformance::ConformanceFix));
    reg.register(Box::new(executors::agentic::EvidenceCompletion));
    reg.register(Box::new(executors::agentic::CitationFix));
    reg.register(Box::new(executors::agentic::CoverageGap));
    reg.register(Box::new(executors::agentic::AnalysisRerun));
    reg.register(Box::new(executors::equivalence::EquivalenceRerun));
    reg
}

/// Production entry point: assess the real package on disk each round and drive
/// the loop with the [`default_registry`].
pub fn run_repair_loop(
    root: &Path,
    config_dir: &Path,
    runner: &dyn TaskRunner,
) -> anyhow::Result<RepairStatus> {
    let registry = default_registry();
    let mut assess = || assess_package(root, config_dir).unwrap_or_default();
    Ok(run_loop(root, config_dir, &mut assess, &registry, runner))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair_loop::executor::Executor;
    use crate::repair_loop::failure::{Failure, FailureSource, RepairClass};
    use crate::repair_loop::runner::RepairDirective;
    use crate::repair_loop::status::RepairVerdict;
    use std::cell::Cell;
    use std::path::Path;

    /// A runner that never invokes an agent and always succeeds.
    struct NoRunner;
    impl TaskRunner for NoRunner {
        fn rerun(&self, _pkg: &Path, _directive: &RepairDirective) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Executor that always reports `Applied` for a fixed class.
    struct AlwaysApply {
        class: RepairClass,
    }
    impl Executor for AlwaysApply {
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
                note: "always-apply".to_string(),
            }
        }
    }

    /// Executor that always reports `NeedsAgent` for a fixed class.
    struct NeedsAgentExec {
        class: RepairClass,
    }
    impl Executor for NeedsAgentExec {
        fn class(&self) -> RepairClass {
            self.class
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
                instruction: format!("rerun {}", f.subject),
            })
        }
    }

    fn mk(class: RepairClass, subject: &str, status: FailureStatus) -> Failure {
        let mut f = Failure::new(
            FailureSource::ClaimMismatch,
            class,
            "task_a",
            subject,
            "detail",
        );
        f.status = status;
        f
    }

    fn registry_with(execs: Vec<Box<dyn Executor>>) -> ExecutorRegistry {
        let mut reg = ExecutorRegistry::default();
        for e in execs {
            reg.register(e);
        }
        reg
    }

    /// (1) round0 yields one open NarrativeCorrection failure; round1 yields it
    /// Resolved -> FullyPassing.
    #[test]
    fn resolves_to_fully_passing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let calls = Cell::new(0usize);
        let mut assess = || {
            let n = calls.get();
            calls.set(n + 1);
            if n == 0 {
                FailureSet(vec![mk(
                    RepairClass::NarrativeCorrection,
                    "claim_1",
                    FailureStatus::Open,
                )])
            } else {
                FailureSet(vec![mk(
                    RepairClass::NarrativeCorrection,
                    "claim_1",
                    FailureStatus::Resolved,
                )])
            }
        };
        let reg = registry_with(vec![Box::new(AlwaysApply {
            class: RepairClass::NarrativeCorrection,
        })]);
        let status = run_loop(root, root, &mut assess, &reg, &NoRunner);
        assert_eq!(
            status.verdict,
            RepairVerdict::FullyPassing,
            "narrative failure resolved next round must be FullyPassing, got {status:?}"
        );
        assert!(
            status.review.is_empty(),
            "no review items expected on full pass, got {:?}",
            status.review
        );
        assert!(
            status.rounds >= 1,
            "at least one repair round must have run, got {}",
            status.rounds
        );
    }

    /// (2) A ReviewRequired failure (budget 0) always present -> MostlyPassing
    /// with exactly one review item.
    #[test]
    fn review_required_routes_to_review() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let mut assess = || {
            FailureSet(vec![mk(
                RepairClass::ReviewRequired,
                "needs_human",
                FailureStatus::Open,
            )])
        };
        // No executor registered for ReviewRequired (budget 0 -> never open).
        let reg = registry_with(vec![]);
        let status = run_loop(root, root, &mut assess, &reg, &NoRunner);
        assert_eq!(
            status.verdict,
            RepairVerdict::MostlyPassing,
            "a single review-required failure must be MostlyPassing, got {status:?}"
        );
        assert_eq!(
            status.review.len(),
            1,
            "exactly one review item expected, got {:?}",
            status.review
        );
        assert!(
            status.rounds <= GLOBAL_ROUND_CAP,
            "must terminate within the global round cap"
        );
    }

    /// (3) An unresolving CitationFix failure -> terminates within the round cap
    /// as MostlyPassing (oscillation guard: ids never change).
    #[test]
    fn unresolving_failure_terminates_mostly_passing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        // Always the same single open failure (NeedsAgent that never resolves).
        let mut assess = || {
            FailureSet(vec![mk(
                RepairClass::CitationFix,
                "cite_1",
                FailureStatus::Open,
            )])
        };
        let reg = registry_with(vec![Box::new(NeedsAgentExec {
            class: RepairClass::CitationFix,
        })]);
        let status = run_loop(root, root, &mut assess, &reg, &NoRunner);
        assert!(
            status.rounds <= GLOBAL_ROUND_CAP,
            "loop must terminate within {GLOBAL_ROUND_CAP} rounds, ran {}",
            status.rounds
        );
        assert_eq!(
            status.verdict,
            RepairVerdict::MostlyPassing,
            "an unresolving failure must end MostlyPassing, got {status:?}"
        );
        assert_eq!(
            status.review.len(),
            1,
            "the unresolving failure must be the single review item"
        );
    }

    /// (4) Regression: an executor "fixes" target A but the next assess shows a
    /// NEW failure B -> driver rolls back and routes A to review.
    #[test]
    fn regression_rolls_back_and_reviews_target() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        // Seed a writable narrative so the snapshot has something to restore.
        let task_dir = root.join("runtime").join("task_a");
        std::fs::create_dir_all(&task_dir).expect("mk task dir");
        let report = task_dir.join("report.md");
        std::fs::write(&report, b"ORIGINAL\n").expect("seed report");

        let calls = Cell::new(0usize);
        let mut assess = || {
            let n = calls.get();
            calls.set(n + 1);
            // Round 0 assess: only A open.
            // After the round's repair, the driver re-assesses (call 1): A still
            // open AND a NEW failure B appears -> regression. Thereafter A is
            // forced to review and B persists, but the id set no longer grows.
            if n == 0 {
                FailureSet(vec![mk(
                    RepairClass::NarrativeCorrection,
                    "A",
                    FailureStatus::Open,
                )])
            } else {
                // B is a fresh, unrelated failure that has NO registered
                // executor (ReviewRequired), so it never re-runs the corrupting
                // executor on later rounds; it exists only to grow the id set
                // and trip the regression guard.
                FailureSet(vec![
                    mk(RepairClass::NarrativeCorrection, "A", FailureStatus::Open),
                    mk(RepairClass::ReviewRequired, "B", FailureStatus::Open),
                ])
            }
        };
        // The executor "corrupts" the writable report when it runs; the
        // regression rollback must restore the ORIGINAL bytes byte-for-byte.
        struct Corrupting {
            report: std::path::PathBuf,
        }
        impl Executor for Corrupting {
            fn class(&self) -> RepairClass {
                RepairClass::NarrativeCorrection
            }
            fn repair(
                &self,
                _f: &Failure,
                _pkg: &Path,
                _config_dir: &Path,
                _runner: &dyn TaskRunner,
            ) -> RepairOutcome {
                std::fs::write(&self.report, b"CORRUPTED\n").expect("corrupt report");
                RepairOutcome::Applied {
                    deterministic: true,
                    note: "corrupting".to_string(),
                }
            }
        }
        let reg = registry_with(vec![Box::new(Corrupting {
            report: report.clone(),
        })]);

        let status = run_loop(root, root, &mut assess, &reg, &NoRunner);

        // Rollback must have restored the original bytes after the regressive
        // round corrupted the file.
        let restored = std::fs::read_to_string(&report).expect("read report");
        assert_eq!(
            restored, "ORIGINAL\n",
            "regression rollback must restore the snapshot byte-for-byte"
        );

        // A must be routed to review (forced over budget by the regression guard).
        assert!(
            status
                .review
                .iter()
                .any(|r| r.failure.subject == "A"),
            "target A must be routed to review after regression, got {:?}",
            status.review
        );
        assert!(
            status.rounds <= GLOBAL_ROUND_CAP,
            "must terminate within the round cap"
        );
    }
}
