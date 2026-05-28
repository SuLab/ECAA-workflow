//! Saga: ordered steps with rollback for non-atomic transitions.
//!
//! When the emit pipeline, broadcaster reset, or SessionStore prune
//! fails partway through, we need explicit rollback rather than silent
//! corruption. A `Saga` is a sequence of `SagaStep`s; each step has a
//! `forward` closure and an optional `rollback`. On any step's failure,
//! prior steps' rollbacks fire in reverse order.
//!
//! Use cases (wired by downstream migrations):
//! - emit_with_conversation_log: backup → promote → cleanup, with
//!   rollback restoring the prior package directory on partial failure.
//! - SessionStore boot recovery: synthesize Blocker for sessions stuck
//!   in Emitting state at process restart.
//! - Broadcaster restart: emit synthetic ResyncRequired event for every
//!   subscriber on the first subscribe after process boot.

use anyhow::Result;
use std::sync::Arc;

/// One forward+rollback unit of work.
pub struct SagaStep {
    name: &'static str,
    forward: Arc<dyn Fn() -> Result<()> + Send + Sync>,
    rollback: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl SagaStep {
    /// Construct a step with mandatory forward closure and optional
    /// rollback. `name` is used in tracing only.
    pub fn new<F, R>(name: &'static str, forward: F, rollback: Option<R>) -> Self
    where
        F: Fn() -> Result<()> + Send + Sync + 'static,
        R: Fn() + Send + Sync + 'static,
    {
        SagaStep {
            name,
            forward: Arc::new(forward),
            rollback: rollback.map(|r| Arc::new(r) as Arc<dyn Fn() + Send + Sync>),
        }
    }

    /// Convenience: step with NO rollback. Use when the forward action
    /// is naturally idempotent or when rollback is meaningless (e.g.,
    /// final cleanup steps).
    pub fn forward_only<F>(name: &'static str, forward: F) -> Self
    where
        F: Fn() -> Result<()> + Send + Sync + 'static,
    {
        SagaStep {
            name,
            forward: Arc::new(forward),
            rollback: None,
        }
    }
}

/// Ordered sequence of steps with reverse-rollback on failure.
pub struct Saga {
    steps: Vec<SagaStep>,
}

impl Saga {
    /// New.
    pub fn new() -> Self {
        Saga { steps: Vec::new() }
    }

    /// Step.
    pub fn step(mut self, step: SagaStep) -> Self {
        self.steps.push(step);
        self
    }

    /// Execute the saga.
    ///
    /// On success: returns `Ok(())`; all steps ran in order.
    /// On failure: the failed step's error is returned; all prior steps'
    /// rollbacks fired in reverse order. The failed step is NOT rolled
    /// back (it didn't complete).
    pub fn execute(self) -> Result<()> {
        let mut completed: Vec<&SagaStep> = Vec::with_capacity(self.steps.len());
        for step in &self.steps {
            tracing::debug!(target: "swfc::saga", saga_step = step.name, "executing");
            match (step.forward)() {
                Ok(()) => {
                    completed.push(step);
                }
                Err(e) => {
                    tracing::warn!(
                        target: "swfc::saga",
                        saga_step = step.name,
                        error = %e,
                        completed_steps = completed.len(),
                        "step failed; rolling back"
                    );
                    for done in completed.iter().rev() {
                        if let Some(rb) = &done.rollback {
                            tracing::debug!(
                                target: "swfc::saga",
                                saga_step = done.name,
                                "rolling back"
                            );
                            // Roll back even if a prior rollback fails (no fail-on-fail).
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (rb)()));
                            if let Err(panic) = result {
                                tracing::error!(
                                    target: "swfc::saga",
                                    saga_step = done.name,
                                    panic = ?panic,
                                    "rollback panicked; continuing"
                                );
                            }
                        }
                    }
                    return Err(e);
                }
            }
        }
        Ok(())
    }
}

impl Default for Saga {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn happy_path_runs_steps_in_order() {
        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let log_a = log.clone();
        let log_b = log.clone();
        let log_c = log.clone();

