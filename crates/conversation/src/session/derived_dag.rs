//! Derived-DAG helpers on `Session`.
//!
//! `session.dag` is no longer authoritative. The truth is:
//! - `session.workflow_dag` for STRUCTURE (v4 typed DAG + edges + proofs).
//! - `session.task_states` for per-task RUNTIME state (Pending → Running
//!   → Completed / Failed / Blocked).
//!
//! `session.dag` survives as a **memoization cache**. Readers call
//! [`Session::current_dag`] which either returns the cached value or
//! re-derives from `workflow_dag` + `task_states`. Whenever `workflow_dag`
//! is rebuilt, the cache is invalidated via [`Session::invalidate_dag`]
//! (`session.dag = None`), forcing the next reader to lower fresh.
//!
//! Why: the original merge logic in `send_turn` keyed structural-change
//! detection on `DAG.workflow_id`. That id is `"workflow-{session_id}"` —
//! constant for the life of the session — so the merge permanently kept
//! the FIRST rebuild's DAG, silently discarding every subsequent
//! `rebuild_dag` that produced a structurally different DAG. Live-session
//! RCA classifier locked chip-seq early, refined to
//! single_cell_rnaseq later, but emit_package read the stale chip-seq
//! `session.dag`. Treating `session.dag` as a derived cache eliminates
//! the merge-bug class entirely.

use ecaa_workflow_core::dag::{TaskState, DAG};

use super::state::Session;

impl Session {
    /// Return the **current** legacy `DAG` for this session — lowered
    /// fresh from `workflow_dag` and overlaid with `task_states` — or
    /// `None` when no workflow_dag has been composed yet.
    ///
    /// Uses [`Session::dag`] as a memoization cache when present.
    /// Callers that mutate workflow_dag MUST call
    /// [`Session::invalidate_dag`] first; otherwise the cache lies.
    ///
    /// This method does NOT mutate `self`. The cache fill happens via
    /// [`Session::ensure_dag_cached`] which takes `&mut self`.
    pub fn current_dag(&self) -> Option<DAG> {
        // Fast path: cache populated. Overlay task_states fresh in
        // case the harness wrote new states after the cache filled.
        if let Some(cached) = &self.dag {
            let mut out = cached.clone();
            apply_task_states(&mut out, &self.task_states);
            return Some(out);
        }
        // No cache: re-derive without mutating self. The next mutable
        // call site (e.g. emit_package, rebuild_dag) will populate via
        // `ensure_dag_cached`.
        let workflow_dag = self.workflow_dag.as_ref()?;
        let id = format!("workflow-{}", self.id.as_simple());
        match ecaa_workflow_core::builder::build_dag_from_workflow_dag(workflow_dag, &id) {
            Ok(mut dag) => {
                apply_task_states(&mut dag, &self.task_states);
                Some(dag)
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %self.id,
                    error = %e,
                    "Session::current_dag: lowering workflow_dag failed; returning None"
                );
                None
            }
        }
    }

    /// Populate or refresh the `session.dag` memoization cache. Idempotent:
    /// when `self.dag` is already populated and `task_states` haven't
    /// changed, this is a cheap overlay; otherwise the workflow_dag is
    /// re-lowered.
    ///
    /// Returns the cached DAG (clone) so callers can pass it through to
    /// downstream consumers that expect an owned `DAG`.
    pub fn ensure_dag_cached(&mut self) -> Option<DAG> {
        if self.dag.is_some() {
            // Overlay-only refresh.
            if let Some(dag) = self.dag.as_mut() {
                apply_task_states(dag, &self.task_states);
            }
            return self.dag.clone();
        }
        let workflow_dag = self.workflow_dag.as_ref()?;
        let id = format!("workflow-{}", self.id.as_simple());
        match ecaa_workflow_core::builder::build_dag_from_workflow_dag(workflow_dag, &id) {
            Ok(mut dag) => {
                apply_task_states(&mut dag, &self.task_states);
                self.dag = Some(dag.clone());
                Some(dag)
            }
            Err(e) => {
                tracing::warn!(
                    session_id = %self.id,
                    error = %e,
                    "Session::ensure_dag_cached: lowering workflow_dag failed"
                );
                self.dag = None;
                None
            }
        }
    }

    /// Invalidate the `session.dag` memoization cache. Callers that
    /// rebuild `workflow_dag` MUST call this so the next read re-derives.
    #[deprecated(
        note = "use Session::workflow_dag_mut() (RAII guard) for workflow_dag mutations; \
                this remains for explicit cache resets after non-workflow_dag state changes."
    )]
    pub fn invalidate_dag(&mut self) {
        self.dag = None;
    }

    /// Non-stable probe for regression tests that need to verify cache
    /// invalidation. `#[doc(hidden)] pub` rather than `#[cfg(test)]`
    /// because the server crate's integration tests link against the
    /// regular library build, not the cfg-test build.
    #[doc(hidden)]
    pub fn dag_cache_is_valid(&self) -> bool {
        self.dag.is_some()
    }

    /// Authoritative entry point for per-task runtime state writes.
    ///
    /// Every harness-side state transition routes
    /// through this method, the server's POST
    /// `/api/chat/session/:id/task/:task_id/state` handler, and the
    /// in-process tool-loop merge. The setter:
    ///
    /// - **enforces monotonicity**: refuses to regress a terminal
    ///   (`Completed` / `Failed`) state back to a non-terminal one.
    ///   Returns the EXISTING terminal state on a regression attempt
    ///   so the caller can log the rejection. Terminal-to-terminal
    ///   (`Failed` → `Completed` on a retry that succeeded) is allowed.
    /// - **invalidates the derived DAG cache** so the next reader
    ///   re-derives via [`Session::current_dag`] and observes the
    ///   freshly written state. Without this invalidation, the
    ///   cached `session.dag` would diverge from `task_states` on
    ///   every harness write, reintroducing exactly the staleness
    ///   class the Phase-D cache refactor closed (commit c94e019a).
    ///
    /// Returns the state that ended up in `task_states` after the
    /// call (either `new_state` on success or the preserved terminal
    /// state on a rejected regression).
    pub fn set_task_state(&mut self, task_id: &str, new_state: TaskState) -> TaskState {
        let entry = self
            .task_states
            .entry(task_id.to_string())
            .or_insert(TaskState::Pending);
        if entry.is_terminal() && !new_state.is_terminal() {
            // Monotonicity violation; preserve the terminal state. The
            // caller (HTTP handler / harness binary) should log the
            // rejection so a stale retry doesn't silently re-run a
            // finished task. We deliberately do NOT invalidate the
            // cache here — no mutation took place, so the next reader
            // can keep its memoized DAG.
            return entry.clone();
        }
        *entry = new_state.clone();
        // Cache reset: a write happened, so the memoized
        // `session.dag` must be re-derived on the next
        // `current_dag()` read. The deprecation note on
        // `invalidate_dag` recommends `workflow_dag_mut`, but that
        // RAII guard wraps STRUCTURAL writes (workflow_dag); per-task
        // state writes go through this setter directly.
        #[allow(deprecated)]
        self.invalidate_dag();
        new_state
    }
}

