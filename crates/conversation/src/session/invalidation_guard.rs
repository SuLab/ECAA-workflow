//! RAII guard for mutating `session.workflow_dag`.
//!
//! Background: `session.dag` is a memoization cache derived from
//! `session.workflow_dag` + `session.task_states` (see `derived_dag.rs`
//! for the full rationale). Any code path that mutates the source-of-
//! truth `workflow_dag` MUST invalidate the cache, otherwise reads
//! through `Session::current_dag()` return stale structure.
//!
//! The previous discipline was an honor-system call to
//! [`Session::invalidate_dag`] after every mutation. Commit `c94e019a`
//! (Phase D) added invalidates at known sites but missed the proposal
//! signoff splice at `crates/server/src/chat_routes/proposal.rs:163`,
//! re-introducing the stale-read class of bug.
//!
//! This guard makes the bypass a compile-time impossibility:
//!
//! ```rust,ignore
//! // Old (foot-gun):
//! if let Some(dag) = session.workflow_dag.as_mut() {
//! dag.nodes.push(node);
//! // forgot to call session.invalidate_dag() → cache lies
//! }
//!
//! // New (safe):
//! if let Some(dag) = session.workflow_dag_mut().as_mut() {
//! dag.nodes.push(node);
//! }
//! // Drop runs here, invalidating the cache.
//! ```
//!
//! The bare [`Session::invalidate_dag`] is `#[deprecated]` to nudge
//! future contributors to the guard. Existing callsites are explicit
//! cache resets after non-`workflow_dag` state changes (cross-omics
//! composition failure, scope replacement, reset-on-rebuild,
//! forward-slice invalidation); they tag `#[allow(deprecated)]` to
//! preserve the intentional semantic while keeping the deprecation
//! warning live for new code.

use crate::session::Session;
use scripps_workflow_core::workflow_contracts::task_node::WorkflowDag;

/// RAII guard returned by [`Session::workflow_dag_mut`]. While the
/// guard is alive, callers can take `&mut WorkflowDag` via
/// [`WorkflowDagMut::as_mut`] or [`WorkflowDagMut::ensure`]. On drop,
/// the derived `session.dag` cache is invalidated unconditionally.
///
/// The "unconditional" part is deliberate: an early-return after the
/// guard is constructed (e.g. inside a fallible `if let` branch) is
/// indistinguishable from an actual mutation from the type system's
/// perspective. Invalidating on every drop is cheap (sets one
/// `Option` to `None`) and keeps the invariant trivially correct.
pub struct WorkflowDagMut<'a> {
    session: &'a mut Session,
}

impl<'a> WorkflowDagMut<'a> {
    /// `pub(crate)` so external crates can only obtain a guard
    /// through [`Session::workflow_dag_mut`].
    pub(crate) fn new(session: &'a mut Session) -> Self {
        Self { session }
    }

    /// Borrow the underlying `Option<&mut WorkflowDag>`. Returns
    /// `None` when no workflow has been composed yet; the on-drop
    /// invalidation still fires in that case (cheap no-op).
    pub fn as_mut(&mut self) -> Option<&mut WorkflowDag> {
        self.session.workflow_dag.as_mut()
    }

    /// Get-or-create: returns a `&mut WorkflowDag`, inserting
    /// `WorkflowDag::default()` (empty id/nodes/edges) when the
    /// field is currently `None`. Use sparingly — production
    /// rebuild paths replace the whole field via
    /// `session.workflow_dag = Some(composed)` rather than mutating
    /// an in-place default.
    pub fn ensure(&mut self) -> &mut WorkflowDag {
        self.session
            .workflow_dag
            .get_or_insert_with(WorkflowDag::default)
    }
}

impl<'a> Drop for WorkflowDagMut<'a> {
    fn drop(&mut self) {
        // Invalidate the derived DAG cache unconditionally.
        // The bare `invalidate_dag` is deprecated but still the
        // canonical primitive — this guard IS its replacement, so
        // the `#[allow(deprecated)]` here is by construction.
        #[allow(deprecated)]
        self.session.invalidate_dag();
    }
}

impl Session {
    /// Request mutable access to `workflow_dag` through an
    /// RAII guard that invalidates the derived `session.dag` cache
    /// on drop. Prefer this over `session.workflow_dag.as_mut()` so
    /// the cache invariant can't be violated by a missing
    /// `invalidate_dag()` call.
    pub fn workflow_dag_mut(&mut self) -> WorkflowDagMut<'_> {
        WorkflowDagMut::new(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scripps_workflow_core::workflow_contracts::task_node::TaskNode;

    fn fresh_session_with_dag() -> Session {
        let mut s = Session::new(false);
        s.workflow_dag = Some(WorkflowDag {
            id: "wf-test".into(),
            nodes: vec![TaskNode::skeleton("alpha", "intent")],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        });
        // Populate the cache so we can observe its invalidation.
        let _ = s.ensure_dag_cached();
        s
    }

    #[test]
    fn guard_invalidates_cache_on_drop_after_mutation() {
        let mut s = fresh_session_with_dag();
        assert!(
            s.dag_cache_is_valid(),
            "setup: cache populated by ensure_dag_cached"
        );

        {
            let mut guard = s.workflow_dag_mut();
            if let Some(dag) = guard.as_mut() {
                dag.nodes.push(TaskNode::skeleton("beta", "intent"));
            }
        } // drop fires here

        assert!(
            !s.dag_cache_is_valid(),
            "cache must be invalidated after guard drops following a splice"
        );
    }

    #[test]
    fn guard_invalidates_cache_on_drop_even_without_mutation() {
        // Conservative invariant: dropping the guard always invalidates.
        // Cheap (sets Option to None) and removes a foot-gun where an
        // early-return branch skips a mutation we still want to model
        // as "I touched this region" — the type system can't tell
        // mutations from non-mutations once `&mut` was handed out.
        let mut s = fresh_session_with_dag();
        assert!(s.dag_cache_is_valid());
        {
            let _guard = s.workflow_dag_mut();
            // no mutation
        }
        assert!(!s.dag_cache_is_valid());
    }

    #[test]
    fn ensure_creates_default_when_none() {
        let mut s = Session::new(false);
        assert!(s.workflow_dag.is_none());
        {
            let mut guard = s.workflow_dag_mut();
            let dag = guard.ensure();
            dag.nodes.push(TaskNode::skeleton("first", "intent"));
        }
        let dag = s.workflow_dag.as_ref().expect("ensure populated dag");
        assert_eq!(dag.nodes.len(), 1);
        assert_eq!(dag.nodes[0].id, "first");
        // Cache should be invalidated post-drop.
        assert!(!s.dag_cache_is_valid());
    }
}