        Saga::new()
            .step(SagaStep::forward_only("a", move || {
                log_a.lock().unwrap().push("do_a");
                Ok(())
            }))
            .step(SagaStep::forward_only("b", move || {
                log_b.lock().unwrap().push("do_b");
                Ok(())
            }))
            .step(SagaStep::forward_only("c", move || {
                log_c.lock().unwrap().push("do_c");
                Ok(())
            }))
            .execute()
            .unwrap();

        assert_eq!(*log.lock().unwrap(), vec!["do_a", "do_b", "do_c"]);
    }

    #[test]
    fn rolls_back_prior_steps_on_failure() {
        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let log_a_do = log.clone();
        let log_a_undo = log.clone();
        let log_b_do = log.clone();
        let log_b_undo = log.clone();
        let log_c_do = log.clone();

        let result = Saga::new()
            .step(SagaStep::new(
                "a",
                move || {
                    log_a_do.lock().unwrap().push("do_a");
                    Ok(())
                },
                Some(move || {
                    log_a_undo.lock().unwrap().push("undo_a");
                }),
            ))
            .step(SagaStep::new(
                "b",
                move || {
                    log_b_do.lock().unwrap().push("do_b");
                    Ok(())
                },
                Some(move || {
                    log_b_undo.lock().unwrap().push("undo_b");
                }),
            ))
            .step(SagaStep::forward_only("c_fails", move || {
                log_c_do.lock().unwrap().push("do_c");
                Err(anyhow::anyhow!("synthetic failure"))
            }))
            .execute();

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("synthetic failure"));

        // Forward: a, b, c (which failed). Rollback: b, a (reverse order).
        // c is not rolled back because it didn't complete.
        assert_eq!(
            *log.lock().unwrap(),
            vec!["do_a", "do_b", "do_c", "undo_b", "undo_a"]
        );
    }

    #[test]
    fn rollback_panic_does_not_abort_remaining_rollbacks() {
        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));

        let log_a = log.clone();
        let log_c = log.clone();

        let result = Saga::new()
            .step(SagaStep::new(
                "a",
                || Ok(()),
                Some(move || {
                    log_a.lock().unwrap().push("undo_a");
                }),
            ))
            .step(SagaStep::new(
                "b_rollback_panics",
                || Ok(()),
                Some(|| {
                    panic!("rollback panic");
                }),
            ))
            .step(SagaStep::forward_only("c_fails", move || {
                log_c.lock().unwrap().push("do_c");
                Err(anyhow::anyhow!("trigger"))
            }))
            .execute();

        assert!(result.is_err());
        // undo_a must run even though undo_b panicked.
        assert!(log.lock().unwrap().contains(&"undo_a"));
    }

    #[test]
    fn forward_only_step_has_no_rollback() {
        let result = Saga::new()
            .step(SagaStep::forward_only("a", || Ok(())))
            .step(SagaStep::forward_only("b_fails", || {
                Err(anyhow::anyhow!("fail"))
            }))
            .execute();
        assert!(result.is_err());
        // No assertion about rollback log — there is none, by design.
    }

    #[test]
    fn empty_saga_succeeds() {
        Saga::new().execute().unwrap();
    }

    #[test]
    fn step_failed_first_no_rollback_runs() {
        let log = Arc::new(Mutex::new(Vec::<&'static str>::new()));
        let log_a = log.clone();

        let _ = Saga::new()
            .step(SagaStep::new(
                "first_fails",
                || Err(anyhow::anyhow!("first")),
                Some(move || {
                    log_a.lock().unwrap().push("undo_a");
                }),
            ))
            .execute();
        assert!(
            log.lock().unwrap().is_empty(),
            "no rollback runs when first step fails"
        );
    }
}
