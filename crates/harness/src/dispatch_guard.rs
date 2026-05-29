//! No-progress dispatch guard.
//!
//! # Problem
//!
//! The harness re-dispatches a task whose prior agent process is gone
//! (orphan recovery) but that never reached a terminal state. When an
//! agent *fast-fails* every time — e.g. the per-task container lacks the
//! runtime the agent needs (`python3: command not found` in an R-only
//! image), or the agent crashes before writing a terminal
//! `state.patch.json` — each re-dispatch refreshes the task's heartbeat,
//! so the heartbeat-stall detector never accumulates its window and the
//! task is re-dispatched forever (observed: a clinical-trial `data_import`
//! reached dispatch iteration 31 with no terminal patch). The agent burns
//! cycles in an unproductive loop and the workflow never converges.
//!
//! Heartbeat-stall can't catch this because the loop keeps the heartbeat
//! fresh. What it misses is *progress*: the agent keeps being invoked but
//! the task never leaves its non-terminal state.
//!
//! # Guard
//!
//! [`NoProgressGuard`] counts, per task, consecutive *dispatches that
//! ended without the task reaching a terminal state* (Completed / Failed /
//! Blocked all count as terminal — a typed blocker is a legitimate
//! outcome, not no-progress). Reaching a terminal state resets the count.
//! Once a task exhausts its budget the guard returns a block reason so the
//! caller can transition it to `Blocked` and break the loop, surfacing the
//! failure to the operator instead of spinning silently.
//!
//! # Why gating on the dispatched ("picks") set is regression-safe
//!
//! The caller only feeds the guard tasks that were actually (re-)dispatched
//! this iteration. A genuinely long-running task whose agent is alive is
//! *not* re-dispatched (the orphan-recovery `is_live` heartbeat probe skips
//! it), so it is observed at most once (its first dispatch) and never
//! accumulates a no-progress count. Only tasks whose agent keeps dying
//! without progress are re-fed, so only crash loops trip the guard.

use std::collections::BTreeMap;

/// Default consecutive no-progress dispatch budget before a task is
/// force-blocked. Generous enough to tolerate a handful of transient
/// orphan recoveries (spot reclaim, transient crash) while still bounding
/// an unproductive crash loop. Override with
/// `ECAA_HARNESS_MAX_NOPROGRESS_DISPATCHES`.
pub const DEFAULT_MAX_NOPROGRESS_DISPATCHES: u32 = 8;

/// Resolve the no-progress budget from the environment, falling back to
/// [`DEFAULT_MAX_NOPROGRESS_DISPATCHES`]. A value of `0` or an unparseable
/// value disables the guard (returns `u32::MAX`) so an operator can opt
/// out without code changes.
pub fn max_noprogress_dispatches_from_env() -> u32 {
    match std::env::var("ECAA_HARNESS_MAX_NOPROGRESS_DISPATCHES") {
        Ok(v) => match v.trim().parse::<u32>() {
            Ok(0) => u32::MAX,
            Ok(n) => n,
            Err(_) => DEFAULT_MAX_NOPROGRESS_DISPATCHES,
        },
        Err(_) => DEFAULT_MAX_NOPROGRESS_DISPATCHES,
    }
}

/// Per-task counter of consecutive dispatches that made no progress.
#[derive(Debug, Clone)]
pub struct NoProgressGuard {
    max: u32,
    counts: BTreeMap<String, u32>,
}

impl NoProgressGuard {
    /// New guard with the given budget. A `max` of `0` is treated as
    /// "disabled" (`u32::MAX`) so callers can pass an env-resolved value
    /// directly.
    pub fn new(max: u32) -> Self {
        Self {
            max: if max == 0 { u32::MAX } else { max },
            counts: BTreeMap::new(),
        }
    }

    /// New guard with the budget resolved from the environment.
    pub fn from_env() -> Self {
        Self::new(max_noprogress_dispatches_from_env())
    }

    /// Record the outcome of a dispatch for `task_id`.
    ///
    /// `reached_terminal` is true when the task's post-harvest state is
    /// `Completed`, `Failed`, or `Blocked`. A terminal outcome resets the
    /// task's no-progress count and returns `None`.
    ///
    /// A non-terminal outcome increments the count; once it reaches the
    /// budget the count is cleared and a block reason is returned so the
    /// caller force-blocks the task. The clear-on-fire keeps the guard
    /// idempotent if the caller's block write is racy.
    pub fn observe(&mut self, task_id: &str, reached_terminal: bool) -> Option<String> {
        if reached_terminal {
            self.counts.remove(task_id);
            return None;
        }
        let entry = self.counts.entry(task_id.to_string()).or_insert(0);
        *entry = entry.saturating_add(1);
        if *entry >= self.max {
            self.counts.remove(task_id);
            return Some(format!(
                "Agent returned {} consecutive times without writing a terminal state patch \
                 (no completed/failed/blocked transition). The task is not making progress — \
                 likely a container/runtime incompatibility (e.g. the image lacks python3 or \
                 node), a crash before the agent writes state.patch.json, or a wedged background \
                 loop. Inspect runtime/outputs/{task_id}/{{error.json,agent-claude.log,progress.log}}. \
                 Force-blocked to stop the re-dispatch loop.",
                self.max
            ));
        }
        None
    }

    /// Current no-progress count for a task (0 when absent). Diagnostic.
    pub fn count(&self, task_id: &str) -> u32 {
        self.counts.get(task_id).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_budget_of_consecutive_no_progress() {
        let mut g = NoProgressGuard::new(3);
        assert_eq!(g.observe("t", false), None, "dispatch 1");
        assert_eq!(g.observe("t", false), None, "dispatch 2");
        let fired = g.observe("t", false);
        assert!(fired.is_some(), "dispatch 3 must trip the guard at budget=3");
        assert!(fired.unwrap().contains("without writing a terminal state patch"));
    }

    #[test]
    fn terminal_outcome_resets_the_count() {
        let mut g = NoProgressGuard::new(3);
        g.observe("t", false);
        g.observe("t", false);
        assert_eq!(g.count("t"), 2);
        // A terminal transition (e.g. the agent finally wrote a blocked
        // patch) clears the budget.
        assert_eq!(g.observe("t", true), None);
        assert_eq!(g.count("t"), 0);
        // ...and the task gets a fresh budget afterward.
        assert_eq!(g.observe("t", false), None);
        assert_eq!(g.count("t"), 1);
    }

    #[test]
    fn per_task_independent() {
        let mut g = NoProgressGuard::new(2);
        assert_eq!(g.observe("a", false), None);
        // b's first no-progress must not be charged a's prior count.
        assert_eq!(g.observe("b", false), None);
        assert!(g.observe("a", false).is_some(), "a hits budget=2");
        assert!(g.observe("b", false).is_some(), "b hits budget=2");
    }

    #[test]
    fn first_dispatch_of_a_long_task_never_blocks() {
        // A genuine long-running task is dispatched once (then polled, not
        // re-dispatched while its agent lives), so the guard sees it at
        // most once and never trips.
        let mut g = NoProgressGuard::new(8);
        assert_eq!(g.observe("long_task", false), None);
        assert_eq!(g.count("long_task"), 1);
    }

    #[test]
    fn budget_zero_disables_guard() {
        let mut g = NoProgressGuard::new(0);
        for _ in 0..1000 {
            assert_eq!(g.observe("t", false), None);
        }
    }
}
