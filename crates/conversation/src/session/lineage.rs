//! branch-lineage types + `Session::branch_from`.
//!
//! forked a child session from a parent session; the
//! parent/child graph drives the `SessionTree` sidebar in the UI. This
//! module owns the wire type + the constructor.

use super::{Session, SessionId, SessionLineage as _SessionLineageAlias};
use chrono::{DateTime, Utc};
use rand::RngCore;
use ecaa_workflow_core::dag::TaskState;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use ts_rs::TS;
use uuid::Uuid;

/// Schema version stamped onto every newly-written
/// `SessionLineage`. Bumped when the wire shape changes; older
/// session JSON omits the field and `serde(default)` falls through to
/// the canonical lineage SemVer. Pairs with `Session::schema_version`
/// (S4.6 / S9.7) so a future migration upcasting Session shape can
/// also upcast embedded lineage records as needed.
///
/// v3 P7 — wraps the canonical lineage SemVer behind a fn so the
/// `SemVer` constructor stays at-rest in `core::migration`.
pub fn session_lineage_schema_version() -> semver::Version {
    ecaa_workflow_core::migration::current_session_lineage_version()
}

fn default_session_lineage_schema_version() -> semver::Version {
    session_lineage_schema_version()
}

/// Records this session's parent
/// + when/where the branch was taken.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct SessionLineage {
    /// Schema version. `#[serde(default)]` so old session
    /// Files (written before the field) deserialize as the
    /// canonical SemVer.
    ///
    /// v3 P7 — promoted from `u32` to `semver::Version`. The
    /// `schema_version_serde` adapter accepts both legacy `u64` JSON
    /// values and canonical SemVer strings, so pre-P7 lineage records
    /// round-trip without migration.
    #[serde(
        default = "default_session_lineage_schema_version",
        with = "ecaa_workflow_core::migration::schema_version_serde"
    )]
    #[ts(type = "string")]
    #[schemars(with = "String")]
    pub schema_version: semver::Version,
    /// Session id of the parent (branched-from) session.
    #[ts(type = "string")]
    pub parent_session_id: SessionId,
    /// UTC timestamp when the branch was created.
    #[ts(type = "string")]
    pub branched_at: DateTime<Utc>,
    /// Index into the parent's `conversation` Vec at the moment the
    /// branch was taken, so the UI can render where the divergence
    /// happened. None when the branch was taken from intake state
    /// (no conversation yet).
    #[serde(default)]
    #[ts(optional)]
    pub branched_from_turn_index: Option<usize>,
    /// parent's emitted package path at the moment
    /// the branch was taken, used by the cross-version diff at child-
    /// emit time to locate `results/tables/*.{csv,tsv}`. None when the
    /// parent had not yet emitted at branch time.
    #[serde(default)]
    #[ts(optional, type = "string")]
    pub parent_emitted_package_path: Option<PathBuf>,
    /// Task id at whose boundary the branch was taken (M1.3). When set,
    /// the child DAG was snapshotted with this task reset to Ready and all
    /// its transitive successors reset to Pending. None for session-scoped
    /// branches (M1.1 behaviour) and for lineage records written before
    /// this field was added (`serde(default)` gives those None).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branched_from_task_id: Option<String>,
}

// Suppress the `unused_imports` warning for the alias above. Rust keeps
// the `use super::SessionLineage as _SessionLineageAlias` import only
// to make sure the type round-trips through the parent re-export; the
// real definition lives in this file.
#[allow(dead_code)] // reserved-for-import-alive: keeps the parent re-export round-trip honest
type _KeepAliasAlive = _SessionLineageAlias;