/// Walk `dag.tasks` and overlay state from `task_states`. Tasks without
/// an entry default to whatever the lowering produced (typically Pending).
fn apply_task_states(dag: &mut DAG, task_states: &std::collections::BTreeMap<String, TaskState>) {
    for (id, t) in dag.tasks.iter_mut() {
        if let Some(state) = task_states.get(id.as_str()) {
            t.state = state.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::dag::TaskState;

    fn fresh_session() -> Session {
        Session::new(false)
    }

    #[test]
    fn current_dag_returns_none_when_no_workflow_dag() {
        let s = fresh_session();
        assert!(s.current_dag().is_none());
    }

    #[test]
    fn set_task_state_records_in_task_states_map() {
        let mut s = fresh_session();
        // `Pending` is the unit variant; struct variants (Running /
        // Completed / Failed / Blocked) carry fields and are covered
        // by integration tests where we can populate them legitimately.
        s.set_task_state("alignment", TaskState::Pending);
        assert!(s.task_states.contains_key("alignment"));
        assert!(matches!(
            s.task_states.get("alignment").unwrap(),
            TaskState::Pending
        ));
    }

    #[test]
    fn invalidate_dag_clears_cache_when_set() {
        let mut s = fresh_session();
        // DAG has no Default impl, so we can't synthesize one cheaply
        // here. Just verify invalidation is a no-op on the None case;
        // the populated-cache invalidation is covered by integration
        // tests that drive `rebuild_dag` end-to-end.
        assert!(s.dag.is_none(), "fresh session must have None dag cache");
        #[allow(deprecated)] // test exercises the underlying primitive directly
        s.invalidate_dag();
        assert!(
            s.dag.is_none(),
            "invalidate_dag on a None-cache session must remain None"
        );
    }
}
