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

    // Failure ids for which a routed-to-review repair-log line has already been
    // written, so a failure that persists across many rounds is accounted for
    // exactly once (not re-logged every assess).
    let mut review_logged: BTreeSet<String> = BTreeSet::new();

    // The final failure set, carried out of the loop for status synthesis.
    let mut last_fs = FailureSet::default();

    for _round in 0..GLOBAL_ROUND_CAP {
        let mut fs = assess();

        // Re-apply carried retry counts; mark over-budget Open failures (and
        // budget-0 ReviewRequired failures) InReview. Each such routing gets a
        // repair-log line exactly once (deduped by id) so every review item is
        // accounted for in provenance, not just those an executor attempted.
        for f in fs.0.iter_mut() {
            if let Some(&rc) = retry_count.get(&f.id) {
                f.retry_count = rc;
            }
            if f.status == FailureStatus::Open
                && f.retry_count >= default_budget(f.class)
            {
                f.status = FailureStatus::InReview;
                log_routed_to_review(
                    root,
                    rounds,
                    f,
                    &mut review_logged,
                    "over budget" ,
                );
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
                RepairOutcome::PartiallyApplied {
                    deterministic,
                    note,
                    residual,
                } => {
                    // Real work was done but the failure is NOT closed. It will
                    // re-surface on the next assess and, once over budget, route
                    // to review. Log the partial outcome and its residual here.
                    (
                        "partial",
                        format!(
                            "deterministic={deterministic}: {note}; residual: {residual}"
                        ),
                    )
                }
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
    // Log each one routed to review (deduped) so the repair-log accounts for
    // every review item even when the loop exited via the oscillation/round cap
    // before the failure went over budget.
    for f in last_fs.0.iter_mut() {
        if f.status == FailureStatus::Open {
            f.status = FailureStatus::InReview;
            log_routed_to_review(root, rounds, f, &mut review_logged, "loop exhausted");
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

/// Append a repair-log line for a failure routed to human review, exactly once
/// per failure id (`logged` is the dedupe set across the whole loop). `why` is a
/// short reason ("over budget", "loop exhausted") folded into the note.
fn log_routed_to_review(
    root: &Path,
    round: usize,
    f: &super::failure::Failure,
    logged: &mut BTreeSet<String>,
    why: &str,
) {
    if !logged.insert(f.id.clone()) {
        return;
    }
    let log = RepairLogEntry {
        round,
        failure_id: f.id.clone(),
        class: serde_class(f.class),
        outcome: "routed_to_review".to_string(),
        note: format!("routed to review ({why}): {}", f.subject),
    };
    // Provenance is best-effort; a log write failure must not abort the loop.
    let _ = append_repair_log(root, &log);
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

    /// Executor that always reports `PartiallyApplied` for a fixed class: real
    /// work was done but the failure is not closed (mirrors the substrate case).
    struct PartialExec {
        class: RepairClass,
    }
    impl Executor for PartialExec {
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
            RepairOutcome::PartiallyApplied {
                deterministic: true,
                note: "did the real re-seal".to_string(),
                residual: "validator offline".to_string(),
            }
        }
    }

    /// Read all repair-log entries from `<root>/runtime/repair-log.jsonl`.
    fn read_repair_log(root: &Path) -> Vec<RepairLogEntry> {
        let path = root.join("runtime").join("repair-log.jsonl");
        let Ok(contents) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        contents
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("repair-log line parses"))
            .collect()
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

    /// (5) D1 twin: a ConformanceFix that returns `PartiallyApplied` must NOT be
    /// counted as resolved — the failure stays in review. A sibling
    /// ConformanceFix that returns `Applied` (ordinary drift, modelled by
    /// `AlwaysApply`) DOES resolve. The same class, same loop, two outcomes:
    /// proves PartiallyApplied is not silently promoted to resolved.
    #[test]
    fn partially_applied_stays_in_review_while_applied_resolves() {
        // (a) PartiallyApplied: failure persists Open across assesses (the work
        // never closes it), so it must route to review.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let mut assess = || {
            FailureSet(vec![mk(
                RepairClass::ConformanceFix,
                "substrate_validity",
                FailureStatus::Open,
            )])
        };
        let reg = registry_with(vec![Box::new(PartialExec {
            class: RepairClass::ConformanceFix,
        })]);
        let status = run_loop(root, root, &mut assess, &reg, &NoRunner);
        assert_eq!(
            status.verdict,
            RepairVerdict::MostlyPassing,
            "an unresolved PartiallyApplied failure must end MostlyPassing, got {status:?}"
        );
        assert_eq!(
            status.review.len(),
            1,
            "the partially-applied failure must remain a review item, got {:?}",
            status.review
        );
        assert_eq!(
            status.review[0].failure.subject, "substrate_validity",
            "the substrate failure must be the review item"
        );

        // (b) Applied: an ordinary ConformanceFix that the next assess shows
        // Resolved fully passes — the resolvable path is intact.
        let tmp2 = tempfile::tempdir().expect("tempdir");
        let root2 = tmp2.path();
        let calls = Cell::new(0usize);
        let mut assess2 = || {
            let n = calls.get();
            calls.set(n + 1);
            let status = if n == 0 {
                FailureStatus::Open
            } else {
                FailureStatus::Resolved
            };
            FailureSet(vec![mk(RepairClass::ConformanceFix, "table_drift", status)])
        };
        let reg2 = registry_with(vec![Box::new(AlwaysApply {
            class: RepairClass::ConformanceFix,
        })]);
        let status2 = run_loop(root2, root2, &mut assess2, &reg2, &NoRunner);
        assert_eq!(
            status2.verdict,
            RepairVerdict::FullyPassing,
            "an ordinary ConformanceFix that resolves must end FullyPassing, got {status2:?}"
        );
        assert!(
            status2.review.is_empty(),
            "no review items for a resolved ConformanceFix, got {:?}",
            status2.review
        );
    }

    /// (6) D2 twin: a loop that produces N review items must write a repair-log
    /// line accounting for ALL of them, including a ReviewRequired failure (no
    /// executor ever attempts it) and a PartiallyApplied failure (an executor
    /// did real work but could not close it). Every review item's id must appear
    /// in a `routed_to_review` log line.
    #[test]
    fn every_review_item_gets_a_repair_log_line() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let mut assess = || {
            FailureSet(vec![
                // ReviewRequired: budget 0, never attempted by any executor.
                mk(RepairClass::ReviewRequired, "needs_human", FailureStatus::Open),
                // PartiallyApplied: attempted, real work, never closes.
                mk(RepairClass::ConformanceFix, "substrate_validity", FailureStatus::Open),
            ])
        };
        let reg = registry_with(vec![Box::new(PartialExec {
            class: RepairClass::ConformanceFix,
        })]);
        let status = run_loop(root, root, &mut assess, &reg, &NoRunner);

        // Both failures are unresolved review items.
        assert_eq!(
            status.review.len(),
            2,
            "both failures must be review items, got {:?}",
            status.review
        );

        let log = read_repair_log(root);
        let routed: BTreeSet<String> = log
            .iter()
            .filter(|e| e.outcome == "routed_to_review")
            .map(|e| e.failure_id.clone())
            .collect();
        // Every review item id must be accounted for by a routed_to_review line.
        for item in &status.review {
            assert!(
                routed.contains(&item.failure.id),
                "review item {:?} must have a routed_to_review repair-log line; log={:?}",
                item.failure.subject,
                log
            );
        }
        assert_eq!(
            routed.len(),
            status.review.len(),
            "exactly one routed_to_review line per review item (deduped), got {:?}",
            log
        );

        // The PartiallyApplied failure must ALSO have produced a `partial` line
        // recording the real work that was attempted (not only the review line).
        let partial = mk(RepairClass::ConformanceFix, "substrate_validity", FailureStatus::Open);
        assert!(
            log.iter()
                .any(|e| e.outcome == "partial" && e.failure_id == partial.id),
            "the substrate failure must have a `partial` repair-log line, got {log:?}"
        );
    }
}