impl Session {
    /// Fork a new session from `parent` at the current turn index, with an
    /// optional task boundary. When `task_id` is `Some`, the child's DAG is
    /// snapshotted with the named task reset to `Ready` and all transitive
    /// successors reset to `Pending`; predecessors keep their `Completed`
    /// state. The lineage record carries `branched_from_task_id` when set.
    ///
    /// When `task_id` is `None` this is equivalent to the existing
    /// `branch_from` — the full DAG state is inherited unchanged (M1.1).
    pub fn branch_from_at_task(
        parent: &Session,
        careful_mode: bool,
        task_id: Option<String>,
    ) -> Self {
        let mut child = Self::branch_from(parent, careful_mode);
        if let Some(ref tid) = task_id {
            if child.dag.is_none() {
                child.ensure_dag_cached();
            }
            // Reset the DAG at the task boundary. If the parent has no
            // dag or the task id is unknown, log and skip (the caller's
            // `branch_session_with_task` tool already validated the id).
            if let Some(dag) = &mut child.dag {
                let descendants = dag.descendants_of(tid);
                match dag.reset_to_task_boundary(tid) {
                    Ok(_) => {
                        child.task_states.insert(tid.clone(), TaskState::Ready);
                        for descendant in descendants {
                            child
                                .task_states
                                .insert(descendant.to_string(), TaskState::Pending);
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "ecaa::branch",
                            task_id = %tid,
                            error = %e,
                            "reset_to_task_boundary on child dag failed; dag kept as-is"
                        );
                    }
                }
            }
            // Stamp the lineage with the task boundary.
            if let Some(ref mut lin) = child.lineage {
                lin.branched_from_task_id = Some(tid.clone());
            }
        }
        child
    }

    /// Fork a new session from
    /// `parent` at the current turn index. The new session inherits
    /// the parent's intake prose, classification, taxonomy, DAG, and
    /// intake_methods so the branch picks up where the parent left
    /// off — but resets user_confirmed, emitted_package_path, and
    /// the run-time audit log so the branch's choices don't leak
    /// into the parent's history.
    pub fn branch_from(parent: &Session, careful_mode: bool) -> Self {
        let now = Utc::now();
        let turn_index = if parent.conversation.is_empty() {
            None
        } else {
            Some(parent.conversation.len())
        };
        Self {
            // Branched sessions inherit the parent's schema_version
            // so an upcasting migration that's already touched the
            // parent doesn't need to re-run on the child. Per S9.7.
            // v3 P7 — `schema_version` is now `semver::Version` which
            // does not implement `Copy`; clone explicitly.
            schema_version: parent.schema_version.clone(),
            // Per S6.13: branched sessions inherit the parent's
            // committed composer_version so the branch stays on the
            // same composer the parent used. Switching composer
            // mid-session (parent on v1, child on v2) would mix
            // emission shapes in the same session-tree and break
            // cross-version-diff alignment.
            composer_version: parent.composer_version,
            // Branched sessions DO NOT inherit the
            // parent's pilot_recommendation; the branch may swap
            // methods that change resource shape, so a fresh pilot
            // (or no pilot) is the safe default. Operators can
            // explicitly carry over via the API if they know the
            // shape is unchanged.
            pilot_recommendation: None,
            id: Uuid::new_v4(),
            created_at: now,
            last_activity: now,
            state: parent.state.clone(),
            conversation: parent.conversation.clone(),
            intake_prose: parent.intake_prose.clone(),
            intake_methods: parent.intake_methods.clone(),
            excluded_atoms: parent.excluded_atoms.clone(),
            // Branches inherit the parent's
            // SME-method-signal map so methods the SME already named
            // on the parent stay set-pinnable on the branch without
            // requiring the SME to re-click each quick-reply chip.
            sme_method_signals: parent.sme_method_signals.clone(),
            classification: parent.classification.clone(),
            taxonomy: parent.taxonomy.clone(),
            workflow_intent: parent.workflow_intent.clone(),
            project_class: parent.project_class,
            // Branches inherit the parent's mode but reset the lock so
            // the branch can re-confirm in its own Confirmation turn.
            mode: parent.mode.clone(),
            mode_locked: false,
            // Branches inherit the parent's checkpoint mode (the SME
            // rarely wants to switch discipline mid-branch; they'd
            // start a new session instead).
            checkpoint_mode: parent.checkpoint_mode,
            dag: parent.dag.clone(),
            // Branches inherit the parent's task_states snapshot;
            // execution-driven state will diverge as the branch runs.
            task_states: parent.task_states.clone(),
            // Branches do NOT inherit the parent's confirmation token.
            // The branch is a different emission and the SME must
            // re-confirm its plan shape (which by definition differs
            // from the parent — that's why they branched). Also reset
            // pending_emission_id so the branch flows through its own
            // PendingConfirmation gate.
            confirmation_token: None,
            pending_emission_id: None,
            emitted_package_path: None,
            harness_events: vec![],
            tool_call_log: vec![],
            // Branches INHERIT the parent's decision history so audit
            // continuity holds across the fork. Without inheritance,
            // the branch's audit trail appears to begin at the branch
            // point, hiding the pre-fork SetIntakeField /
            // SetIntakeMethod / Confirm chain. Replay tooling needs
            // the full lineage from intake through the split. The
            // split index is preserved via
            // `lineage.branched_from_turn_index`; downstream readers
            // walk forward from that index to find branch-local
            // decisions.
            decisions: parent.decisions.clone(),
            careful_mode,
            // Branches start with a fresh Blocked-escalation guard —
            // a new branch re-earns its one Opus turn on first block.
            blocked_opus_escalation_consumed: false,
            lineage: Some(SessionLineage {
                schema_version: session_lineage_schema_version(),
                parent_session_id: parent.id,
                branched_at: now,
                branched_from_turn_index: turn_index,
                parent_emitted_package_path: parent.emitted_package_path.clone(),
                // `branch_from` is the session-scoped path (M1.1); no task
                // boundary. `branch_from_at_task` stamps the task id after the
                // fact when `task_id` is Some.
                branched_from_task_id: None,
            }),
            // Branches don't inherit the parent's title: a branched
            // session is semantically a new direction and re-using the
            // parent's Haiku-generated title would be misleading in
            // the SessionTree. `None` prompts a fresh auto-title once
            // the branch has enough turns.
            title: None,
            // Inherit the parent's budget + authorship so a branched
            // session starts with the same cap its parent was working
            // under. The SME can adjust via the budget endpoint.
            budget_usd: parent.budget_usd,
            budget_set_by: parent.budget_set_by.clone(),
            budget_set_at: parent.budget_set_at,
            // Share tokens don't inherit — each session issues its own.
            share_tokens: vec![],
            // Branches inherit the parent's data inputs by reference
            // (same root_path, same file manifest). The branched
            // workflow is a different analysis on the same data; cloning
            // the input list is the right default. The SME can remove
            // or add inputs on the branch via the Inputs tab.
            inputs: parent.inputs.clone(),
            // Branches do NOT inherit pending hints — they're tied to
            // the parent's intake-prose state and may not be relevant
            // to the branch's altered direction. Fresh extraction on
            // any new prose the SME types on the branch will repopulate.
            pending_input_hints: vec![],
            // Same owner. Cross-user branching is a permission concern
            // we'll handle once an auth proxy is wired up.
            owner_user: parent.owner_user.clone(),
            // Branches do not inherit pending-amendment
            // context. The branch is a new direction and the parent's
            // amend chain (if any) is captured already via
            // `lineage.parent_emitted_package_path`.
            pending_amendment: None,
            // Branched sessions start without a pending disambiguation
            // latch; the parent's disambiguation history is captured
            // via the conversation log.
            pending_disambiguation: None,
            // Fresh empty per Session::new.
            deferred_state_triggers: Vec::new(),
            // Branches inherit the parent's archetype
            // snapshot so the branch composes against the same
            // archetype version. A branch that wants to migrate to a
            // newer archetype version must explicitly clear the
            // snapshot via the dedicated migration tool.
            archetype_snapshot: parent.archetype_snapshot.clone(),
            // Branches inherit the authoritative v4 workflow DAG so
            // the UI, package re-emission, and execution harness all
            // see the same graph on the child. Runtime state still
            // diverges through `task_states` and task-boundary reset.
            workflow_dag: parent.workflow_dag.clone(),
            compose_outcome: parent.compose_outcome.clone(),
            ranked_alternatives: Vec::new(),
            policy_decisions: Vec::new(),
            // Branches inherit the parent's active
            // policy bundle so a clinical-trial branch stays under
            // clinical policy. SME can clear via the
            // policy-bundle endpoint if the branch is a private
            // exploration that doesn't need the gate.
            active_policy_bundle: parent.active_policy_bundle.clone(),
            // Flexible plotting upgrade plan branches start
            // with an empty renderer-proposals registry. The parent's
            // proposals are not inherited: a branch is a new analysis
            // direction and the SME may describe different preferred
            // renderers on the new branch.
            renderer_proposals: crate::session::state::RendererProposals::default(),
            // Branches
            // start with an empty hypothesized-node proposals
            // registry. The parent's proposals are not inherited: a
            // branch is a new analysis direction and the SME may
            // propose different novel capabilities. Lineage of the
            // proposals themselves is captured in `lineage` /
            // `parent_emitted_package_path`.
            proposals: std::collections::BTreeMap::new(),
            // Flexible plotting upgrade plan branches start
            // with an empty fallback counter; the parent's runtime
            // telemetry is not inherited. `#[serde(skip)]` on the field
            // means this is always the default regardless, but we set
            // it explicitly for symmetry with the other tracked fields.
            affordance_fallback_counter:
                ecaa_workflow_core::plot_affordance::AffordanceFallbackCounter::default(),
            // v3 P8 — branches start with an empty adjudication queue.
            adjudication_queue: Vec::new(),
            // Atom-safety-policy branches do not inherit
            // the parent's `atom_runtime_overrides`. A branch is a
            // new analysis direction and the parent's widened set is
            // not implicitly carried over; the SME re-approves
            // packages on the branch if the same blocker fires there.
            atom_runtime_overrides: std::collections::BTreeMap::new(),
            // branches start fresh; the parent's followup streak does
            // not carry over.
            intake_followup_streak: 0,
            // Branched sessions get a fresh HMAC secret; do not inherit
            // the parent's so each session-tree branch has its own audit
            // chain that can be independently verified.
            audit_writer_secret: {
                let mut secret = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut secret);
                secret
            },
            // None until the child session emits its own package.
            last_emitted_run_id: None,
        }
    }
}

#[cfg(test)]
mod branch_from_exhaustiveness {
    //! F5 Gate A — compile-time exhaustiveness on `Session::branch_from`.
    //!
    //! When a new field lands on `Session`, the author must answer the
    //! "does the branched child inherit, or reset?" question for that
    //! field. Without a gate, missed fields silently default to whatever
    //! `Self {.. }` syntax fills (which `branch_from` doesn't use — but
    //! a future refactor could). This test pins the answer to the
    //! current 49 fields: every field is named explicitly. Adding a new
    //! `Session` field forces the test to fail-to-compile until the test
    //! and `branch_from` agree on the inheritance decision.
    //!
    //! Mechanism: build a `Session::new(false)`, then call
    //! `Session::branch_from(&parent, false)`, then destructure the
    //! result with every field named. `..` is intentionally absent — a
    //! new field on `Session` makes this pattern non-exhaustive and the
    //! compiler errors with `missing field `<new_field>` in pattern`.
    //!
    //! The test asserts nothing about field VALUES (those are covered
    //! by the existing branch_from behavioural tests); it only asserts
    //! that every field is named, forcing a code-review touchpoint on
    //! every new `Session` field.
    use super::*;

    #[test]
    fn every_session_field_is_named_in_branch_from() {
        let parent = Session::new(false);
        let child = Session::branch_from(&parent, false);
        // Exhaustive destructure: no `..` rest pattern. Adding a new
        // Session field WITHOUT updating this test will produce
        // `missing field <field> in pattern` at compile time.
        let Session {
            schema_version: _,
            composer_version: _,
            pilot_recommendation: _,
            id: _,
            created_at: _,
            last_activity: _,
            state: _,
            conversation: _,
            intake_prose: _,
            intake_methods: _,
            excluded_atoms: _,
            sme_method_signals: _,
            classification: _,
            taxonomy: _,
            workflow_intent: _,
            project_class: _,
            mode: _,
            mode_locked: _,
            checkpoint_mode: _,
            dag: _,
            task_states: _,
            // Mirrors the prior user_confirmed bool gate.
            confirmation_token: _,
            pending_emission_id: _,
            emitted_package_path: _,
            harness_events: _,
            tool_call_log: _,
            decisions: _,
            careful_mode: _,
            blocked_opus_escalation_consumed: _,
            lineage: _,
            title: _,
            budget_usd: _,
            budget_set_by: _,
            budget_set_at: _,
            share_tokens: _,
            inputs: _,
            pending_input_hints: _,
            owner_user: _,
            pending_amendment: _,
            pending_disambiguation: _,
            deferred_state_triggers: _,
            archetype_snapshot: _,
            workflow_dag: _,
            compose_outcome: _,
            ranked_alternatives: _,
            policy_decisions: _,
            active_policy_bundle: _,
            renderer_proposals: _,
            proposals: _,
            affordance_fallback_counter: _,
            adjudication_queue: _,
            atom_runtime_overrides: _,
            intake_followup_streak: _,
            last_emitted_run_id: _,
            audit_writer_secret: _,
        } = child;
    }
}
